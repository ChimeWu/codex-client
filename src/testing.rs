//! 可脚本化的 fake App Server，用来测试建立在本客户端之上的应用。
//!
//! 它在临时 Unix socket 上跑**完整的**握手与 JSON-RPC 信封，因此被测代码走的
//! 是真实的传输、解析和订阅路径——只有对面的 agent 是假的。没有真实 daemon、
//! 不消耗额度，也不需要管理员预先起进程。
//!
//! 脚本按 **turn 的输入文本**匹配（[`FakeServer::on_input`]），不是按到达顺序。
//! 编队同时跑多条 thread 时到达顺序不确定，按文本匹配才能写出确定性的断言。
//!
//! ```no_run
//! use codex_client_cw::testing::{Beat, FakeServer};
//! use codex_client_cw::types::TurnStatus;
//!
//! # async fn demo() -> anyhow::Result<()> {
//! let server = FakeServer::start().await?;
//! server.on_input("查一下", [Beat::Delta("查完了".into()), Beat::Complete(TurnStatus::Completed)]);
//! let client = server.connect();
//! # let _ = client;
//! # Ok(())
//! # }
//! ```

use crate::client::AppServerClient;
use crate::generated::types::TurnStatus;
use anyhow::{Context, Result};
use futures_util::SinkExt;
use futures_util::stream::{SplitSink, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{WebSocketStream, accept_async};

type Writer = SplitSink<WebSocketStream<UnixStream>, Message>;

/// 脚本里的一拍。一个 turn 就是一串拍子。
#[derive(Clone, Debug)]
pub enum Beat {
    /// 推一段助手回复增量。
    Delta(String),
    /// 同一轮里属于**另一条**消息的增量。用来验证两条回复不被接成一段。
    DeltaOf { item: String, delta: String },
    /// 推一条与文本无关的通知，用来验证进度处理。
    Notice,
    /// 发起一次命令审批**并阻塞等待**客户端答复，就像真实服务端那样。
    ///
    /// `cwd` 是审批闸要审的东西，所以由脚本指定而不是写死。
    Approval { command: String, cwd: String },
    /// 停一会儿。中断在这里生效，因此想测 `/stop` 就得留出这一拍。
    Pause(Duration),
    /// 结束这个 turn。脚本没有以它收尾时会自动补一个 `Completed`。
    Complete(TurnStatus),
    /// 服务端侧把 turn 记成结束了，但**不发完成通知**，脚本继续往下走。
    ///
    /// 模拟推送丢失：只有轮询才能发现这一轮其实已经跑完。
    Silence(TurnStatus),
    /// 服务端主动断开连接。
    Disconnect,
}

/// 一个跑在临时 Unix socket 上的假 App Server。
///
/// 丢弃它会关掉监听并删除 socket 文件。
pub struct FakeServer {
    path: String,
    endpoint: String,
    shared: Arc<Shared>,
    accept: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct Recorded {
    /// 每次 `turn/start` 的输入文本。
    turns: Vec<String>,
    /// 客户端对审批请求给出的响应值。
    approvals: Vec<Value>,
    /// 每次 `turn/steer` 的文本。
    steers: Vec<String>,
    /// 被 `turn/interrupt` 打断过的 turn。
    interrupted: Vec<String>,
}

struct Shared {
    scripts: Mutex<Vec<(String, Vec<Beat>)>>,
    fallback: Mutex<Vec<Beat>>,
    recorded: Mutex<Recorded>,
    /// 等待客户端答复的服务端请求。
    awaiting: Mutex<HashMap<i64, oneshot::Sender<Value>>>,
    /// 每个在跑的 turn 一个中断信号。
    interrupts: Mutex<HashMap<String, Arc<Notify>>>,
    /// 服务端自己记的 turn 状态，`thread/read` 的答案来自这里。
    turns: Mutex<HashMap<String, Vec<(String, TurnStatus)>>>,
    /// 打开之后 `thread/resume` 一律拒绝，用来制造真正断掉的 thread。
    reject_resume: Mutex<bool>,
    next_id: AtomicU64,
}

impl Shared {
    fn next(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// 首个包含 needle 的脚本优先；都不匹配就用兜底脚本。
    fn script_for(&self, input: &str) -> Vec<Beat> {
        let scripts = self.scripts.lock().expect("脚本表未中毒");
        for (needle, beats) in scripts.iter() {
            if input.contains(needle.as_str()) {
                return beats.clone();
            }
        }
        drop(scripts);
        self.fallback.lock().expect("兜底脚本未中毒").clone()
    }
}

impl FakeServer {
    /// 绑定一个临时 socket 并开始接受连接。
    pub async fn start() -> Result<Self> {
        // 进程内计数器不能省：同一进程里并行起的多个 fake 会落在同一个纳秒上。
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = format!(
            "/tmp/codex-fake-app-server-{}-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("时钟不早于 UNIX 纪元")
                .as_nanos(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let listener = UnixListener::bind(&path).context("绑定 fake app server socket")?;
        let shared = Arc::new(Shared {
            scripts: Mutex::new(Vec::new()),
            fallback: Mutex::new(vec![Beat::Complete(TurnStatus::Completed)]),
            recorded: Mutex::new(Recorded::default()),
            awaiting: Mutex::new(HashMap::new()),
            interrupts: Mutex::new(HashMap::new()),
            turns: Mutex::new(HashMap::new()),
            reject_resume: Mutex::new(false),
            next_id: AtomicU64::new(1),
        });
        let accept = tokio::spawn(accept_loop(listener, shared.clone()));
        Ok(Self {
            endpoint: format!("unix://{path}"),
            path,
            shared,
            accept,
        })
    }

    /// `unix://…` 形式的端点，可直接交给 [`AppServerClient::builder`]。
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// 连上这个 fake server 的客户端。
    pub fn connect(&self) -> Arc<AppServerClient> {
        AppServerClient::builder(self.endpoint.clone()).build()
    }

    /// turn 的输入文本包含 `needle` 时按这串拍子演。先登记的先匹配。
    pub fn on_input(&self, needle: impl Into<String>, beats: impl IntoIterator<Item = Beat>) {
        self.shared
            .scripts
            .lock()
            .expect("脚本表未中毒")
            .push((needle.into(), beats.into_iter().collect()));
    }

    /// 此后拒绝一切 `thread/resume`：断线之后客户端恢复不回来，thread 真的没了。
    pub fn reject_resume(&self) {
        *self.shared.reject_resume.lock().expect("恢复开关未中毒") = true;
    }

    /// 没有脚本匹配时的兜底，默认是「立刻成功完成」。
    pub fn otherwise(&self, beats: impl IntoIterator<Item = Beat>) {
        *self.shared.fallback.lock().expect("兜底脚本未中毒") = beats.into_iter().collect();
    }

    /// 到目前为止每次 `turn/start` 的输入文本，按到达顺序。
    pub fn started_turns(&self) -> Vec<String> {
        self.shared
            .recorded
            .lock()
            .expect("记录未中毒")
            .turns
            .clone()
    }

    /// 客户端对审批请求给出的响应值，按到达顺序。
    pub fn approvals(&self) -> Vec<Value> {
        self.shared
            .recorded
            .lock()
            .expect("记录未中毒")
            .approvals
            .clone()
    }

    /// 客户端发过的 steer 文本。
    pub fn steers(&self) -> Vec<String> {
        self.shared
            .recorded
            .lock()
            .expect("记录未中毒")
            .steers
            .clone()
    }

    /// 被中断过的 turn id。
    pub fn interrupted(&self) -> Vec<String> {
        self.shared
            .recorded
            .lock()
            .expect("记录未中毒")
            .interrupted
            .clone()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.accept.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn accept_loop(listener: UnixListener, shared: Arc<Shared>) {
    while let Ok((stream, _)) = listener.accept().await {
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Ok(ws) = accept_async(stream).await {
                serve(ws, shared).await;
            }
        });
    }
}

async fn serve(ws: WebSocketStream<UnixStream>, shared: Arc<Shared>) {
    let (writer, mut reader) = ws.split();
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    while let Some(Ok(message)) = reader.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        // 没有 method 的带 id 消息是**客户端对服务端请求的回复**。
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            deliver_reply(&shared, &value);
            continue;
        };
        handle_client_message(method.to_owned(), value, &writer, &shared).await;
    }
}

fn deliver_reply(shared: &Arc<Shared>, value: &Value) {
    let Some(id) = value.get("id").and_then(Value::as_i64) else {
        return;
    };
    let waiting = shared.awaiting.lock().expect("等待表未中毒").remove(&id);
    if let Some(waiting) = waiting {
        let response = value
            .get("result")
            .cloned()
            .unwrap_or_else(|| value.get("error").cloned().unwrap_or(Value::Null));
        shared
            .recorded
            .lock()
            .expect("记录未中毒")
            .approvals
            .push(response.clone());
        let _ = waiting.send(response);
    }
}

async fn handle_client_message(
    method: String,
    value: Value,
    writer: &Arc<tokio::sync::Mutex<Writer>>,
    shared: &Arc<Shared>,
) {
    let id = value.get("id").cloned();
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    match method.as_str() {
        "initialize" => {
            reply(
                writer,
                id,
                json!({
                    "userAgent": "fake-app-server",
                    "codexHome": "/tmp/.codex",
                    "platformFamily": "unix",
                    "platformOs": "macos"
                }),
            )
            .await;
        }
        // 通知，没有 id，不必回复。
        "initialized" => {}
        "thread/resume" if *shared.reject_resume.lock().expect("恢复开关未中毒") => {
            if let Some(id) = id {
                send(
                    writer,
                    json!({"id": id, "error": {"code": -32000, "message": "thread 不可恢复"}}),
                )
                .await;
            }
        }
        "thread/read" => {
            let thread_id = params
                .get("threadId")
                .and_then(Value::as_str)
                .unwrap_or("thread-0")
                .to_owned();
            let turns: Vec<Value> = shared
                .turns
                .lock()
                .expect("turn 记录未中毒")
                .get(&thread_id)
                .map(|turns| {
                    turns
                        .iter()
                        .map(|(turn_id, status)| {
                            json!({"id": turn_id, "items": [],
                                "status": serde_json::to_value(status).expect("状态可序列化")})
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut thread = thread_result(&thread_id, "/tmp");
            thread["thread"]["turns"] = Value::Array(turns);
            reply(writer, id, json!({"thread": thread["thread"]})).await;
        }
        "thread/start" | "thread/resume" => {
            let cwd = params
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("/tmp")
                .to_owned();
            let thread_id = params
                .get("threadId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("thread-{}", shared.next()));
            reply(writer, id, thread_result(&thread_id, &cwd)).await;
        }
        "turn/start" => {
            let thread_id = params
                .get("threadId")
                .and_then(Value::as_str)
                .unwrap_or("thread-0")
                .to_owned();
            let turn_id = format!("turn-{}", shared.next());
            let input = input_text(&params);
            shared
                .recorded
                .lock()
                .expect("记录未中毒")
                .turns
                .push(input.clone());
            shared
                .turns
                .lock()
                .expect("turn 记录未中毒")
                .entry(thread_id.clone())
                .or_default()
                .push((turn_id.clone(), TurnStatus::InProgress));
            let interrupt = Arc::new(Notify::new());
            shared
                .interrupts
                .lock()
                .expect("中断表未中毒")
                .insert(turn_id.clone(), interrupt.clone());
            reply(
                writer,
                id,
                json!({"turn": {"id": turn_id, "items": [], "status": "inProgress"}}),
            )
            .await;
            let beats = shared.script_for(&input);
            tokio::spawn(perform(
                beats,
                thread_id,
                turn_id,
                interrupt,
                writer.clone(),
                shared.clone(),
            ));
        }
        "turn/interrupt" => {
            if let Some(turn_id) = params.get("turnId").and_then(Value::as_str) {
                let notify = shared
                    .interrupts
                    .lock()
                    .expect("中断表未中毒")
                    .get(turn_id)
                    .cloned();
                if let Some(notify) = notify {
                    shared
                        .recorded
                        .lock()
                        .expect("记录未中毒")
                        .interrupted
                        .push(turn_id.to_owned());
                    notify.notify_waiters();
                }
            }
            reply(writer, id, json!({})).await;
        }
        "turn/steer" => {
            shared
                .recorded
                .lock()
                .expect("记录未中毒")
                .steers
                .push(input_text(&params));
            reply(writer, id, json!({})).await;
        }
        "thread/unsubscribe" => reply(writer, id, json!({})).await,
        _ => {
            // 诚实地说不认识，而不是拿一个空对象冒充任意响应形状。
            if let Some(id) = id {
                send(
                    writer,
                    json!({"id": id, "error": {"code": -32601, "message": format!("fake server 未实现 {method}")}}),
                )
                .await;
            }
        }
    }
}

/// 按脚本演完一个 turn，中断随时可以打断它。
async fn perform(
    beats: Vec<Beat>,
    thread_id: String,
    turn_id: String,
    interrupt: Arc<Notify>,
    writer: Arc<tokio::sync::Mutex<Writer>>,
    shared: Arc<Shared>,
) {
    let mut closed = false;
    for beat in beats {
        match beat {
            Beat::Delta(delta) => {
                send(
                    &writer,
                    json!({"method": "item/agentMessage/delta", "params": {
                        "delta": delta, "itemId": "item-1",
                        "threadId": thread_id, "turnId": turn_id}}),
                )
                .await;
            }
            Beat::DeltaOf { item, delta } => {
                send(
                    &writer,
                    json!({"method": "item/agentMessage/delta", "params": {
                        "delta": delta, "itemId": item,
                        "threadId": thread_id, "turnId": turn_id}}),
                )
                .await;
            }
            Beat::Notice => {
                send(
                    &writer,
                    json!({"method": "item/started", "params": {
                        "threadId": thread_id, "turnId": turn_id,
                        "item": {"id": "item-1", "itemType": "reasoning", "text": ""}}}),
                )
                .await;
            }
            Beat::Approval { command, cwd } => {
                let request_id = shared.next() as i64;
                let (tx, rx) = oneshot::channel();
                shared
                    .awaiting
                    .lock()
                    .expect("等待表未中毒")
                    .insert(request_id, tx);
                send(
                    &writer,
                    json!({"id": request_id, "method": "item/commandExecution/requestApproval",
                        "params": {"itemId": "item-1", "threadId": thread_id, "turnId": turn_id,
                            "startedAtMs": 0, "command": command, "cwd": cwd}}),
                )
                .await;
                // 真实服务端在这里阻塞；中断仍然要能把 turn 拉回来。
                tokio::select! {
                    _ = rx => {}
                    _ = interrupt.notified() => {
                        record(&shared, &thread_id, &turn_id, &TurnStatus::Interrupted);
                        complete(&writer, &thread_id, &turn_id, TurnStatus::Interrupted).await;
                        closed = true;
                        break;
                    }
                }
            }
            Beat::Pause(duration) => {
                tokio::select! {
                    _ = tokio::time::sleep(duration) => {}
                    _ = interrupt.notified() => {
                        record(&shared, &thread_id, &turn_id, &TurnStatus::Interrupted);
                        complete(&writer, &thread_id, &turn_id, TurnStatus::Interrupted).await;
                        closed = true;
                        break;
                    }
                }
            }
            Beat::Complete(status) => {
                record(&shared, &thread_id, &turn_id, &status);
                complete(&writer, &thread_id, &turn_id, status).await;
                closed = true;
                break;
            }
            // 服务端知道跑完了，客户端不知道——轮询兜底要挡的就是这一格。
            // 不 break：后面还可以接 `Disconnect`，把「通知丢了」和「连接也没了」
            // 叠在一起。
            Beat::Silence(status) => {
                record(&shared, &thread_id, &turn_id, &status);
                closed = true;
            }
            Beat::Disconnect => {
                let _ = writer.lock().await.close().await;
                closed = true;
                break;
            }
        }
    }
    if !closed {
        record(&shared, &thread_id, &turn_id, &TurnStatus::Completed);
        complete(&writer, &thread_id, &turn_id, TurnStatus::Completed).await;
    }
    shared
        .interrupts
        .lock()
        .expect("中断表未中毒")
        .remove(&turn_id);
}

/// 更新服务端自己记的 turn 状态。`thread/read` 的答案来自这里。
fn record(shared: &Arc<Shared>, thread_id: &str, turn_id: &str, status: &TurnStatus) {
    if let Some(turns) = shared
        .turns
        .lock()
        .expect("turn 记录未中毒")
        .get_mut(thread_id)
        && let Some(entry) = turns.iter_mut().find(|(id, _)| id == turn_id)
    {
        entry.1 = status.clone();
    }
}

async fn complete(
    writer: &Arc<tokio::sync::Mutex<Writer>>,
    thread_id: &str,
    turn_id: &str,
    status: TurnStatus,
) {
    let status = serde_json::to_value(status).expect("TurnStatus 可序列化");
    send(
        writer,
        json!({"method": "turn/completed", "params": {
            "threadId": thread_id,
            "turn": {"id": turn_id, "items": [], "status": status}}}),
    )
    .await;
}

fn input_text(params: &Value) -> String {
    params
        .get("input")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn thread_result(id: &str, cwd: &str) -> Value {
    json!({
        "thread": {
            "id": id, "sessionId": id, "cwd": cwd,
            "createdAt": 0, "updatedAt": 0, "recencyAt": 0,
            "cliVersion": "0.153.0", "modelProvider": "openai",
            "preview": "", "ephemeral": false, "historyMode": "paginated",
            "source": "vscode", "status": {"type": "idle"},
            "path": "/tmp/rollout.jsonl", "canAcceptDirectInput": true,
            "turns": []
        },
        "cwd": cwd,
        "model": "gpt-5.6-luna",
        "modelProvider": "openai",
        "sandbox": {"type": "workspaceWrite", "writableRoots": [cwd], "networkAccess": false},
        "approvalPolicy": "untrusted",
        "approvalsReviewer": "user",
        "instructionSources": []
    })
}

async fn reply(writer: &Arc<tokio::sync::Mutex<Writer>>, id: Option<Value>, result: Value) {
    if let Some(id) = id {
        send(writer, json!({"id": id, "result": result})).await;
    }
}

async fn send(writer: &Arc<tokio::sync::Mutex<Writer>>, value: Value) {
    let _ = writer
        .lock()
        .await
        .send(Message::Text(value.to_string().into()))
        .await;
}
