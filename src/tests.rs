//! Fake App Server 与真实 daemon 的连接级测试。
//!
//! Fake server 测试在临时 Unix socket 上跑完整握手，覆盖订阅、审批回复、用量
//! 补齐和未知服务端请求。标记 `#[ignore]` 的测试需要一个**管理员预先运行的**
//! daemon：测试自身绝不启动或停止它。跑它们需要
//! `CODEX_APP_SERVER_ENDPOINT=unix:///…/app-server-control.sock`，
//! 可选 `CODEX_APP_SERVER_TEST_CWD` 指定工作目录。

use crate::client::AppServerClient;
use crate::client::inbound::{GlobalMessage, ThreadMessage};
use crate::generated::methods::{self as requests, Request};
use crate::generated::types::{
    ApprovalsReviewer, AskForApproval, ClientInfo, CommandExecutionApprovalDecision,
    CommandExecutionRequestApprovalResponse, GetAccountRateLimitsParams, ModelListParams,
    SandboxPolicy, ThreadItem, ThreadResumeParams, ThreadStartParams, ThreadTokenUsage,
    TurnInterruptParams, TurnStartParams, TurnStatus, UserInput,
};
use crate::protocol::{ThreadNotification, ThreadRequest};
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{UnixListener, UnixStream};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use uuid::Uuid;

type ServerSocket = tokio_tungstenite::WebSocketStream<UnixStream>;

fn temp_socket_path() -> String {
    format!("/tmp/codex-client-cw-test-{}.sock", Uuid::new_v4().simple())
}

async fn read_json(ws: &mut ServerSocket) -> Value {
    match ws.next().await {
        Some(Ok(Message::Text(text))) => serde_json::from_str(&text).unwrap(),
        _ => panic!("fake server received no JSON"),
    }
}

async fn send_json(ws: &mut ServerSocket, value: Value) {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

/// 应答 `initialize` 并消费 `initialized`，返回握手完成的服务端连接。
async fn accept_handshake(listener: &UnixListener) -> ServerSocket {
    let (stream, _) = listener.accept().await.unwrap();
    let mut ws = accept_async(stream).await.unwrap();
    let init = read_json(&mut ws).await;
    assert_eq!(init["method"], "initialize");
    send_json(
        &mut ws,
        json!({"id":init["id"],"result":{
            "userAgent":"fake","codexHome":"/tmp/.codex",
            "platformFamily":"unix","platformOs":"macos"}}),
    )
    .await;
    assert_eq!(read_json(&mut ws).await["method"], "initialized");
    ws
}

fn text_input(text: &str) -> UserInput {
    serde_json::from_value(json!({"type": "text", "text": text})).expect("文本输入")
}

/// 一个 thread/start 的最小参数：只指定工作目录。
fn start_params(cwd: &str) -> ThreadStartParams {
    ThreadStartParams {
        cwd: Some(cwd.to_owned()),
        ..Default::default()
    }
}

/// 一个 turn/start 的最小参数，reviewer 与它绑定的 approvalPolicy 成对给出。
fn turn_params(
    thread_id: &str,
    text: &str,
    cwd: &str,
    reviewer: ApprovalsReviewer,
) -> TurnStartParams {
    let approval_policy = match reviewer {
        ApprovalsReviewer::User => AskForApproval::Untrusted,
        _ => AskForApproval::OnRequest,
    };
    TurnStartParams {
        thread_id: thread_id.to_owned(),
        input: vec![text_input(text)],
        cwd: Some(cwd.to_owned()),
        sandbox_policy: Some(SandboxPolicy::WorkspaceWrite {
            writable_roots: Some(vec![cwd.to_owned()]),
            network_access: Some(false),
            exclude_slash_tmp: None,
            exclude_tmpdir_env_var: None,
        }),
        approval_policy: Some(approval_policy),
        approvals_reviewer: Some(reviewer),
        ..Default::default()
    }
}

/// 服务端侧：一个符合 schema 的 thread 对象。
///
/// 字段一个都不能少——类型化的客户端会拒绝残缺的响应，这正是要的效果。
fn fake_thread(id: &str, cwd: &str) -> Value {
    json!({
        "id": id, "sessionId": id, "cwd": cwd,
        "createdAt": 0, "updatedAt": 0, "recencyAt": 0,
        "cliVersion": "0.153.0", "modelProvider": "openai",
        "preview": "", "ephemeral": false, "historyMode": "paginated",
        "source": "vscode", "status": {"type": "idle"},
        "path": "/tmp/rollout.jsonl", "canAcceptDirectInput": true,
        "turns": []
    })
}

/// 服务端侧：一个符合 schema 的 thread/start（或 thread/resume）响应。
fn thread_start_result(id: &str, cwd: &str) -> Value {
    json!({
        "thread": fake_thread(id, cwd),
        "cwd": cwd,
        "model": "gpt-5.6-luna",
        "modelProvider": "openai",
        "sandbox": {"type": "workspaceWrite", "writableRoots": [cwd], "networkAccess": false},
        "approvalPolicy": "untrusted",
        "approvalsReviewer": "user",
        "instructionSources": []
    })
}

/// 从一条入站消息里挑出助手回复的增量。
///
/// 这个 crate 不再替谁从 81 种通知里挑出「重要的那几种」——挑的动作现在写在
/// 调用方这边，测试里也一样。
fn delta_of(message: &ThreadMessage) -> Option<&str> {
    match message {
        ThreadMessage::Notification(notification) => match &**notification {
            ThreadNotification::ItemAgentMessageDelta(payload) => Some(&payload.delta),
            _ => None,
        },
        _ => None,
    }
}

fn completed_status(message: &ThreadMessage) -> Option<TurnStatus> {
    match message {
        ThreadMessage::Notification(notification) => match &**notification {
            ThreadNotification::TurnCompleted(payload) => Some(payload.turn.status.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn usage_of(message: &ThreadMessage) -> Option<&ThreadTokenUsage> {
    match message {
        ThreadMessage::Notification(notification) => match &**notification {
            ThreadNotification::ThreadTokenUsageUpdated(payload) => Some(&payload.token_usage),
            _ => None,
        },
        _ => None,
    }
}

/// 用量走独立通知、先于 `turn/completed` 到达，因此想在 turn 结束时一并拿到
/// 它，就得自己记住最后一次——这三行以前藏在 crate 里，现在归调用方。
async fn wait_for_real_turn(
    mut handle: tokio::sync::mpsc::UnboundedReceiver<ThreadMessage>,
) -> Result<(TurnStatus, Option<ThreadTokenUsage>)> {
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let mut last_usage = None;
        while let Some(message) = handle.recv().await {
            if let Some(usage) = usage_of(&message) {
                last_usage = Some(usage.clone());
            }
            match message {
                ThreadMessage::Request { reply, .. } => drop(reply),
                other => {
                    if let Some(status) = completed_status(&other) {
                        return Ok((status, last_usage));
                    }
                }
            }
        }
        // actor 会跨临时断线保留 route；走到这里说明恢复被明确拒绝或客户端退出。
        anyhow::bail!("event stream closed before turn completion")
    })
    .await
    .context("real turn test timed out")?
}

/// 消费一条 thread 的消息流直到当前 turn 结束，把增量文本拼起来。
async fn collect_text(mut handle: tokio::sync::mpsc::UnboundedReceiver<ThreadMessage>) -> String {
    let mut text = String::new();
    while let Some(message) = handle.recv().await {
        if let Some(delta) = delta_of(&message) {
            text.push_str(delta);
        }
        if completed_status(&message).is_some() {
            break;
        }
    }
    text
}

fn daemon_endpoint() -> Option<String> {
    std::env::var("CODEX_APP_SERVER_ENDPOINT").ok()
}

fn daemon_test_cwd() -> String {
    std::env::var("CODEX_APP_SERVER_TEST_CWD").unwrap_or_else(|_| {
        std::env::current_dir()
            .expect("current working directory")
            .to_string_lossy()
            .to_string()
    })
}

#[tokio::test]
async fn initialize_response_is_emitted_as_connected() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let _ws = accept_handshake(&listener).await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();

    let event = tokio::time::timeout(std::time::Duration::from_secs(5), client.next_event())
        .await
        .expect("握手必须及时完成")
        .expect("全局消息频道不应关闭");
    let GlobalMessage::Connected(response) = event else {
        panic!("第一条全局消息必须是 Connected");
    };
    assert_eq!(response.user_agent, "fake");

    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

#[tokio::test]
async fn builder_applies_initialize_options_and_owns_a_connection() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let init = read_json(&mut ws).await;
        assert_eq!(init["params"]["clientInfo"]["name"], "builder-test");
        assert_eq!(
            init["params"]["capabilities"]["optOutNotificationMethods"][0],
            "account/rateLimits/updated"
        );
        send_json(
            &mut ws,
            json!({"id":init["id"],"result":{
                "userAgent":"fake","codexHome":"/tmp/.codex",
                "platformFamily":"unix","platformOs":"macos"}}),
        )
        .await;
        assert_eq!(read_json(&mut ws).await["method"], "initialized");
    });
    let endpoint = format!("unix://{path}");
    let client = AppServerClient::builder(endpoint.clone())
        .with_client_info(ClientInfo {
            name: "builder-test".to_owned(),
            title: None,
            version: "1".to_owned(),
        })
        .opt_out_notifications(["account/rateLimits/updated"])
        .build();
    assert_eq!(client.endpoint(), endpoint);
    assert!(matches!(
        client.next_event().await,
        Some(GlobalMessage::Connected(_))
    ));

    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

#[tokio::test]
async fn unix_websocket_turn_stream() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        assert_eq!(start["method"], "thread/start");
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let req = read_json(&mut ws).await;
        assert_eq!(req["method"], "turn/start");
        assert_eq!(req["params"]["approvalsReviewer"], "auto_review");
        assert_eq!(req["params"]["approvalPolicy"], "on-request");
        assert_eq!(req["params"]["sandboxPolicy"]["type"], "workspaceWrite");
        assert_eq!(req["params"]["sandboxPolicy"]["writableRoots"][0], "/tmp");
        assert_eq!(req["params"]["input"][0]["type"], "text");
        send_json(
            &mut ws,
            json!({"id":req["id"],"result":{"turn":{"id":"turn-fake","items":[],"status":"inProgress"}}}),
        )
        .await;
        send_json(
            &mut ws,
            json!({"method":"item/agentMessage/delta","params":{
                "delta":"hello","itemId":"i","threadId":"thread-fake","turnId":"turn-fake"}}),
        )
        .await;
        send_json(
            &mut ws,
            json!({"id":77,"method":"item/commandExecution/requestApproval","params":{
            "itemId":"i","threadId":"thread-fake","turnId":"turn-fake","startedAtMs":0,
            "command":"echo hi","cwd":"/tmp"}}),
        )
        .await;
        let approval_response = read_json(&mut ws).await;
        assert_eq!(approval_response["id"], 77);
        assert_eq!(approval_response["result"]["decision"], "accept");
        send_json(
            &mut ws,
            json!({"method":"turn/completed","params":{"threadId":"thread-fake",
                "turn":{"id":"turn-fake","items":[],"status":"completed"}}}),
        )
        .await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_thread, mut handle) = client.start_thread(start_params("/tmp")).await.unwrap();
    let turn = client
        .request(turn_params(
            "thread-fake",
            "test",
            "/tmp",
            ApprovalsReviewer::AutoReview,
        ))
        .await
        .unwrap();
    assert_eq!(turn.turn.id, "turn-fake");
    let message = handle.recv().await.expect("增量必须到达");
    assert_eq!(delta_of(&message), Some("hello"));
    match handle.recv().await {
        Some(ThreadMessage::Request {
            id,
            method,
            request,
            reply,
        }) => {
            // 请求原样交给调用方：生成的协议类型，没有第二层枚举，也没有替它
            // 编好的文案。批准的形状同样来自生成的响应类型。
            //
            // JSON-RPC id 也一并交出：`serverRequest/resolved` 只带 requestId，
            // 想撤掉对应的界面就得按它反查。
            assert_eq!(id, crate::generated::types::RequestId::Integer(77));
            let ThreadRequest::ItemCommandExecutionRequestApproval(params) = *request else {
                panic!("命令审批必须解析成命令审批");
            };
            assert_eq!(params.command.as_deref(), Some("echo hi"));
            assert_eq!(method, "item/commandExecution/requestApproval");
            let response = CommandExecutionRequestApprovalResponse {
                decision: CommandExecutionApprovalDecision::Accept,
            };
            reply.send(serde_json::to_value(response).unwrap()).unwrap();
        }
        other => panic!("expected approval request, got {other:?}"),
    }
    assert_eq!(
        handle.recv().await.as_ref().and_then(completed_status),
        Some(TurnStatus::Completed)
    );
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 无参方法的参数类型是我们凭空造的 unit struct；可空 params 仍必须能序列化成 `null`。
///
/// 它们此前是 `type Params = ()`；换成 unit struct 是为了让 `Request` 能实现在
/// 参数类型上（7 个方法没法共用 `()`），线上字节不该跟着变。
#[test]
fn nullary_params_serialize_to_null() {
    assert_eq!(
        serde_json::to_value(requests::AccountLogoutParams).unwrap(),
        Value::Null
    );
    assert_eq!(
        serde_json::to_value(requests::ConfigRequirementsReadParams).unwrap(),
        serde_json::to_value(()).unwrap()
    );
    assert_eq!(
        serde_json::to_value(None::<GetAccountRateLimitsParams>).unwrap(),
        Value::Null
    );
    assert_eq!(
        <Option<GetAccountRateLimitsParams> as Request>::METHOD,
        "account/rateLimits/read"
    );
}

/// 多个请求复用同一条连接，不是每次都重新握手。
///
/// 服务端在这里只接受**一条**连接：如果客户端每个请求都新建连接，第二个请求
/// 会因为没人 accept 而卡在握手上，测试随之失败。
#[tokio::test]
async fn requests_share_one_connection() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        for _ in 0..3 {
            let req = read_json(&mut ws).await;
            assert_eq!(req["method"], "model/list");
            send_json(&mut ws, json!({"id":req["id"],"result":{"data":[]}})).await;
        }
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    for _ in 0..3 {
        client
            .request(ModelListParams::default())
            .await
            .expect("三个请求都应当走同一条连接");
    }
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 连接可能被服务端回收；客户端应当自动重连，而不是把错误抛给调用方。
#[tokio::test]
async fn the_client_reconnects_after_the_server_drops_the_connection() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        // 第一条连接：应答一次之后主动断开。
        let mut ws = accept_handshake(&listener).await;
        let req = read_json(&mut ws).await;
        send_json(&mut ws, json!({"id":req["id"],"result":{"data":[]}})).await;
        ws.close(None).await.unwrap();
        // 客户端应当自己重连过来。
        let mut ws = accept_handshake(&listener).await;
        let req = read_json(&mut ws).await;
        send_json(&mut ws, json!({"id":req["id"],"result":{"data":[]}})).await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    client.request(ModelListParams::default()).await.unwrap();
    client
        .request(ModelListParams::default())
        .await
        .expect("连接被服务端关掉之后应当重连重试，而不是把错误抛给调用方");
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 一条连接同时驱动两个 thread 的 turn，事件各归各位。
///
/// 这是新连接模型的核心：订阅按连接维护（ADR 0005），而这里只有**一条**连接
/// ——fake server 只 accept 一次，客户端要是还想着"每个 turn 一条连接"，第二个
/// turn 会卡在握手上。
#[tokio::test]
async fn one_connection_streams_two_threads_at_once() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;

        let start = read_json(&mut ws).await;
        assert_eq!(start["method"], "thread/start");
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-a","/tmp")}),
        )
        .await;

        // thread-a 刚由 thread/start 订阅上，**不该**再 resume 一次：空 thread
        // 还没有 rollout，resume 它会失败（ADR 0005）。
        let turn_a = read_json(&mut ws).await;
        assert_eq!(turn_a["method"], "turn/start");
        assert_eq!(turn_a["params"]["threadId"], "thread-a");
        send_json(
            &mut ws,
            json!({"id":turn_a["id"],"result":{"turn":{"id":"turn-a","items":[],"status":"inProgress"}}}),
        )
        .await;

        // thread-b 没订阅过，先 resume 再 turn/start——都在同一条连接上。
        let resume = read_json(&mut ws).await;
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"]["threadId"], "thread-b");
        send_json(
            &mut ws,
            json!({"id":resume["id"],"result":thread_start_result("thread-b","/tmp")}),
        )
        .await;
        let turn_b = read_json(&mut ws).await;
        assert_eq!(turn_b["method"], "turn/start");
        send_json(
            &mut ws,
            json!({"id":turn_b["id"],"result":{"turn":{"id":"turn-b","items":[],"status":"inProgress"}}}),
        )
        .await;

        // 两个 thread 的事件交错推送。
        for (thread, item, text) in [("thread-a", "item-a", "AAA"), ("thread-b", "item-b", "BBB")] {
            send_json(
                &mut ws,
                json!({"method":"item/agentMessage/delta","params":{
                    "threadId":thread,"turnId":"t","itemId":item,"delta":text}}),
            )
            .await;
        }
        for (thread, turn) in [("thread-b", "turn-b"), ("thread-a", "turn-a")] {
            send_json(
                &mut ws,
                json!({"method":"turn/completed","params":{"threadId":thread,
                    "turn":{"id":turn,"items":[],"status":"completed"}}}),
            )
            .await;
        }
    });

    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_thread, handle_a) = client.start_thread(start_params("/tmp")).await.unwrap();
    client
        .start_turn(turn_params(
            "thread-a",
            "test",
            "/tmp",
            ApprovalsReviewer::User,
        ))
        .await
        .unwrap();
    let (_, handle_b) = client
        .resume_thread(ThreadResumeParams {
            thread_id: "thread-b".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    client
        .start_turn(turn_params(
            "thread-b",
            "test",
            "/tmp",
            ApprovalsReviewer::User,
        ))
        .await
        .unwrap();

    let (text_a, text_b) = tokio::join!(collect_text(handle_a), collect_text(handle_b));
    assert_eq!(text_a, "AAA", "thread-a 的事件流里不该出现 thread-b 的文本");
    assert_eq!(text_b, "BBB");
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 断线后保留活跃 route；新连接必须自动 resume，并继续把事件送进原频道。
#[tokio::test]
async fn active_routes_resume_after_reconnecting() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let turn = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":turn["id"],"result":{"turn":{"id":"turn-fake","items":[],"status":"inProgress"}}}),
        )
        .await;
        // turn 跑到一半，连接没了。
        drop(ws);
        // 新连接握手后，actor 主动恢复仍有消费者的 route。
        let mut ws = accept_handshake(&listener).await;
        let resume = read_json(&mut ws).await;
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"]["threadId"], "thread-fake");
        send_json(
            &mut ws,
            json!({"id":resume["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        send_json(
            &mut ws,
            json!({"method":"item/agentMessage/delta","params":{
                "delta":"after reconnect","itemId":"i","threadId":"thread-fake",
                "turnId":"turn-fake"}}),
        )
        .await;
        send_json(
            &mut ws,
            json!({"method":"turn/completed","params":{"threadId":"thread-fake",
                "turn":{"id":"turn-fake","items":[],"status":"completed"}}}),
        )
        .await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_thread, mut handle) = client.start_thread(start_params("/tmp")).await.unwrap();
    client
        .request(turn_params(
            "thread-fake",
            "test",
            "/tmp",
            ApprovalsReviewer::User,
        ))
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(message) = client.next_event().await {
            if matches!(message, GlobalMessage::Disconnected) {
                break;
            }
        }
    })
    .await
    .expect("断线必须及时通知到全局流");
    let thread_event = tokio::time::timeout(std::time::Duration::from_secs(5), handle.recv())
        .await
        .expect("恢复订阅后必须继续收到事件")
        .expect("原 thread 频道不应因临时断线关闭");
    assert_eq!(delta_of(&thread_event), Some("after reconnect"));
    assert_eq!(
        handle.recv().await.as_ref().and_then(completed_status),
        Some(TurnStatus::Completed)
    );
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// turn 可能在断线窗口里结束，新的连接收不到那条旧通知。resume 响应里的终态
/// 必须补成原事件流上的 `turn/completed`，否则调用方会永久等待。
#[tokio::test]
async fn reconnect_recovers_a_completion_missed_while_offline() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let turn = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":turn["id"],"result":{"turn":{
                "id":"turn-fake","items":[],"status":"inProgress"}}}),
        )
        .await;
        drop(ws);

        let mut ws = accept_handshake(&listener).await;
        let resume = read_json(&mut ws).await;
        assert_eq!(resume["method"], "thread/resume");
        let mut result = thread_start_result("thread-fake", "/tmp");
        result["thread"]["turns"] = json!([{
            "id":"turn-fake","items":[],"status":"completed"
        }]);
        send_json(&mut ws, json!({"id":resume["id"],"result":result})).await;
    });

    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_thread, mut handle) = client.start_thread(start_params("/tmp")).await.unwrap();
    client
        .start_turn(turn_params(
            "thread-fake",
            "test",
            "/tmp",
            ApprovalsReviewer::User,
        ))
        .await
        .unwrap();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(5), handle.recv())
        .await
        .expect("重连后应从 resume 响应恢复终态")
        .expect("恢复终态之前不应关闭 route");
    assert_eq!(completed_status(&completed), Some(TurnStatus::Completed));
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 一个句柄跑两个 turn：`turn/completed` 不关闭流，中间也不该再 resume 一次。
///
/// 路由属于 thread 而不是某一个 turn。从前一个 turn 一条流，第二个 turn 只能
/// 重新订阅——多一次往返不说，服务端那边的订阅本来就还在。
#[tokio::test]
async fn one_handle_runs_two_turns_in_a_row() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        assert_eq!(start["method"], "thread/start");
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        for turn_id in ["turn-1", "turn-2"] {
            // 两个 turn 之间**没有** thread/resume：读到的下一条必须还是
            // turn/start，否则说明订阅被白白重建了一次。
            let turn = read_json(&mut ws).await;
            assert_eq!(turn["method"], "turn/start", "两个 turn 之间不该再订阅一次");
            send_json(
                &mut ws,
                json!({"id":turn["id"],"result":{"turn":{
                    "id":turn_id,"items":[],"status":"inProgress"}}}),
            )
            .await;
            send_json(
                &mut ws,
                json!({"method":"turn/completed","params":{"threadId":"thread-fake",
                    "turn":{"id":turn_id,"items":[],"status":"completed"}}}),
            )
            .await;
        }
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_thread, mut handle) = client.start_thread(start_params("/tmp")).await.unwrap();
    for expected in ["turn-1", "turn-2"] {
        let turn = client
            .start_turn(turn_params(
                "thread-fake",
                "test",
                "/tmp",
                ApprovalsReviewer::User,
            ))
            .await
            .unwrap();
        assert_eq!(turn.turn.id, expected);
        assert_eq!(
            handle.recv().await.as_ref().and_then(completed_status),
            Some(TurnStatus::Completed),
            "turn/completed 不该关闭 thread 的消息流"
        );
    }
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// Arc 副本维持连接：原客户端先析构，事件流照常。
///
/// 后台事件任务显式持有客户端 Arc，创建它的临时客户端可以先行析构。
/// 连接在那时被抽走的话，turn 一启动就"断线"了。
#[tokio::test]
async fn a_client_clone_keeps_the_connection_alive_after_the_original_is_dropped() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let turn = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":turn["id"],"result":{"turn":{
                "id":"turn-fake","items":[],"status":"inProgress"}}}),
        )
        .await;
        send_json(
            &mut ws,
            json!({"method":"item/agentMessage/delta","params":{
                "delta":"still here","itemId":"i","threadId":"thread-fake",
                "turnId":"turn-fake"}}),
        )
        .await;
        send_json(
            &mut ws,
            json!({"method":"turn/completed","params":{"threadId":"thread-fake",
                "turn":{"id":"turn-fake","items":[],"status":"completed"}}}),
        )
        .await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_thread, mut handle) = client.start_thread(start_params("/tmp")).await.unwrap();
    client
        .start_turn(turn_params(
            "thread-fake",
            "test",
            "/tmp",
            ApprovalsReviewer::User,
        ))
        .await
        .unwrap();
    let _shared = client.clone();
    drop(client);

    let message = tokio::time::timeout(std::time::Duration::from_secs(5), handle.recv())
        .await
        .expect("客户端析构不该带走连接")
        .expect("句柄仍然持有这条连接");
    assert_eq!(delta_of(&message), Some("still here"));
    assert_eq!(
        handle.recv().await.as_ref().and_then(completed_status),
        Some(TurnStatus::Completed)
    );
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 闲着的 thread 重连之后**不**该收到一条凭空的 `turn/completed`。
///
/// 断线补发只针对"发出过一个 turn、还没等到它结束"的路由。路由现在跨 turn
/// 存活，对一条闲着的路由照样补发，等于报告一个没人发起过的 turn 结束了。
#[tokio::test]
async fn an_idle_thread_is_not_told_a_turn_completed_after_reconnecting() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        // 一个 turn 都没发就断线。
        drop(ws);

        let mut ws = accept_handshake(&listener).await;
        let resume = read_json(&mut ws).await;
        assert_eq!(resume["method"], "thread/resume");
        // 这条 thread 历史上跑过 turn，而且都结束了——正是会诱发误报的形状。
        let mut result = thread_start_result("thread-fake", "/tmp");
        result["thread"]["turns"] = json!([{
            "id":"turn-old","items":[],"status":"completed"
        }]);
        send_json(&mut ws, json!({"id":resume["id"],"result":result})).await;
        send_json(
            &mut ws,
            json!({"method":"item/agentMessage/delta","params":{
                "delta":"awake","itemId":"i","threadId":"thread-fake","turnId":"turn-new"}}),
        )
        .await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_thread, mut handle) = client.start_thread(start_params("/tmp")).await.unwrap();

    // 恢复之后的第一条消息必须是那条增量。补发过 turn/completed 的话，它会排
    // 在增量前面，这个断言就会失败。
    let message = tokio::time::timeout(std::time::Duration::from_secs(5), handle.recv())
        .await
        .expect("恢复订阅后必须继续收到事件")
        .expect("闲着的 thread 不该因为重连而关闭");
    assert_eq!(delta_of(&message), Some("awake"));
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 服务端的拒绝要作为结构化错误抛上来，而不是拍成一句字符串。
#[tokio::test]
async fn server_rejections_keep_their_code() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let req = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":req["id"],"error":{"code":-32001,"message":"Server overloaded; retry later."}}),
        )
        .await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let error = client
        .request(ModelListParams::default())
        .await
        .expect_err("服务端返回 error 时不该当成成功");
    assert_eq!(error.code(), Some(-32001));
    assert!(error.is_overloaded(), "调用方要能据此退避重试");
    assert!(!error.is_connection_lost(), "这不是连接问题，重连没有意义");
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 用量走独立通知，且必须在 `turn/completed` 上补齐——协议的 `Turn` 对象里
/// 没有任何用量字段，只从那里取的实现永远只能拿到空值。
#[tokio::test]
async fn token_usage_notification_is_attached_to_turn_completion() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let req = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":req["id"],"result":{"turn":{"id":"turn-fake","items":[],"status":"inProgress"}}}),
        )
        .await;
        // 形状取自真实 daemon 抓包（app-server 0.153.0）。
        send_json(
            &mut ws,
            json!({"method":"thread/tokenUsage/updated","params":{
                "threadId":"thread-fake","turnId":"turn-fake",
                "tokenUsage":{
                    "total":{"totalTokens":14860,"inputTokens":14855,"cachedInputTokens":9984,
                             "cacheWriteInputTokens":0,"outputTokens":5,"reasoningOutputTokens":0},
                    "last":{"totalTokens":14860,"inputTokens":14855,"cachedInputTokens":9984,
                            "cacheWriteInputTokens":0,"outputTokens":5,"reasoningOutputTokens":0},
                    "modelContextWindow":258400}}}),
        )
        .await;
        // 注意 turn/completed 自己不带 usage，与真实协议一致。
        send_json(
            &mut ws,
            json!({"method":"turn/completed","params":{"threadId":"thread-fake",
                "turn":{"id":"turn-fake","items":[],"status":"completed"}}}),
        )
        .await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_thread, mut handle) = client.start_thread(start_params("/tmp")).await.unwrap();
    client
        .start_turn(turn_params(
            "thread-fake",
            "test",
            "/tmp",
            ApprovalsReviewer::User,
        ))
        .await
        .unwrap();

    let message = handle.recv().await.expect("用量必须作为独立通知推送");
    let usage = usage_of(&message).expect("thread/tokenUsage/updated 里带着用量");
    assert_eq!(usage.total.total_tokens, 14860);
    // 抓包来自一个只跑过一个 turn 的 thread，因此 last 与 total 相等。窗口占用
    // 该拿哪一个算，是调用方的判断——这个 crate 不再替它决定。
    assert_eq!(usage.last.total_tokens, 14860);
    assert_eq!(usage.model_context_window, Some(258400));

    assert_eq!(
        handle.recv().await.as_ref().and_then(completed_status),
        Some(TurnStatus::Completed),
        "用量不在 turn/completed 里，它自己走一条通知"
    );
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 通用 `request::<R>()` 打通协议全部方法：方法名、参数与响应类型由 schema 决定。
#[tokio::test]
async fn typed_request_covers_arbitrary_methods() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let req = read_json(&mut ws).await;
        assert_eq!(req["method"], "model/list");
        send_json(
            &mut ws,
            json!({"id":req["id"],"result":{"data":[{
                "id":"gpt-5.6-luna","model":"gpt-5.6-luna","displayName":"GPT-5.6-Luna",
                "description":"","hidden":false,"isDefault":true,
                "defaultReasoningEffort":"medium","supportedReasoningEfforts":[]
            }]}}),
        )
        .await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let models = client.request(ModelListParams::default()).await.unwrap();
    assert_eq!(models.data.len(), 1);
    assert_eq!(models.data[0].id, "gpt-5.6-luna");
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// `config` 原样透传，客户端不平白增删字段。
#[tokio::test]
async fn thread_start_passes_config_through_untouched() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        assert_eq!(start["params"]["config"]["model"], "from-overlay");
        assert_eq!(start["params"]["config"]["instructions"], "你是 architect");
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    client
        .start_thread(ThreadStartParams {
            cwd: Some("/tmp".to_owned()),
            config: Some(json!({"model": "from-overlay", "instructions": "你是 architect"})),
            ..Default::default()
        })
        .await
        .unwrap();
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

#[tokio::test]
async fn resumed_thread_uses_reviewer_and_answers_unknown_requests() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let resume = read_json(&mut ws).await;
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"]["threadId"], "thread-fake");
        assert_eq!(resume["params"]["approvalsReviewer"], "auto_review");
        assert_eq!(resume["params"]["approvalPolicy"], "on-request");
        send_json(
            &mut ws,
            json!({"id":resume["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let req = read_json(&mut ws).await;
        assert_eq!(req["method"], "turn/start");
        send_json(
            &mut ws,
            json!({"id":req["id"],"result":{"turn":{"id":"turn-fake","items":[],"status":"inProgress"}}}),
        )
        .await;
        // 客户端拿不出真实凭据的服务端请求：必须明确报错，不能编造 result，
        // 更不能不回复——不回复会让 App Server 永久阻塞。
        send_json(
            &mut ws,
            json!({"id":42,"method":"attestation/generate","params":{"nonce":"n"}}),
        )
        .await;
        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), read_json(&mut ws))
            .await
            .expect("client must answer an unanswerable server request instead of hanging");
        assert_eq!(reply["id"], 42);
        assert!(
            reply.get("error").is_some(),
            "拿不出凭据就该报错而不是编造：{reply}"
        );
        send_json(
            &mut ws,
            json!({"method":"turn/completed","params":{"threadId":"thread-fake",
                "turn":{"id":"turn-fake","items":[],"status":"completed"}}}),
        )
        .await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    // 没订阅过的 thread：先 `thread/resume` 补上订阅，reviewer 与它的
    // approvalPolicy 一并发送，再在句柄上启动 turn。
    let (_, mut handle) = client
        .resume_thread(ThreadResumeParams {
            thread_id: "thread-fake".to_owned(),
            approvals_reviewer: Some(ApprovalsReviewer::AutoReview),
            approval_policy: Some(AskForApproval::OnRequest),
            ..Default::default()
        })
        .await
        .unwrap();
    client
        .start_turn(turn_params(
            "thread-fake",
            "test",
            "/tmp",
            ApprovalsReviewer::AutoReview,
        ))
        .await
        .unwrap();
    let mut completed = false;
    while let Some(message) = handle.recv().await {
        if completed_status(&message) == Some(TurnStatus::Completed) {
            completed = true;
            break;
        }
        if let ThreadMessage::Request { reply, .. } = message {
            drop(reply);
        }
    }
    assert!(completed);
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 审批请求的 id 可以是 0（实测），丢弃单次回复句柄时也必须替它拒绝。
#[tokio::test]
async fn dropped_approval_declines_request_id_zero() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let req = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":req["id"],"result":{"turn":{"id":"turn-fake","items":[],"status":"inProgress"}}}),
        )
        .await;
        send_json(
            &mut ws,
            json!({"id":0,"method":"item/commandExecution/requestApproval","params":{
            "itemId":"i","threadId":"thread-fake","turnId":"turn-fake","startedAtMs":0,
            "command":"echo hi"}}),
        )
        .await;
        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), read_json(&mut ws))
            .await
            .expect("丢弃回复句柄时客户端必须拒绝");
        assert_eq!(reply["id"], 0, "id 为 0 的请求必须原样回传");
        assert_eq!(reply["result"]["decision"], "decline");
        send_json(
            &mut ws,
            json!({"method":"turn/completed","params":{"threadId":"thread-fake",
                "turn":{"id":"turn-fake","items":[],"status":"completed"}}}),
        )
        .await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_thread, mut handle) = client.start_thread(start_params("/tmp")).await.unwrap();
    client
        .start_turn(turn_params(
            "thread-fake",
            "test",
            "/tmp",
            ApprovalsReviewer::User,
        ))
        .await
        .unwrap();
    // 收到审批请求后直接丢弃，单次句柄会把它转成保守拒绝。
    match handle.recv().await {
        Some(ThreadMessage::Request { reply, .. }) => drop(reply),
        other => panic!("expected approval request, got {other:?}"),
    }
    assert_eq!(
        handle.recv().await.as_ref().and_then(completed_status),
        Some(TurnStatus::Completed)
    );
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 回复邮箱属于单条连接；连接断开后，旧请求的 sender 必须立即失效，不能等到
/// 超时，更不能在重连后误答同号请求。
#[tokio::test]
async fn server_reply_sender_closes_with_its_connection() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let turn = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":turn["id"],"result":{"turn":{"id":"turn-fake","items":[],"status":"inProgress"}}}),
        )
        .await;
        send_json(
            &mut ws,
            json!({"id":7,"method":"item/commandExecution/requestApproval","params":{
                "itemId":"i","threadId":"thread-fake","turnId":"turn-fake","startedAtMs":0,
                "command":"echo hi"}}),
        )
        .await;
        drop(ws);
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_thread, mut handle) = client.start_thread(start_params("/tmp")).await.unwrap();
    client
        .start_turn(turn_params(
            "thread-fake",
            "test",
            "/tmp",
            ApprovalsReviewer::User,
        ))
        .await
        .unwrap();
    let reply = match handle.recv().await {
        Some(ThreadMessage::Request { reply, .. }) => reply,
        other => panic!("expected approval request, got {other:?}"),
    };

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !matches!(client.next_event().await, Some(GlobalMessage::Disconnected)) {}
    })
    .await
    .expect("断线必须关闭当前回复邮箱");
    assert!(reply.send(json!({"decision":"accept"})).is_err());

    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 答不了的服务端请求要明确拒绝，而且拒绝的形状必须来自生成的响应类型。
///
/// 从前这类请求被包装成审批交给用户，用户点「批准」，客户端却还是回一个
/// `success:false`——一个永远不可能兑现的批准按钮。现在请求原样交出，
/// `item/tool/call` 长什么样调用方自己看得见。
#[tokio::test]
async fn tool_calls_are_requests_not_approvals() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let req = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":req["id"],"result":{"turn":{"id":"turn-fake","items":[],"status":"inProgress"}}}),
        )
        .await;
        send_json(
            &mut ws,
            json!({"id":9,"method":"item/tool/call","params":{
                "callId":"c","itemId":"i","threadId":"thread-fake","turnId":"turn-fake",
                "tool":"grep","arguments":{}}}),
        )
        .await;
        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), read_json(&mut ws))
            .await
            .expect("答不了也必须回复，不能让服务端一直等");
        assert_eq!(reply["id"], 9);
        assert_eq!(
            reply["result"],
            json!({"contentItems":[],"success":false}),
            "形状必须符合 DynamicToolCallResponse"
        );
        send_json(
            &mut ws,
            json!({"method":"turn/completed","params":{"threadId":"thread-fake",
                "turn":{"id":"turn-fake","items":[],"status":"completed"}}}),
        )
        .await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_thread, mut handle) = client.start_thread(start_params("/tmp")).await.unwrap();
    client
        .start_turn(turn_params(
            "thread-fake",
            "test",
            "/tmp",
            ApprovalsReviewer::User,
        ))
        .await
        .unwrap();
    match handle.recv().await {
        Some(ThreadMessage::Request {
            method,
            request,
            reply,
            ..
        }) => {
            assert!(matches!(*request, ThreadRequest::ItemToolCall(_)));
            assert_eq!(method, "item/tool/call");
            drop(reply);
        }
        other => panic!("工具调用必须原样交给调用方：{other:?}"),
    }
    assert_eq!(
        handle.recv().await.as_ref().and_then(completed_status),
        Some(TurnStatus::Completed)
    );
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 手上没有句柄时，`turn/interrupt` 之前必须先让本连接加入这条 thread
/// （ADR 0005），否则服务端不会把这条 thread 操作路由过来。
#[tokio::test]
async fn subscribing_without_a_handle_resumes_the_thread_first() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let resume = read_json(&mut ws).await;
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"]["threadId"], "thread-fake");
        send_json(
            &mut ws,
            json!({"id":resume["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let interrupt = read_json(&mut ws).await;
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert_eq!(interrupt["params"]["threadId"], "thread-fake");
        assert_eq!(interrupt["params"]["turnId"], "turn-fake");
        send_json(&mut ws, json!({"id":interrupt["id"],"result":{}})).await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    client.interrupt("thread-fake", "turn-fake").await.unwrap();
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// `/stop` 的形状：从另一条命令中断一个正在跑的 turn，而句柄在别的任务手里。
///
/// 这条路径必须走 `subscribe`——它不挂路由，因此不会顶掉正在跑的那条。
#[tokio::test]
async fn interrupting_without_a_handle_leaves_the_running_stream_alone() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let turn = read_json(&mut ws).await;
        assert_eq!(turn["method"], "turn/start");
        send_json(
            &mut ws,
            json!({"id":turn["id"],"result":{"turn":{
                "id":"turn-fake","items":[],"status":"inProgress"}}}),
        )
        .await;
        // 本连接已经因为 thread/start 订阅上了，subscribe 不该再发一次 resume。
        let interrupt = read_json(&mut ws).await;
        assert_eq!(
            interrupt["method"], "turn/interrupt",
            "已订阅时不该多发一次 thread/resume"
        );
        send_json(&mut ws, json!({"id":interrupt["id"],"result":{}})).await;
        send_json(
            &mut ws,
            json!({"method":"turn/completed","params":{"threadId":"thread-fake",
                "turn":{"id":"turn-fake","items":[],"status":"interrupted"}}}),
        )
        .await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_thread, mut handle) = client.start_thread(start_params("/tmp")).await.unwrap();
    client
        .start_turn(turn_params(
            "thread-fake",
            "test",
            "/tmp",
            ApprovalsReviewer::User,
        ))
        .await
        .unwrap();

    client
        .subscribe(ThreadResumeParams {
            thread_id: "thread-fake".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    client
        .request(TurnInterruptParams {
            thread_id: "thread-fake".to_owned(),
            turn_id: "turn-fake".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(
        handle.recv().await.as_ref().and_then(completed_status),
        Some(TurnStatus::Interrupted),
        "中断不该抢掉这个 turn 自己的消息流"
    );
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

#[tokio::test]
async fn poll_turn_reads_status_and_output_via_thread_read() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let req = read_json(&mut ws).await;
        assert_eq!(req["method"], "thread/read");
        assert_eq!(req["params"]["includeTurns"], true);
        // 形状匹配从真实 daemon 捕获的 `thread/read` 响应。
        let mut thread = json!({"thread": fake_thread("thread-fake", "/tmp")});
        thread["thread"]["turns"] = json!([
            {"id":"turn-other","status":"inProgress","items":[]},
            {"id":"turn-fake","status":"completed","error":null,"items":[
                {"id":"item-1","type":"userMessage","content":[{"type":"text","text":"hi"}]},
                {"id":"item-2","type":"agentMessage","phase":"final_answer","text":"OK"},
                {"id":"item-3","type":"agentMessage","phase":"final_answer","text":" done"}
            ]}
        ]);
        send_json(&mut ws, json!({"id":req["id"],"result":thread})).await;
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let turn = client
        .poll_turn("thread-fake", "turn-fake")
        .await
        .unwrap()
        .expect("turn-fake must be found in the response");
    assert_eq!(turn.status, TurnStatus::Completed);
    assert!(turn.error.is_none());
    // items 原样交出：怎么把它们拼成给人看的文本，不同产品的答案不一样。
    let text = turn
        .items
        .iter()
        .filter_map(|item| match item {
            ThreadItem::AgentMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .concat();
    assert_eq!(text, "OK done");
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

/// 可空 params 在真实 daemon 上仍然可以用 `null` 调用。
///
/// 单测只能证明 unit struct 序列化成 `null`，证明不了服务端吃这一口。挑
/// `account/rateLimits/read` 是因为它只读、无副作用、不消耗额度。
#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon"]
async fn real_daemon_accepts_nullary_params() {
    let Some(endpoint) = daemon_endpoint() else {
        return;
    };
    let client = AppServerClient::builder(endpoint).build();
    client
        .request(None::<GetAccountRateLimitsParams>)
        .await
        .expect("daemon account/rateLimits/read");
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon"]
async fn real_daemon_emits_initialize_response_as_connected() {
    let Some(endpoint) = daemon_endpoint() else {
        return;
    };
    let client = AppServerClient::builder(endpoint).build();
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), client.next_event())
        .await
        .expect("daemon initialize timeout")
        .expect("global event stream closed");
    assert!(matches!(event, GlobalMessage::Connected(_)));
    let models = client
        .request(ModelListParams::default())
        .await
        .expect("daemon model/list");
    assert!(
        !models.data.is_empty(),
        "daemon should publish at least one model"
    );
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon and uses a no-tool test turn"]
async fn real_daemon_turn_start_completes() -> Result<()> {
    let Some(endpoint) = daemon_endpoint() else {
        return Ok(());
    };
    let cwd = daemon_test_cwd();
    let client = AppServerClient::builder(endpoint.clone()).build();
    let (thread, handle) = client.start_thread(start_params(&cwd)).await?;
    client
        .start_turn(turn_params(
            &thread.thread.id,
            "Reply with exactly OK. Do not call tools or make any file changes.",
            &cwd,
            ApprovalsReviewer::User,
        ))
        .await?;
    let (status, usage) = wait_for_real_turn(handle).await?;
    assert_eq!(status, TurnStatus::Completed);
    // 真实 daemon 一定会推送用量；拿不到说明解析又回到了错误的字段上。
    let usage = usage.context("真实 turn 必须报告用量")?;
    assert!(usage.total.total_tokens > 0);
    assert!(
        usage.model_context_window.is_some(),
        "真实 turn 必须报告 modelContextWindow"
    );

    // 新连接必须先 resume（订阅）已持久化的 thread，再启动下一次 turn，
    // 才能确保收到该 thread 的通知。
    let resumed = AppServerClient::builder(endpoint).build();
    let (_, resumed_handle) = resumed
        .resume_thread(ThreadResumeParams {
            thread_id: thread.thread.id.clone(),
            approvals_reviewer: Some(ApprovalsReviewer::User),
            approval_policy: Some(AskForApproval::Untrusted),
            ..Default::default()
        })
        .await?;
    resumed
        .start_turn(turn_params(
            &thread.thread.id,
            "Reply with exactly AGAIN. Do not call tools or make any file changes.",
            &cwd,
            ApprovalsReviewer::User,
        ))
        .await?;
    assert_eq!(
        wait_for_real_turn(resumed_handle).await?.0,
        TurnStatus::Completed
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon and uses a declined approval probe"]
async fn real_daemon_approval_is_pushed_to_turn_connection() -> Result<()> {
    let Some(endpoint) = daemon_endpoint() else {
        return Ok(());
    };
    let cwd = daemon_test_cwd();
    let probe = std::path::Path::new(&cwd)
        .parent()
        .context("approval probe cwd must have a parent")?
        .join(format!(
            "codex-client-cw-approval-probe-{}",
            Uuid::new_v4().simple()
        ));
    let client = AppServerClient::builder(endpoint).build();
    let (thread, mut handle) = client.start_thread(start_params(&cwd)).await?;
    let prompt = format!(
        "Use the shell once to run `touch {}`. Do not use another tool. If permission is denied, reply DENIED.",
        probe.display()
    );
    client
        .start_turn(turn_params(
            &thread.thread.id,
            &prompt,
            &cwd,
            ApprovalsReviewer::User,
        ))
        .await?;
    let (status, saw_approval) = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let mut saw_approval = false;
        while let Some(message) = handle.recv().await {
            if let Some(status) = completed_status(&message) {
                return Ok((status, saw_approval));
            }
            if let ThreadMessage::Request { reply, .. } = message {
                saw_approval = true;
                drop(reply);
            }
        }
        // actor 会跨临时断线保留 route；提前关闭说明无法恢复。
        anyhow::bail!("message stream closed before approval probe completed")
    })
    .await
    .context("approval push test timed out")??;
    assert!(
        saw_approval,
        "turn completed without pushing an approval request"
    );
    assert_eq!(status, TurnStatus::Completed);
    assert!(
        !probe.exists(),
        "declined approval probe unexpectedly created {}",
        probe.display()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon and starts then interrupts a sleep turn"]
async fn real_daemon_turn_can_be_interrupted() -> Result<()> {
    let Some(endpoint) = daemon_endpoint() else {
        return Ok(());
    };
    let cwd = daemon_test_cwd();
    let stream_client = AppServerClient::builder(endpoint.clone()).build();
    let (thread, _handle) = stream_client.start_thread(start_params(&cwd)).await?;
    let turn = stream_client
        .start_turn(turn_params(
            &thread.thread.id,
            "Run `sleep 30`, then reply DONE.",
            &cwd,
            ApprovalsReviewer::User,
        ))
        .await?;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // 另一条连接、手上没有句柄：先加入这条 thread，再发中断。
    let control = AppServerClient::builder(endpoint.clone()).build();
    control
        .subscribe(ThreadResumeParams {
            thread_id: thread.thread.id.clone(),
            ..Default::default()
        })
        .await?;
    control
        .request(TurnInterruptParams {
            thread_id: thread.thread.id.clone(),
            turn_id: turn.turn.id.clone(),
        })
        .await?;

    let status = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let poll = AppServerClient::builder(endpoint.clone()).build();
            if let Some(polled) = poll.poll_turn(&thread.thread.id, &turn.turn.id).await?
                && polled.status != TurnStatus::InProgress
            {
                return Ok::<_, anyhow::Error>(polled.status);
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    })
    .await
    .context("interrupted turn did not reach a terminal state")??;
    assert_eq!(status, TurnStatus::Interrupted);
    Ok(())
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon and probes auto-review on a resumed thread"]
async fn real_daemon_resumed_thread_switches_to_auto_review() -> Result<()> {
    let Some(endpoint) = daemon_endpoint() else {
        return Ok(());
    };
    let cwd = daemon_test_cwd();
    let client = AppServerClient::builder(endpoint.clone()).build();
    let (thread, first) = client.start_thread(start_params(&cwd)).await?;
    client
        .start_turn(turn_params(
            &thread.thread.id,
            "Reply with exactly ASK. Do not call tools or make file changes.",
            &cwd,
            ApprovalsReviewer::User,
        ))
        .await?;
    assert_eq!(wait_for_real_turn(first).await?.0, TurnStatus::Completed);

    let probe = std::path::Path::new(&cwd)
        .parent()
        .context("auto-review probe cwd must have a parent")?
        .join(format!(
            "codex-client-cw-auto-review-probe-{}",
            Uuid::new_v4().simple()
        ));
    let prompt = format!(
        "Use the shell once to run `touch {}`. Do not use another tool. If permission is denied, reply DENIED.",
        probe.display()
    );
    let resumed = AppServerClient::builder(endpoint).build();
    // 新连接、新 reviewer：resume 必须带上 auto_review 与它的 approvalPolicy，
    // 否则 thread 会沿用创建时的那一套。
    let (_, mut handle) = resumed
        .resume_thread(ThreadResumeParams {
            thread_id: thread.thread.id.clone(),
            approvals_reviewer: Some(ApprovalsReviewer::AutoReview),
            approval_policy: Some(AskForApproval::OnRequest),
            ..Default::default()
        })
        .await?;
    resumed
        .start_turn(turn_params(
            &thread.thread.id,
            &prompt,
            &cwd,
            ApprovalsReviewer::AutoReview,
        ))
        .await?;
    let (status, saw_manual_approval) =
        tokio::time::timeout(std::time::Duration::from_secs(180), async {
            let mut saw_manual_approval = false;
            while let Some(message) = handle.recv().await {
                if let Some(status) = completed_status(&message) {
                    return Ok((status, saw_manual_approval));
                }
                if let ThreadMessage::Request { reply, .. } = message {
                    saw_manual_approval = true;
                    drop(reply);
                }
            }
            // actor 会跨临时断线保留 route；提前关闭说明无法恢复。
            anyhow::bail!("message stream closed before auto-review probe completed")
        })
        .await
        .context("auto-review probe timed out")??;
    if probe.exists() {
        tokio::fs::remove_file(&probe).await?;
    }
    assert_eq!(status, TurnStatus::Completed);
    assert!(
        !saw_manual_approval,
        "resumed auto-review thread routed an approval back to the client"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon; starts an ephemeral thread"]
async fn real_daemon_thread_start_accepts_config_overrides() -> Result<()> {
    let Some(endpoint) = daemon_endpoint() else {
        return Ok(());
    };
    let client = AppServerClient::builder(endpoint).build();
    let (response, _handle) = client
        .start_thread(ThreadStartParams {
            cwd: Some("/tmp".to_owned()),
            ephemeral: Some(true),
            model: Some("gpt-5.6-luna".to_owned()),
            ..Default::default()
        })
        .await?;
    assert_eq!(response.model, "gpt-5.6-luna");
    assert!(response.thread.ephemeral);
    Ok(())
}

#[tokio::test]
async fn starting_turn_requires_the_original_live_receiver() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        assert_eq!(start["method"], "thread/start");
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let turn = read_json(&mut ws).await;
        assert_eq!(turn["method"], "turn/start");
        send_json(&mut ws, json!({"id":turn["id"],"result":{"turn":{"id":"turn-fake","items":[],"status":"inProgress"}}})).await;
        send_json(&mut ws, json!({"method":"turn/completed","params":{"threadId":"thread-fake","turn":{"id":"turn-fake","items":[],"status":"completed"}}})).await;
        // 后续非法启动应在客户端拒绝，不得发送到服务端。
        tokio::select! {
            _ = done_rx => {},
            message = read_json(&mut ws) => panic!("意外发送请求：{message}"),
        }
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_, mut current) = client.start_thread(start_params("/tmp")).await.unwrap();
    let params = || turn_params("thread-fake", "test", "/tmp", ApprovalsReviewer::User);
    let mut missing = params();
    missing.thread_id = "missing".to_owned();
    assert!(matches!(
        client.start_turn(missing).await,
        Err(crate::Error::Closed { .. })
    ));
    client.start_turn(params()).await.unwrap();
    assert_eq!(
        current.recv().await.as_ref().and_then(completed_status),
        Some(TurnStatus::Completed)
    );
    current.close();
    assert!(matches!(
        client.start_turn(params()).await,
        Err(crate::Error::Closed { .. })
    ));
    drop(current);
    assert!(matches!(
        client.start_turn(params()).await,
        Err(crate::Error::Closed { .. })
    ));
    done_tx.send(()).unwrap();
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

#[tokio::test]
async fn disconnect_fails_all_pending_requests_without_a_request_timeout() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let first = read_json(&mut ws).await;
        let second = read_json(&mut ws).await;
        assert_eq!(first["method"], "model/list");
        assert_eq!(second["method"], "model/list");
        assert_ne!(first["id"], second["id"]);
        // 两个请求都已发送但尚未响应，关闭连接应使它们全部失败。
        drop(ws);
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    // 这里只给测试设置防挂死期限，客户端自身不使用请求计时器。
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            client.request(ModelListParams::default()),
            client.request(ModelListParams::default())
        )
    })
    .await
    .expect("断线应立即结束等待中的请求");
    assert!(matches!(first, Err(crate::Error::Closed { .. })));
    assert!(matches!(second, Err(crate::Error::Closed { .. })));
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

#[tokio::test]
async fn ordinary_calls_update_subscription_and_unsubscribe_only_on_success() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        assert_eq!(start["method"], "thread/start");
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        // 通用 thread/start 成功后已经建立消息出口。
        for success in [false, true] {
            let request = read_json(&mut ws).await;
            assert_eq!(request["method"], "thread/unsubscribe");
            if success {
                send_json(
                    &mut ws,
                    json!({"id":request["id"],"result":{"status":"unsubscribed"}}),
                )
                .await;
            } else {
                send_json(
                    &mut ws,
                    json!({"id":request["id"],"error":{"code":-32000,"message":"拒绝退订"}}),
                )
                .await;
            }
        }
        // 成功退订已在响应交付前清除订阅，重新接收必须真实 resume。
        let resume = read_json(&mut ws).await;
        assert_eq!(resume["method"], "thread/resume");
        send_json(
            &mut ws,
            json!({"id":resume["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        done_rx.await.unwrap();
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_, mut messages) = client.start_thread(start_params("/tmp")).await.unwrap();
    let resume = || ThreadResumeParams {
        thread_id: "thread-fake".to_owned(),
        ..Default::default()
    };
    assert!(
        client
            .call("thread/unsubscribe", json!({"threadId":"thread-fake"}))
            .await
            .is_err()
    );
    // 失败响应不能改变订阅，也不能关闭已有消息出口。
    client.subscribe(resume()).await.unwrap();
    assert!(matches!(
        messages.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    client
        .call("thread/unsubscribe", json!({"threadId":"thread-fake"}))
        .await
        .unwrap();
    assert!(messages.recv().await.is_none());
    let _replacement = client.resume_thread(resume()).await.unwrap();
    done_tx.send(()).unwrap();
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}

#[tokio::test]
async fn cancelled_resume_still_records_success_without_replacing_the_receiver() {
    let path = temp_socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut ws = accept_handshake(&listener).await;
        let start = read_json(&mut ws).await;
        send_json(
            &mut ws,
            json!({"id":start["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        let resume = read_json(&mut ws).await;
        assert_eq!(resume["method"], "thread/resume");
        seen_tx.send(()).unwrap();
        reply_rx.await.unwrap();
        send_json(
            &mut ws,
            json!({"id":resume["id"],"result":thread_start_result("thread-fake","/tmp")}),
        )
        .await;
        send_json(&mut ws, json!({"method":"turn/completed","params":{"threadId":"thread-fake","turn":{"id":"turn-fake","items":[],"status":"completed"}}})).await;
        done_rx.await.unwrap();
    });
    let client = AppServerClient::builder(format!("unix://{path}")).build();
    let (_, mut messages) = client.start_thread(start_params("/tmp")).await.unwrap();
    let peer = client.clone();
    let task = tokio::spawn(async move {
        peer.resume_thread(ThreadResumeParams {
            thread_id: "thread-fake".to_owned(),
            ..Default::default()
        })
        .await
    });
    seen_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    reply_tx.send(()).unwrap();
    assert_eq!(
        messages.recv().await.as_ref().and_then(completed_status),
        Some(TurnStatus::Completed)
    );
    client
        .subscribe(ThreadResumeParams {
            thread_id: "thread-fake".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    done_tx.send(()).unwrap();
    server.await.unwrap();
    let _ = tokio::fs::remove_file(path).await;
}
