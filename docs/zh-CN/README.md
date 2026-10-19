# codex-client-cw

Codex App Server 的 Rust 客户端。覆盖协议的**全部**表面：105 个客户端请求、85 种服务端通知、10 种服务端请求，以及它们用到的 724 个数据类型——全部由 `codex app-server generate-json-schema` 的产物生成。

本 crate 从一个 Telegram 应用项目中独立抽取，不依赖 Telegram 或任何特定业务概念，可以单独使用。

通知、请求、响应的 payload 都来自 schema。客户端只补协议说不出的连接状态、未知消息、thread/global 归属，以及一次性回复通道；产品展示和审批策略由调用方决定。

```toml
[dependencies]
codex-client-cw = "0.1"
```

## 快速开始

前提：本机已经有一个在运行的 App Server。本客户端**从不**启动、停止或监管它——进程的生命周期属于 `codex app-server daemon` 或等价的 systemd/launchd 单元。

```sh
codex app-server daemon start
codex app-server daemon version   # 拿到 socketPath
```

### 通用层：任何一个协议方法

`Request` 实现在**参数类型**上，方法名和响应类型都由它决定——传进去哪个参数就是调哪个方法，返回类型是确定的：

```rust
use codex_client_cw::{AppServerClient, types::*};

let mut client = AppServerClient::builder(
    "unix:///Users/me/.codex/app-server-control/app-server-control.sock",
).build();

let models = client.request(ModelListParams::default()).await?;
for model in models.data {
    println!("{} ({})", model.id, model.display_name);
}

let config = client.request(ConfigReadParams::default()).await?;
```

生成时还没有的方法（服务端比本 crate 新）走逃生舱：`client.call("some/new/method", json!({...}))`。

### thread 层：独立消息流与共享客户端

`start_thread` 和 `resume_thread` 返回独立的
`mpsc::UnboundedReceiver<ThreadMessage>`。所有 RPC 都通过客户端调用，接收器可以跨
turn 使用。actor 根据实际 RPC method 统一维护订阅与 turn 状态，通用 `request/call`
与便捷方法行为一致。`turn/start` 和 inline `review/start` 都要求目标 thread 已订阅且
具有可用接收端。`start_turn_with` 已移除，review 直接使用 `request(ReviewStartParams)`。
需要变更 reviewer 时先通过真实 `thread/resume` 应用配置。

```rust
use codex_client_cw::{AppServerClient, ThreadMessage, ThreadNotification, types::*};

let repo = "/path/to/repo".to_owned();
let (thread, mut handle) = client
    .start_thread(ThreadStartParams { cwd: Some(repo.clone()), ..Default::default() })
    .await?;

let _turn = client
    .start_turn(TurnStartParams {
        thread_id: thread.thread.id.clone(),
        input: vec![serde_json::from_value(serde_json::json!({
            "type": "text", "text": "把 README 里的拼写错误改掉"
        }))?],
        cwd: Some(repo.clone()),
        sandbox_policy: Some(SandboxPolicy::WorkspaceWrite {
            writable_roots: Some(vec![repo.clone()]),
            network_access: Some(false),
            exclude_slash_tmp: None,
            exclude_tmpdir_env_var: None,
        }),
        approval_policy: Some(AskForApproval::Untrusted),
        approvals_reviewer: Some(ApprovalsReviewer::User),
        ..Default::default()
    })
    .await?;

while let Some(message) = handle.recv().await {
    match message {
        // 通知原样交出，85 种一视同仁。哪几种重要是你的产品判断。
        ThreadMessage::Notification(notification) => match *notification {
            ThreadNotification::ItemAgentMessageDelta(payload) => print!("{}", payload.delta),
            ThreadNotification::TurnCompleted(payload) => {
                println!("{:?}", payload.turn.status);
                break;
            }
            _ => {}
        },
        // 服务端在阻塞等待。答得了就用它的生成响应类型答，答不了就明确拒绝。
        ThreadMessage::Request { method, request, reply } => {
            println!("服务端要 {method}：{request:?}");
            drop(reply); // 答不了：actor 立即发送保守拒绝
        }
        ThreadMessage::Unparsed(_) => {}
    }
}
```

## 连接与错误

客户端只开**一条**连接，交给一个独占它的 actor：所有请求、通知和服务端请求在这条连接上多路复用。因此一个实例可以同时开着多条 thread——每条拿到自己的消息流，按 `threadId` 路由。握手（socket + WebSocket + `initialize` + `initialized`）不便宜，`poll_turn` 这类轮询调用尤其不该每次重来。

所有客户端方法只需要 `&self`；`build()` 返回 `Arc<AppServerClient>`，调用 `.clone()`
即可共享。全局接收器由 Tokio Mutex 单独保护，等待 `next_event()` 不阻塞 RPC。
多个读取者逐条竞争消息，不会广播。连接活到最后一份客户端 Arc 被丢弃为止：后台事件
任务需要显式持有一份 Arc，thread 接收器本身不维持连接。

RPC 不设统一超时：连接健康时等待服务端结果，连接断开时 actor 使所有等待中的请求失败，
不自动重放。失败不代表服务端操作未执行，调用方应按操作语义核对结果。
`with_connect_timeout(ms)` 仅限制连接建立与初始化握手；业务等待期限由调用方决定。

断线后 actor 自动退避重连（上限 30 秒），重连期间发出的新请求**立刻失败**而不是空等。活跃 thread 的路由和 resume 参数会保留；新连接握手后 actor 自动恢复订阅，原来的接收器继续工作。turn 在断线期间结束时会补发一条 `turn/completed`，等结果的调用方不会一直等下去。全局流仍会依次给出 `GlobalMessage::Disconnected` 和 `GlobalMessage::Connected`。

失败用结构化的 `Error`，服务端拒绝时 code 与 message 原样保留：

```rust
match client.request(ModelListParams::default()).await {
    Ok(models) => { /* … */ }
    Err(error) if error.is_overloaded() => {          // -32001，退避重试
        tokio::time::sleep(reconnect_delay(1, 30_000)).await;
    }
    Err(error) if error.is_method_not_found() => { /* 服务端比本 crate 旧 */ }
    Err(error) => eprintln!("{error}"),                // 连接问题客户端已重试过
}
```

握手时还可以让服务端别推某些通知，实测一个 turn 能少掉十来条：

```rust
let client = AppServerClient::builder(endpoint)
    .opt_out_notifications([
        "mcpServer/startupStatus/updated",
        "account/rateLimits/updated",
    ])
    .build();
```

## 三条必须知道的规则

**一、通知只发给订阅了该 thread 的连接。** `thread/start` 和 `thread/resume` 会让**发出请求的那条连接**订阅该 thread。在别的连接上对同一个 thread 发 `turn/start` 会成功返回、turn 也真的会跑，但那条连接收不到任何增量、审批和完成通知——这是本协议最容易踩空的地方。启动 turn 前必须建立并保留这两个方法返回的接收器；丢弃接收器意味着放弃该 thread 的消息出口。同一条连接可以订阅任意多个 thread；断线后 actor 会在新连接上自动恢复仍然活跃的订阅。

只想操作一条 thread、不接收它的消息流（典型是从另一个任务中断一个正在跑的 turn）时用 `subscribe`——它只让本连接加入这条 thread，不挂路由。

```rust
client.subscribe(ThreadResumeParams { thread_id: thread_id.clone(), ..Default::default() }).await?;
client.request(TurnInterruptParams { thread_id, turn_id }).await?;
```

**二、服务端请求必须应答。** 服务端会反向发起 JSON-RPC 请求并**阻塞等待**。方法不认识或没有任何 turn 认领时，本客户端会替你发出明确的拒绝，或者在确实答不了时（`attestation/generate` 这类要真实凭据的）回一个 JSON-RPC error。应答的形状一律由 schema 生成的响应类型给出，不是手写的 JSON。

请求随附一个单次 reply sender。发送响应值即成功应答；丢弃 sender 会让 actor 立即按请求方法发送保守拒绝。客户端不设置回复超时，持有 sender 就表示服务端应继续等待。

请求原样交给你：`item/commandExecution/requestApproval` 能用一句「批准」回答，`item/tool/call` 要的是工具执行结果，`attestation/generate` 要的是一份真实凭据——它们在 `ServerRequest` 上本来就是不同的变体，各自的响应类型也不同。哪些该问用户、「批准」映射成什么，是你的产品判断，本 crate 只保留自己必须有的那张兜底拒绝表。

另外服务端请求的 `id` 可以是 `0`（实测），任何用真值判断 id 的实现都会在这里丢请求。

**三、用量不在 `turn/completed` 里。** 协议的 `Turn` 对象没有任何用量字段；token 用量走独立的 `thread/tokenUsage/updated` 通知，并在那里附带 `modelContextWindow`。想在 turn 结束时一并拿到它，就自己记住最后一次。算上下文占用比例时**不要用模型名去猜窗口大小**——`model/list` 也不返回它；`last` 是最近一次请求的用量，`total` 是整条 thread 的累计值，两者含义不同，哪个算「占用」取决于你拿它做什么。

## 安全边界由调用方决定

本客户端**不替你做策略判断**。`thread/shellCommand`、`command/exec*`、`fs/writeFile`、`config/value/write`、插件安装等方法都可以正常调用——哪些该暴露给你自己的用户，是你的产品决定，不是这个库的。每个调用方都应在自己的适配层决定要向用户暴露哪些能力。

两处协议约束值得注意：

- `SandboxPolicy` 是唯一决定 agent 能改动什么的东西，必须由你在服务端侧构造，绝不能让不受信任的输入拼装它。
- `ApprovalsReviewer` 与 `approvalPolicy` 的配对是固定的：`user` 要配 `untrusted`，`auto_review` 要配 `on-request`。用 `user` 配 `on-request` 会让模型自行判断哪些命令"可信"，很多请求根本不会发到你手里。

## 协议类型是生成的

`src/generated/` 下的文件由本 crate 的 `protocol-codegen` bin 从 JSON Schema 生成，已 check-in——使用本 crate 不需要装 codex CLI，也不在构建期跑代码生成。

- `types.rs`：全部数据类型（724 个）
- `methods.rs`：105 个客户端请求的 `Request` 实现（实现在各自的参数类型上；无参方法的参数类型也在这里）
- `events.rs`：`ServerNotification`（85 种）与 `ServerRequest`（10 种）
- `SURFACE.md`：协议表面清单，**专门用来 diff**——codex 升级后重新生成，`git diff` 它就能一眼看出方法级变化

codex 升级后重新生成：

```sh
codex app-server generate-json-schema --out /tmp/codex-schema
cargo run --bin protocol-codegen -- /tmp/codex-schema src/generated
cargo test --all-features
```

**codex 迭代很快，完整的升级流程、schema → Rust 的映射规则、以及编译不过时的排查表都在
[protocol-codegen 的维护文档](protocol-codegen.md) 里。** 升级协议前先读它。

生成的 enum **只包含生成时已知的取值**。协议演进时读不懂的入站消息会作为 `Event::Unparsed` 原样交出 JSON，不会让连接失败——但也不会假装认识它。

## 测试

```sh
cargo test
```

纯单测覆盖无参方法参数的序列化；连接行为由需要真实 daemon 的集成测试验证，这些测试默认忽略且不会启动或停止 daemon。

需要真实 daemon 的测试默认 `#[ignore]`，会消耗额度：

```sh
export CODEX_APP_SERVER_ENDPOINT="unix://$HOME/.codex/app-server-control/app-server-control.sock"
export CODEX_APP_SERVER_TEST_CWD="/path/to/some/repo"   # 可选，默认当前目录
cargo test real_daemon_ -- --ignored --nocapture
```

它们**绝不**启动或停止 daemon，只连接到已经运行的那个。

## 版本

类型由 codex-cli **0.163.0** 的 schema 生成；对 app-server **0.163.0** 实测通过。

## 许可

MIT OR Apache-2.0
