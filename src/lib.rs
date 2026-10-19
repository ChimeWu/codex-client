//! Codex App Server 的 Rust 客户端。
//!
//! 通过 Unix domain socket 上的 WebSocket 连接到一个**已经在运行**的 App
//! Server，覆盖协议的全部表面：99 个客户端请求、81 种服务端通知、10 种服务端
//! 请求，以及它们用到的 680 多个数据类型——全部由
//! `codex app-server generate-json-schema` 的产物生成，见 [`generated`]。
//!
//! # 两层 API
//!
//! 这个 crate **不在协议之上另建一套词汇**：通知、请求、响应都是
//! [`generated`] 里由 schema 生成的类型，原样交出。它只补协议**说不出**的
//! 两件事——连接建立与断开，以及「这条消息本客户端解析不了」。挑哪些通知重要、审批卡片长什么样、上下文
//! 占用怎么算，都是调用方的产品判断，不在这里。
//!
//! **通用层**：[`AppServerClient::request`] 可以调用协议的任何一个方法。每个参数
//! 类型自己实现了 [`Request`]（见 [`generated::methods`]），方法名和响应类型都由
//! 它决定，因此传进去哪个参数就是调哪个方法：
//!
//! ```no_run
//! # use codex_client_cw::{AppServerClient, types::*};
//! # async fn demo(client: &AppServerClient) -> anyhow::Result<()> {
//! let models = client.request(ModelListParams::default()).await?;
//! for model in models.data {
//!     println!("{} ({})", model.id, model.display_name);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! **thread 层**：订阅方法返回独立事件接收器，RPC 统一通过共享客户端调用。
//! actor 依据实际 method 维护订阅与 turn 状态，通用 request/call 与便捷方法一致。
//! 接收器可跨 turn 使用；事件消费任务应持有一份客户端 Arc 来维持连接。
//!
//! ```no_run
//! use codex_client_cw::{AppServerClient, ThreadMessage, ThreadNotification, types::*};
//!
//! # async fn demo() -> anyhow::Result<()> {
//! let repo = "/path/to/repo".to_owned();
//! let client = AppServerClient::builder(
//!     "unix:///Users/me/.codex/app-server-control/app-server-control.sock",
//! ).build();
//!
//! let (thread, mut handle) = client
//!     .start_thread(ThreadStartParams { cwd: Some(repo.clone()), ..Default::default() })
//!     .await?;
//!
//! let _turn = client
//!     .start_turn(TurnStartParams {
//!         thread_id: thread.thread.id.clone(),
//!         input: vec![serde_json::from_value(serde_json::json!({
//!             "type": "text", "text": "把 README 里的拼写错误改掉"
//!         }))?],
//!         cwd: Some(repo.clone()),
//!         sandbox_policy: Some(SandboxPolicy::WorkspaceWrite {
//!             writable_roots: Some(vec![repo.clone()]),
//!             network_access: Some(false),
//!             exclude_slash_tmp: None,
//!             exclude_tmpdir_env_var: None,
//!         }),
//!         approval_policy: Some(AskForApproval::Untrusted),
//!         approvals_reviewer: Some(ApprovalsReviewer::User),
//!         ..Default::default()
//!     })
//!     .await?;
//!
//! while let Some(message) = handle.recv().await {
//!     match message {
//!         // 通知原样交出。哪几种重要、怎么攒成进度，是你的产品判断。
//!         ThreadMessage::Notification(notification) => match *notification {
//!             ThreadNotification::ItemAgentMessageDelta(payload) => print!("{}", payload.delta),
//!             ThreadNotification::TurnCompleted(payload) => {
//!                 println!("{:?}", payload.turn.status);
//!                 break;
//!             }
//!             _ => {}
//!         },
//!         // 服务端在阻塞等待。答得了就用它的生成响应类型答，答不了就明确拒绝。
//!         ThreadMessage::Request { method, request, reply, .. } => {
//!             println!("服务端要 {method}：{request:?}");
//!             // 丢弃一次性 sender 就是明确表示答不了；actor 会立即保守拒绝。
//!             drop(reply);
//!         }
//!         ThreadMessage::Unparsed(_) => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # 连接与错误
//!
//! 客户端只开**一条**连接，交给一个独占它的 actor：所有请求、通知和服务端
//! 请求在这条连接上多路复用，因此一个实例可以同时开着多条 thread，每条拿到
//! 自己的消息流（按 `threadId` 路由——同一时刻一个 thread 上只有一个未完成的
//! turn，因此这个 id 单独就足够定位它）。一整轮握手（socket、WebSocket、
//! `initialize`、`initialized`）不便宜，[`AppServerClient::poll_turn`] 这类
//! 轮询尤其不该每次重来。
//!
//! 所有客户端方法只需要 `&self`，构造器返回 `Arc<AppServerClient>`。
//! 全局接收器单独加锁；等待事件不会阻塞 RPC。事件消费任务应持有一份客户端
//! Arc，最后一份 Arc 丢弃后连接关闭；thread 接收器本身不维持连接。
//!
//! 断线后 actor 自动退避重连，重连期间发出的新请求**立刻失败**而不是空等；
//! [`AppServerClient::next_event`] 会给出 [`GlobalMessage::Disconnected`] 与
//! [`GlobalMessage::Connected`]。活跃 thread 的路由与 resume 参数跨连接保留；
//! 新连接握手后 actor 会主动恢复订阅，原来的接收器继续工作。turn 在断线期间结束
//! 时补发一条 `turn/completed`，等结果的调用方不会一直等下去。
//!
//! 失败用 [`Error`] 表达，调用方能分辨该怎么处理：
//!
//! ```no_run
//! # use codex_client_cw::{AppServerClient, types::*, reconnect_delay};
//! # async fn demo(client: &AppServerClient) {
//! match client.request(ModelListParams::default()).await {
//!     Ok(models) => { let _ = models; }
//!     // 服务端过载：退避重试。
//!     Err(error) if error.is_overloaded() => {
//!         tokio::time::sleep(reconnect_delay(1, 30_000)).await;
//!     }
//!     // 本客户端比服务端新，这个方法那边还没有。
//!     Err(error) if error.is_method_not_found() => {}
//!     // 其它：连接问题客户端已经自己重试过了，到这里说明真的失败了。
//!     Err(error) => eprintln!("{error}"),
//! }
//! # }
//! ```
//!
//! 服务端拒绝时 code 与 message 原样保留在 [`ServerError`] 里，不会被拍成
//! 一句字符串。
//!
//! # 三条必须知道的规则
//!
//! **一、通知只发给订阅了该 thread 的连接。** `thread/start` 和 `thread/resume`
//! 会让**发出请求的那条连接**订阅该 thread。在别的连接上对同一个 thread 发
//! `turn/start` 会成功返回、turn 也真的会跑，但那条连接收不到任何增量、审批和
//! 完成通知。启动 turn 前必须通过 `thread/start` 或 `thread/resume` 建立并保留该
//! thread 的接收器；丢弃后不能另挂一条出口。同一条连接可以订阅任意多个 thread，
//! 所以不必为了换 thread 去换连接。断线会让服务端的订阅作废，但 actor 会保存活跃
//! 路由，并在新连接握手后自动 resume。
//!
//! 只想操作一条 thread、不打算接管它的消息流（典型是从另一个任务中断一个正在
//! 跑的 turn）时用 [`AppServerClient::subscribe`]：它只让本连接加入这条 thread。
//! 它不会改变已经存在的消息出口。
//!
//! **二、服务端请求必须应答。** 服务端会反向发起 JSON-RPC 请求并**阻塞等待**。
//! 没有接收器认领或方法不认识时，本客户端会替你发出明确的拒绝或 JSON-RPC error。
//! 应答的形状一律由 schema 生成的响应类型给出，不是手写的 JSON。另外服务端
//! 请求的 `id` 可以是 `0`（实测），任何用真值判断 id 的实现都会在这里丢请求。
//!
//! 请求消息携带一个 [`ReplySender`]。发送响应值即成功应答；丢弃 sender 会让 actor
//! 立即按请求方法发出保守拒绝。`send` 消费 sender，保证一条服务端请求最多收到一个
//! 调用方决定；除此以外不设客户端超时，服务端请求会一直等待调用方回复。
//!
//! 请求原样交给你：`item/commandExecution/requestApproval` 能用一句「批准」
//! 回答，`item/tool/call` 要的是工具执行结果，`attestation/generate` 要的是
//! 一份真实凭据——它们在 [`ServerRequest`] 上本来就是不同的变体，各自的响应
//! 类型也不同。哪些该问用户、怎么问，是你的产品判断。
//!
//! **三、用量不在 `turn/completed` 里。** 协议的 `Turn` 对象没有任何用量字段；
//! token 用量走独立的 `thread/tokenUsage/updated` 通知，并在那里附带
//! `modelContextWindow`。想在 turn 结束时一并拿到它，就自己记住最后一次。
//! 算上下文占用比例时**不要用模型名去猜窗口大小**——`model/list` 也不返回它；
//! `last` 是最近一次请求的用量，`total` 是整条 thread 的累计值，两者含义不同。
//!
//! # 安全边界由调用方决定
//!
//! 本客户端**不替你做策略判断**：`thread/shellCommand`、`command/exec*`、
//! `fs/writeFile`、`config/value/write`、插件安装等方法都可以正常调用。哪些该
//! 暴露给你自己的用户，是你的产品决定，不是这个库的。
//!
//! 需要注意的两处协议约束：
//!
//! - [`types::SandboxPolicy`] 是唯一决定 agent 能改动什么的东西，必须由你在
//!   服务端侧构造，绝不能让不受信任的输入拼装它。
//! - [`types::ApprovalsReviewer`] 与 `approvalPolicy` 的配对是固定的：`user`
//!   要配 `untrusted`，`auto_review` 要配 `on-request`。用 `user` 配
//!   `on-request` 会让模型自行判断哪些命令"可信"，很多请求根本不会发到你手里。
//!
//! # 少收点噪音
//!
//! 握手时可以让服务端别推某些通知——实测一个 turn 能少掉十来条：
//!
//! ```no_run
//! # use codex_client_cw::AppServerClient;
//! let client = AppServerClient::builder("unix:///path/to.sock")
//!     .opt_out_notifications([
//!         "mcpServer/startupStatus/updated",
//!         "account/rateLimits/updated",
//!     ])
//!     .build();
//! ```
//!
//! # 版本
//!
//! 生成自 codex 0.153.0 的 schema，对 app-server 0.150.1 与 0.153.0 实测通过。
//! 协议演进时解析不了的入站消息会作为 [`GlobalMessage::Unparsed`]/
//! [`ThreadMessage::Unparsed`] 原样交出，不会让连接失败；重新生成见
//! `src/bin/protocol-codegen/`，维护说明见 `docs/protocol-codegen.md`。

pub mod client;
pub mod error;
pub mod generated;
// 信封与协议细节是**实现**，不是契约：这个 crate 承诺的是下面 `pub use` 出去
// 的那些类型和 `AppServerClient` 的方法。把解析、分类、应答构造也摆到公开面上，
// 等于把每一次内部整理都变成一次破坏性变更。
mod envelope;
mod protocol;

#[cfg(feature = "testing")]
pub mod testing;

#[cfg(test)]
mod tests;

pub use client::inbound::{GlobalMessage, ReplySender, ThreadMessage};
pub use client::{AppServerClient, AppServerClientBuilder};
pub use error::{Error, Result, ServerError};
pub use generated::events::{ServerNotification, ServerRequest};
// `requests` 里现在只剩 `Request` 本身和无参方法的参数类型（例如
// `requests::AccountLogoutParams`）——有参方法的参数类型都在 `types` 里。
pub use generated::methods::{self as requests, Request};
pub use generated::types;
pub use protocol::{
    GlobalNotification, GlobalRequest, ThreadNotification, ThreadRequest, reconnect_delay,
};
