//! 独占连接的 actor：一条 WebSocket 上的全双工 JSON-RPC。
//!
//! 客户端句柄不碰连接，只往 [`Command`] 通道里投递意图；这个 actor 是唯一
//! 读写 socket 的地方。这样一条连接就能同时应付四件本来互相打架的事：等待
//! 若干个请求的响应、接住随时到来的通知、应答服务端反向发起的请求、以及在
//! 断线后重连——旧实现为此开了两条连接，还是只能同时跑一个 turn。
//!
//! # 两条频道，不是一条
//!
//! 入站消息按 thread 归属分流到两条性质不同的频道（[`crate::protocol`] 的
//! [`crate::protocol::split_notification`]/[`crate::protocol::split_request`]
//! 负责判定归属）：
//!
//! - **`routes`**：`threadId → Route`，一个 thread 一条独占频道，从
//!   `thread/start`、`thread/resume` 或补订阅那一刻挂上，直到调用方丢弃接收器
//!   才卸下——**跨 turn 存活**。同一时刻一个 thread 上只有一个未完成的
//!   turn（`turn/steer` 的 `expectedTurnId` precondition 与 `turn/start` 在已有
//!   活跃 turn 时退化成 steer 都印证了这一点），因此按 `threadId` 路由既够用
//!   也不会有多个 turn 抢一条频道的问题，不需要再叠加 `turnId`。
//! - **`events`**：有界 `mpsc::Sender`，装连接建立/断开与全局消息。服务端请求
//!   携带一次性回复通道，只能交给一个消费者，因此这里不能用广播。
//!
//! # 依据协议消息维护状态
//!
//! 所有 RPC 共用 Call 和 Pending；发送前按 method 提取上下文，响应到达后按 id
//! 找回 method，在结果交付之前更新订阅和 turn 状态。通用调用与便捷方法一致。
//! 本地消息出口独立登记，新 thread 的出口先按本地请求编号保存，响应中的 thread ID
//! 已知后再绑定。挂载出口不会制造订阅事实；取消等待也不抹掉服务端已完成的操作。
//!
//! 没有路由认领的 thread 消息（`handling_unrouted_requests` 打开时）与真正的
//! 全局消息一起走全局频道，但类型上仍然是 `Orphaned*` 变体而不是
//! `Global*`——它们本质上仍是"某个 thread 的消息"，只是这条连接上暂时没人听。
//!
//! # 断线
//!
//! 订阅是**连接级**的（ADR 0005），但 route 是客户端级的。连接断开只清掉
//! `pending` / `subscribed`；仍有消费者的 route 与 resume 参数保留。
//! 新连接握手后先逐一发 `thread/resume`，原事件接收端因此可以跨重连继续工作。
//!
//! 恢复响应若表明 turn 已在断线期间结束，actor 会补发一条 `turn/completed`，
//! 否则那个等结果的调用方会一直等下去。**只对正在等 turn 的 route 补**
//! （`Route::awaiting_turn`）：route 现在跨 turn 存活，对一条闲着的 route 补发
//! 完成通知等于凭空报告一个没人发起过的 turn 结束了。
//!
//! # 传输
//!
//! 建连与发帧就在本模块底部：一条 Unix socket 上的 WebSocket，只有这个
//! actor 会读写它，没有第二个调用方，也就不必单开一层。

use crate::client::inbound::{GlobalMessage, ReplySender, ServerReply, ThreadMessage};
use crate::envelope::{ErrorObject, Incoming, Outgoing, Reply};
use crate::error::{Error, Result};
use crate::generated::types::{
    InitializeResponse, RequestId, ThreadResumeParams, Turn, TurnCompletedNotification, TurnStatus,
};
use crate::protocol::{
    NotificationScope, RequestScope, ThreadNotification, decline_by_method, method_of,
    parse_notification, parse_server_request, reconnect_delay, sniff_thread_id, split_notification,
    split_request, tagged,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::client_async;
use tokio_tungstenite::tungstenite::Message;

/// 重连退避的上限。
const MAX_BACKOFF_MS: u64 = 30_000;
/// `initialize` 固定用这个 id：它先于任何别的请求发出。
const INITIALIZE_ID: u64 = 0;
const THREAD_START: &str = "thread/start";
const THREAD_RESUME: &str = "thread/resume";

/// 本地消息出口。代次防止旧响应改变新路由。
pub(crate) struct Attach {
    pub(crate) epoch: u64,
    pub(crate) events: mpsc::UnboundedSender<ThreadMessage>,
}

/// RPC 只有 Call；其余命令只操作本地消息出口或查询连接状态。
pub(crate) enum Command {
    Call {
        request_id: u64,
        method: String,
        params: Value,
        responder: oneshot::Sender<Result<Value>>,
    },
    /// 等指定请求返回 thread ID 后，绑定消息出口。
    RegisterRoute { request_id: u64, attach: Attach },
    IsSubscribed {
        thread_id: String,
        responder: oneshot::Sender<Result<bool>>,
    },
}

/// 统一的请求上下文，由 actor 在发送前根据 method 提取。
struct Pending {
    method: String,
    thread_id: Option<String>,
    resume: Option<Value>,
    epoch: Option<u64>,
    previous_awaiting: bool,
    previous_resume: Option<Value>,
    attach: Option<Attach>,
    responder: Option<oneshot::Sender<Result<Value>>>,
}

struct Route {
    epoch: u64,
    resume: Value,
    events: mpsc::UnboundedSender<ThreadMessage>,
    /// 有一个已发出、还没等到 `turn/completed` 的 turn。只有它为真时，重连
    /// 发现服务端那边已经没有活跃 turn 才值得补发一条完成通知。
    awaiting_turn: bool,
}

pub(crate) struct Actor {
    endpoint: String,
    initialize: Value,
    connect_timeout: Duration,
    /// 是否把没有 turn 认领的服务端请求交给调用方，见
    /// [`crate::AppServerClientBuilder::handling_unrouted_requests`]。
    handle_unrouted: bool,
    commands_rx: mpsc::Receiver<Command>,
    /// 与任何 thread 都无关的全局消息；服务端请求只能由一个消费者回复。
    events: mpsc::Sender<GlobalMessage>,

    // 以下是连接级状态，断线时清空；routes 是客户端级状态，跨重连保留。
    next_id: u64,
    pending: HashMap<u64, Pending>,
    /// 每个 thread 的消息出口，按 `threadId` 分，跨 turn 存活。
    routes: HashMap<String, Route>,
    subscribed: HashMap<String, Value>,
    registrations: HashMap<u64, Attach>,
}

impl Actor {
    pub(crate) fn new(
        endpoint: String,
        initialize: Value,
        connect_timeout: Duration,
        handle_unrouted: bool,
        commands_rx: mpsc::Receiver<Command>,
        events: mpsc::Sender<GlobalMessage>,
    ) -> Self {
        Self {
            endpoint,
            initialize,
            connect_timeout,
            handle_unrouted,
            commands_rx,
            events,
            next_id: 1,
            pending: HashMap::new(),
            routes: HashMap::new(),
            subscribed: HashMap::new(),
            registrations: HashMap::new(),
        }
    }

    /// 连接 → 服务 → 断线 → 退避 → 再连，直到最后一份客户端 Arc 被丢弃。
    pub(crate) async fn run(mut self) {
        let mut attempt = 0u32;
        loop {
            match self.dial().await {
                Ok((ws, response)) => {
                    attempt = 0;
                    self.on_connected(response);
                    let mut ws = ws;
                    let reconnect = match self.restore_routes(&mut ws).await {
                        Ok(()) => self.serve(ws).await,
                        Err(error) => {
                            tracing::warn!(%error, "恢复 thread 订阅时连接中断");
                            true
                        }
                    };
                    self.reset(&Error::Closed {
                        method: "(连接断开)".to_owned(),
                    });
                    self.emit_global(GlobalMessage::Disconnected);
                    if !reconnect {
                        break;
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, endpoint = %self.endpoint, "连接 App Server 失败");
                    attempt = attempt.saturating_add(1);
                    if !self
                        .idle(&error, reconnect_delay(attempt, MAX_BACKOFF_MS))
                        .await
                    {
                        break;
                    }
                }
            }
        }
        self.reset(&Error::Closed {
            method: "(客户端已关闭)".to_owned(),
        });
    }

    /// 建连并完成 `initialize` / `initialized` 握手。
    async fn dial(&mut self) -> Result<(WebSocket, InitializeResponse)> {
        let mut ws = connect(&self.endpoint, self.connect_timeout).await?;
        send(
            &mut ws,
            &Outgoing::Request {
                id: INITIALIZE_ID,
                method: "initialize",
                params: &self.initialize,
            },
        )
        .await?;
        let response = tokio::time::timeout(self.connect_timeout, await_initialize(&mut ws))
            .await
            .map_err(|_| Error::Timeout {
                method: "initialize".to_owned(),
                after: self.connect_timeout,
            })??;
        send(
            &mut ws,
            &Outgoing::Notification {
                method: "initialized",
                params: Value::Object(serde_json::Map::new()),
            },
        )
        .await?;
        Ok((ws, response))
    }

    fn on_connected(&mut self, response: InitializeResponse) {
        self.next_id = 1;
        self.emit_global(GlobalMessage::Connected(Box::new(response)));
    }

    /// 新连接握手成功后，恢复所有仍有消费者的 thread 订阅。
    async fn restore_routes(&mut self, ws: &mut WebSocket) -> Result<()> {
        self.routes.retain(|_, route| !route.events.is_closed());
        let routes = self
            .routes
            .iter()
            .map(|(thread_id, route)| (thread_id.clone(), route.resume.clone()))
            .collect::<Vec<_>>();
        for (_, resume) in routes {
            self.call(ws, THREAD_RESUME.to_owned(), resume, None, None)
                .await?;
        }
        Ok(())
    }

    /// 主循环。返回 `true` 表示断线该重连，`false` 表示客户端没了、该收摊。
    async fn serve(&mut self, mut ws: WebSocket) -> bool {
        // 每条连接一只回复邮箱。ReplySender 直接向这里投递一次结果；连接
        // 结束、receiver 被丢弃后，旧连接的所有 reply 都会自然失效。
        let (replies_tx, mut replies_rx) = mpsc::unbounded_channel::<ServerReply>();
        loop {
            tokio::select! {
                reply = replies_rx.recv() => {
                    let Some(ServerReply { id, method, value }) = reply else { continue };
                    let reply = value.map_or_else(|| decline_by_method(method), Reply::ok);
                    if send(&mut ws, &reply.as_outgoing(&id)).await.is_err() {
                        return true;
                    }
                }
                command = self.commands_rx.recv() => {
                    let Some(command) = command else { return false };
                    if self.dispatch(&mut ws, command).await.is_err() {
                        return true;
                    }
                }
                message = ws.next() => {
                    match message {
                        Some(Ok(message)) => {
                            if self.on_message(&mut ws, message, &replies_tx).await.is_err() {
                                return true;
                            }
                        }
                        Some(Err(error)) => {
                            tracing::warn!(%error, "App Server 连接中断");
                            return true;
                        }
                        None => return true,
                    }
                }
            }
        }
    }

    /// 退避期间：命令一律立刻失败，别让调用方空等一个连不上的服务端。
    ///
    /// 返回 `false` 表示该退出。
    async fn idle(&mut self, error: &Error, delay: Duration) -> bool {
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return true,
                command = self.commands_rx.recv() => match command {
                    Some(command) => self.fail(command, error),
                    None => return false,
                },
            }
        }
    }

    async fn dispatch(&mut self, ws: &mut WebSocket, command: Command) -> Result<()> {
        match command {
            Command::Call {
                request_id,
                method,
                params,
                responder,
            } => {
                let attach = self.registrations.remove(&request_id);
                self.call(ws, method, params, Some(responder), attach).await
            }
            local => {
                self.local(local);
                Ok(())
            }
        }
    }

    /// 本地命令不发送 RPC，也不宣称服务端订阅已经建立。
    fn local(&mut self, command: Command) {
        match command {
            Command::RegisterRoute { request_id, attach } => {
                self.registrations
                    .retain(|_, attach| !attach.events.is_closed());
                self.registrations.insert(request_id, attach);
            }
            Command::IsSubscribed {
                thread_id,
                responder,
            } => {
                let _ = responder.send(Ok(self.subscribed.contains_key(&thread_id)));
            }
            Command::Call { .. } => unreachable!("RPC 由连接处理"),
        }
    }

    /// 所有请求（包括重连恢复）都经过同一发送路径。
    async fn call(
        &mut self,
        ws: &mut WebSocket,
        method: String,
        params: Value,
        responder: Option<oneshot::Sender<Result<Value>>>,
        attach: Option<Attach>,
    ) -> Result<()> {
        let mut pending = Pending {
            method,
            thread_id: None,
            resume: None,
            epoch: None,
            previous_awaiting: false,
            previous_resume: None,
            attach,
            responder,
        };
        if let Err(error) = self.before_request(&params, &mut pending) {
            if let Some(responder) = pending.responder {
                let _ = responder.send(Err(error));
                return Ok(());
            }
            return Err(error);
        }
        let id = self.take_id();
        let method = pending.method.clone();
        self.pending.insert(id, pending);
        send(
            ws,
            &Outgoing::Request {
                id,
                method: &method,
                params: &params,
            },
        )
        .await
    }

    /// 根据实际发送的方法识别状态变化；普通请求不保留参数副本。
    fn before_request(&mut self, params: &Value, pending: &mut Pending) -> Result<()> {
        match pending.method.as_str() {
            THREAD_START | "thread/fork" => {}
            THREAD_RESUME | "thread/unsubscribe" | "turn/start" | "review/start" => {
                let thread = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Protocol(format!("{} 缺少 threadId", pending.method)))?;
                pending.thread_id = Some(thread.to_owned());
                pending.epoch = self.routes.get(thread).map(|route| route.epoch);
                if pending.method == THREAD_RESUME {
                    pending.resume = Some(params.clone());
                } else if pending.method == "turn/start" || pending.method == "review/start" {
                    // detached review 的结果属于新 thread，不给原 thread 记 turn 状态。
                    if pending.method == "review/start"
                        && params.get("delivery").and_then(Value::as_str) == Some("detached")
                    {
                        return Ok(());
                    }
                    let Some(route) = self
                        .routes
                        .get_mut(thread)
                        .filter(|route| !route.events.is_closed())
                    else {
                        return Err(Error::Closed {
                            method: pending.method.clone(),
                        });
                    };
                    if !self.subscribed.contains_key(thread) {
                        return Err(Error::Protocol(format!("thread {thread} 尚未完成订阅")));
                    }
                    pending.previous_awaiting = route.awaiting_turn;
                    route.awaiting_turn = true;
                    // 保存已提供的覆盖项，未提供字段保留原恢复配置。
                    let mut resume = route.resume.clone();
                    for field in ["approvalsReviewer", "approvalPolicy"] {
                        if let Some(value) = params.get(field).filter(|value| !value.is_null()) {
                            resume[field] = value.clone();
                        }
                    }
                    pending.previous_resume =
                        Some(std::mem::replace(&mut route.resume, resume.clone()));
                    pending.resume = Some(resume);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// 只绑定本地出口，不改变服务端订阅事实。
    fn mount(&mut self, thread_id: String, resume: Value, attach: Attach) {
        if attach.events.is_closed()
            || self
                .routes
                .get(&thread_id)
                .is_some_and(|route| route.epoch > attach.epoch)
        {
            return;
        }
        let awaiting_turn = self
            .routes
            .get(&thread_id)
            .is_some_and(|route| route.awaiting_turn);
        self.routes.insert(
            thread_id,
            Route {
                epoch: attach.epoch,
                resume,
                events: attach.events,
                awaiting_turn,
            },
        );
    }

    async fn on_message(
        &mut self,
        ws: &mut WebSocket,
        message: Message,
        replies: &mpsc::UnboundedSender<ServerReply>,
    ) -> Result<()> {
        match message {
            Message::Text(text) => {
                match serde_json::from_str::<Incoming>(&text) {
                    Ok(Incoming::Response { id, result }) => self.on_response(&id, Ok(result)),
                    Ok(Incoming::Error { id, error }) => self.on_response(&id, Err(error)),
                    Ok(Incoming::Request { id, method, params }) => {
                        self.on_server_request(ws, id, &method, params, replies)
                            .await?;
                    }
                    Ok(Incoming::Notification { method, params }) => {
                        self.on_notification(&method, params);
                    }
                    // 四种形态都对不上：原样交出去，绝不让一条读不懂的消息
                    // 拖垮连接。
                    Err(error) => {
                        tracing::debug!(%error, "读不懂的 App Server 消息");
                        let raw = serde_json::from_str::<Value>(&text)
                            .unwrap_or_else(|_| Value::String(text.to_string()));
                        self.emit_global(GlobalMessage::Unparsed(raw));
                    }
                }
                Ok(())
            }
            Message::Ping(payload) => ws
                .send(Message::Pong(payload))
                .await
                .map_err(Error::transport),
            Message::Close(_) => Err(Error::Closed {
                method: "(服务端关闭连接)".to_owned(),
            }),
            _ => Ok(()),
        }
    }

    /// 我们发出的某个请求有结果了：成功的 `result` 或服务端的 `error`。
    fn on_response(&mut self, id: &RequestId, outcome: std::result::Result<Value, ErrorObject>) {
        // 我们只用整数 id 发请求，因此文本 id 的响应必然不是我们的。
        let RequestId::Integer(id) = id else {
            tracing::debug!("响应带着一个我们没用过的文本 id，忽略");
            return;
        };
        let Ok(id) = u64::try_from(*id) else {
            return;
        };
        let Some(mut pending) = self.pending.remove(&id) else {
            tracing::debug!(id, "响应没有对应的请求，忽略");
            return;
        };
        let result = outcome.map_err(|error| Error::rejected(&pending.method, error));
        let result = match result {
            Ok(value) => self.after_response(&mut pending, &value).map(|()| value),
            Err(error) => Err(error),
        };
        if result.is_err() {
            self.failed_response(&pending);
        }
        if let Some(responder) = pending.responder {
            let _ = responder.send(result);
        }
    }

    /// 响应副作用在交付调用方之前、在回答请求的连接上完成。
    fn after_response(&mut self, pending: &mut Pending, value: &Value) -> Result<()> {
        match pending.method.as_str() {
            THREAD_START | "thread/fork" => {
                let thread = value
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        Error::Protocol(format!("{} 响应缺少 thread.id", pending.method))
                    })?;
                let mut resume = resume_params(thread);
                for field in ["approvalsReviewer", "approvalPolicy"] {
                    if let Some(setting) = value.get(field) {
                        resume[field] = setting.clone();
                    }
                }
                self.subscribed.insert(thread.to_owned(), resume.clone());
                if let Some(attach) = pending.attach.take() {
                    self.mount(thread.to_owned(), resume, attach);
                }
            }
            THREAD_RESUME => {
                let thread = pending.thread_id.as_ref().expect("发送前已提取 threadId");
                let resume = pending
                    .resume
                    .as_ref()
                    .expect("发送前已保存恢复参数")
                    .clone();
                let completion =
                    if self.routes.get(thread).is_some_and(|route| {
                        Some(route.epoch) == pending.epoch && route.awaiting_turn
                    }) {
                        let turns = value.pointer("/thread/turns").ok_or_else(|| {
                            Error::Protocol("thread/resume 响应缺少 thread.turns".to_owned())
                        })?;
                        Some(
                            Vec::<Turn>::deserialize(turns).map_err(|source| Error::Decode {
                                method: THREAD_RESUME.to_owned(),
                                source,
                            })?,
                        )
                    } else {
                        None
                    };
                self.subscribed.insert(thread.clone(), resume.clone());
                if let Some(route) = self.routes.get_mut(thread) {
                    route.resume = resume.clone();
                }
                if let Some(attach) = pending.attach.take() {
                    self.mount(thread.clone(), resume, attach);
                }
                if let Some(turns) = completion {
                    self.recover_completion(thread, turns);
                }
            }
            "thread/unsubscribe" => {
                let thread = pending.thread_id.as_ref().expect("发送前已提取 threadId");
                self.subscribed.remove(thread);
                self.routes.remove(thread);
            }
            "turn/start" | "review/start" => {
                if let (Some(thread), Some(resume)) = (&pending.thread_id, &pending.resume) {
                    if let Some(route) = self
                        .routes
                        .get_mut(thread)
                        .filter(|route| Some(route.epoch) == pending.epoch)
                    {
                        route.resume = resume.clone();
                    }
                    if let Some(saved) = self.subscribed.get_mut(thread) {
                        *saved = resume.clone();
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn failed_response(&mut self, pending: &Pending) {
        let Some(thread) = &pending.thread_id else {
            return;
        };
        if !self
            .routes
            .get(thread)
            .is_some_and(|route| Some(route.epoch) == pending.epoch)
        {
            return;
        }
        if pending.method == THREAD_RESUME && !self.subscribed.contains_key(thread) {
            self.subscribed.remove(thread);
            self.routes.remove(thread);
        } else if matches!(pending.method.as_str(), "turn/start" | "review/start")
            && pending.resume.is_some()
            && let Some(route) = self.routes.get_mut(thread)
        {
            route.awaiting_turn = pending.previous_awaiting;
            if let Some(resume) = &pending.previous_resume {
                route.resume = resume.clone();
            }
        }
    }

    /// 断线期间结束的 turn 不会再推完成通知，这里替它补一条。
    ///
    /// 只补给**正在等一个 turn** 的路由：路由跨 turn 存活，对一条闲着的路由补
    /// 发完成通知等于凭空报告一个没人发起过的 turn 结束了。
    fn recover_completion(&mut self, thread_id: &str, turns: Vec<Turn>) {
        let Some(route) = self.routes.get_mut(thread_id) else {
            return;
        };
        if !route.awaiting_turn {
            return;
        }
        if turns
            .iter()
            .any(|turn| turn.status == TurnStatus::InProgress)
        {
            return;
        }
        let Some(turn) = turns.into_iter().max_by_key(|turn| turn.started_at) else {
            return;
        };
        route.awaiting_turn = false;
        self.push(
            thread_id,
            ThreadMessage::Notification(Box::new(ThreadNotification::TurnCompleted(
                TurnCompletedNotification {
                    thread_id: thread_id.to_owned(),
                    turn,
                },
            ))),
        );
    }

    /// 服务端反向发起的请求。**一定**要应答，否则那个 turn 会悬到超时。
    async fn on_server_request(
        &mut self,
        ws: &mut WebSocket,
        id: RequestId,
        method: &str,
        params: Value,
        replies: &mpsc::UnboundedSender<ServerReply>,
    ) -> Result<()> {
        let Some(request) = parse_server_request(&tagged(method, params)) else {
            // 读不懂的请求仍然必须应答，只是没法交给调用方决定。它不会作为
            // `Unparsed` 交出去——那条流是尽力交付的，而这条请求有人在等。
            tracing::warn!(%method, "自动拒绝一个无法解析的 App Server 请求");
            send(ws, &decline_by_method(method).as_outgoing(&id)).await?;
            return Ok(());
        };
        let method = method_of(&request);
        let scope = split_request(request);

        // 服务端在**阻塞等待**这个应答，因此它绝不能走全局流那条尽力交付的
        // 路：没人消费就等于一直悬着。没有 turn 认领时默认当场拒绝，除非调用方
        // 明确表示要自己接管。genuinely 全局的请求同样受这条开关约束——它本来
        // 就不可能有任何 route 认领。
        let claimed = matches!(&scope,
            RequestScope::Thread { thread_id, .. } if self.routes.contains_key(thread_id)
        );
        if !claimed && !self.handle_unrouted {
            tracing::warn!(%method, "没有 turn 认领这个 App Server 请求，自动拒绝");
            send(ws, &decline_by_method(method).as_outgoing(&id)).await?;
            return Ok(());
        }
        let reply = ReplySender::new(id.clone(), method, replies.clone());
        match scope {
            RequestScope::Thread { thread_id, request } => match self.routes.get(&thread_id) {
                Some(route) => {
                    let message = ThreadMessage::Request {
                        id: id.clone(),
                        method,
                        request: Box::new(request),
                        reply,
                    };
                    if route.events.send(message).is_err() {
                        // 出口已经没人听了。这是「调用方消失」最主要的场景，
                        // SendError 会丢弃消息，其中的 ReplySender 随即投递拒绝。
                        tracing::debug!(%method, "turn 的消息出口已关闭，拒绝该请求");
                        return Ok(());
                    }
                }
                // `claimed` 已经保证只有 `handle_unrouted` 打开时才会走到这里。
                None => self.emit_global(GlobalMessage::OrphanedRequest {
                    id: id.clone(),
                    thread_id,
                    method,
                    request: Box::new(request),
                    reply,
                }),
            },
            RequestScope::Global(request) => self.emit_global(GlobalMessage::Request {
                id: id.clone(),
                method,
                request: Box::new(request),
                reply,
            }),
        }
        Ok(())
    }

    fn on_notification(&mut self, method: &str, params: Value) {
        let tagged = tagged(method, params);
        match parse_notification(&tagged) {
            // 属于某个 thread 的通知分给它认领的路由，其余走全局流。**不做
            // 归纳**：85 种通知一视同仁地原样交出，挑哪几种重要是调用方的判断。
            Some(notification) => match split_notification(notification) {
                NotificationScope::Thread {
                    thread_id,
                    notification,
                } => {
                    if let Some(route) = self.routes.get_mut(&thread_id) {
                        // turn 结束只是解除"在等一个 turn"，**不**关掉出口：
                        // 路由属于 thread 而不是某一个 turn，调用方拿着同一个
                        // 句柄就能接着发下一个 turn。
                        if matches!(notification, ThreadNotification::TurnCompleted(_)) {
                            route.awaiting_turn = false;
                        }
                        self.push(
                            &thread_id,
                            ThreadMessage::Notification(Box::new(notification)),
                        );
                    } else {
                        self.emit_global(GlobalMessage::OrphanedNotification {
                            thread_id,
                            notification: Box::new(notification),
                        });
                    }
                }
                NotificationScope::Global(notification) => {
                    self.emit_global(GlobalMessage::Notification(Box::new(notification)));
                }
            },
            // 本客户端还不认识这种通知：尽力猜一个 thread 归属，猜得到且
            // 那个 thread 有人认领就还投给它，否则只能走全局流。
            None => match sniff_thread_id(&tagged).map(str::to_owned) {
                Some(thread_id) if self.routes.contains_key(&thread_id) => {
                    self.push(&thread_id, ThreadMessage::Unparsed(tagged));
                }
                _ => self.emit_global(GlobalMessage::Unparsed(tagged)),
            },
        }
    }

    fn push(&mut self, thread: &str, message: ThreadMessage) {
        let Some(route) = self.routes.get(thread) else {
            return;
        };
        if route.events.send(message).is_err() {
            // 调用方已经不听了。
            self.routes.remove(thread);
        }
    }

    /// 全局诊断频道尽力交付；请求投递失败时 ReplySender 会被丢弃，随即把
    /// 保守拒绝投回当前连接。
    fn emit_global(&self, message: GlobalMessage) {
        let _ = self.events.try_send(message);
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// 未连接时对一条命令的答复：立刻失败。
    ///
    /// 会订阅的那几条一律失败而不是"先记下来等重连"——订阅是连接级的
    /// （ADR 0005），在没有连接的时候记一笔订阅，等于给下一条连接留下一个假账。
    fn fail(&mut self, command: Command, error: &Error) {
        match command {
            Command::Call {
                request_id,
                responder,
                ..
            } => {
                self.registrations.remove(&request_id);
                let _ = responder.send(Err(clone_error(error)));
            }
            Command::RegisterRoute { .. } => self.local(command),
            Command::IsSubscribed { responder, .. } => {
                let _ = responder.send(Err(clone_error(error)));
            }
        }
    }

    /// 断线清空连接事实；活跃路由保留以便恢复，已发送请求不自动重放。
    fn reset(&mut self, error: &Error) {
        for (_, pending) in self.pending.drain() {
            if let Some(responder) = pending.responder {
                let _ = responder.send(Err(clone_error(error)));
            }
        }
        self.routes.retain(|_, route| !route.events.is_closed());
        self.registrations
            .retain(|_, attach| !attach.events.is_closed());
        self.subscribed.clear();
    }
}

/// 读到 `initialize` 的响应为止。期间到达的通知转发给全局流会更啰嗦而收益
/// 很小——握手窗口只有几毫秒——因此这里只记日志。
async fn await_initialize(ws: &mut WebSocket) -> Result<InitializeResponse> {
    while let Some(message) = ws.next().await {
        match message.map_err(Error::transport)? {
            Message::Text(text) => {
                let incoming =
                    serde_json::from_str::<Incoming>(&text).map_err(|source| Error::Decode {
                        method: "initialize".to_owned(),
                        source,
                    })?;
                let expected = RequestId::Integer(INITIALIZE_ID as i64);
                match incoming {
                    Incoming::Response { id, result } if id == expected => {
                        return serde_json::from_value(result).map_err(|source| Error::Decode {
                            method: "initialize".to_owned(),
                            source,
                        });
                    }
                    Incoming::Error { id, error } if id == expected => {
                        return Err(Error::rejected("initialize", error));
                    }
                    other => {
                        tracing::debug!(?other, "握手期间收到的消息，忽略");
                        continue;
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    Err(Error::Closed {
        method: "initialize".to_owned(),
    })
}

/// 新建 thread 的默认恢复参数。
///
/// 形状来自生成类型而不是手写 `json!`：手写的参数服务端解析不了就等于没恢复
/// 订阅，症状是重连之后那条 thread 再也收不到任何事件。真正用到的覆盖项
/// （reviewer、approvalPolicy）由第一个 turn 通过 请求发送前的 method 识别 补上。
fn resume_params(thread_id: &str) -> Value {
    serde_json::to_value(ThreadResumeParams {
        thread_id: thread_id.to_owned(),
        ..Default::default()
    })
    .expect("ThreadResumeParams 只包含可序列化的生成类型")
}

/// [`Error`] 里有不可克隆的 source，但断线时要把同一个原因发给多个等待者。
fn clone_error(error: &Error) -> Error {
    match error {
        Error::Server(server) => Error::Server(server.clone()),
        Error::Timeout { method, after } => Error::Timeout {
            method: method.clone(),
            after: *after,
        },
        Error::Closed { method } => Error::Closed {
            method: method.clone(),
        },
        Error::Connect { endpoint, source } => Error::Connect {
            endpoint: endpoint.clone(),
            source: source.to_string().into(),
        },
        other => Error::Protocol(other.to_string()),
    }
}

/// 一条 App Server 连接：Unix socket 上的 WebSocket。
type WebSocket = tokio_tungstenite::WebSocketStream<UnixStream>;

/// 建立一条到 App Server 的 WebSocket。
///
/// `endpoint` 形如 `unix:///absolute/path/to/app-server-control.sock`。返回时
/// 只完成了 WebSocket 握手，**尚未** `initialize`——服务端会拒绝握手完成之前
/// 的任何请求，那一步由 [`Actor::dial`] 接着做。
async fn connect(endpoint: &str, timeout: Duration) -> Result<WebSocket> {
    let fail = |source: Box<dyn std::error::Error + Send + Sync>| Error::Connect {
        endpoint: endpoint.to_owned(),
        source,
    };
    let path = endpoint
        .strip_prefix("unix://")
        .ok_or_else(|| fail("endpoint 必须是 unix:///absolute/path/to.sock 形式".into()))?;
    let stream = tokio::time::timeout(timeout, UnixStream::connect(path))
        .await
        .map_err(|_| fail("连接 Unix socket 超时".into()))?
        .map_err(|error| fail(Box::new(error)))?;
    let (ws, _) = tokio::time::timeout(timeout, client_async("ws://localhost/", stream))
        .await
        .map_err(|_| fail("WebSocket 握手超时".into()))?
        .map_err(|error| fail(Box::new(error)))?;
    Ok(ws)
}

/// 发一条 JSON-RPC 文本帧。
///
/// 只接受 [`Outgoing`]：出站消息的形状由信封类型决定，不由调用点的 `json!` 决定。
async fn send(ws: &mut WebSocket, message: &Outgoing<'_>) -> Result<()> {
    let text = serde_json::to_string(message).map_err(|source| Error::Encode { source })?;
    ws.send(Message::Text(text.into()))
        .await
        .map_err(Error::transport)
}
