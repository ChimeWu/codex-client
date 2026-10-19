//! 共享客户端负责 RPC，独立接收器负责 thread 事件；一条连接由一个 actor 管理。

pub(crate) mod actor;
pub mod inbound;

use crate::error::{Error, Result};
use crate::generated::methods::Request;
use crate::generated::types::{
    ClientInfo, InitializeCapabilities, InitializeParams, Thread, ThreadListCwdFilter,
    ThreadListParams, ThreadReadParams, ThreadResumeParams, ThreadResumeResponse,
    ThreadStartParams, ThreadStartResponse, ThreadUnsubscribeParams, Turn, TurnInterruptParams,
    TurnStartParams, TurnStartResponse, TurnSteerParams, TurnSteerResponse, UserInput,
};
use actor::{Actor, Attach, Command};
use inbound::{GlobalMessage, ThreadMessage};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot};

const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 3_000;
/// 全局消息流的容量。它是尽力交付的，满了就丢。
const SERVER_EVENT_CAPACITY: usize = 256;
/// 客户端命令通道容量。
const COMMAND_CAPACITY: usize = 128;

/// 连接到一个由管理员运行的 App Server。
///
/// 客户端**从不**启动、停止或监管 App Server 进程——它只是连上去。进程的
/// 生命周期属于 `codex app-server daemon` 或等价的 systemd/launchd 单元。通过
/// [`AppServerClient::builder`] 配置并创建实例。
///
/// 所有操作只需要 `&self`；构造器返回可克隆的 `Arc<AppServerClient>`。
pub struct AppServerClient {
    endpoint: String,
    commands: mpsc::Sender<Command>,
    /// 本地操作编号，用于关联请求与预登记的消息出口，同时作为路由代次。
    next_operation: AtomicU64,
    global: Mutex<mpsc::Receiver<GlobalMessage>>,
}

/// [`AppServerClient`] 的构造器。
pub struct AppServerClientBuilder {
    endpoint: String,
    initialize: InitializeParams,
    connect_timeout: Duration,
    handle_unrouted: bool,
}

impl AppServerClientBuilder {
    /// `endpoint` 形如 `unix:///absolute/path/to/app-server-control.sock`。
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            initialize: InitializeParams {
                client_info: ClientInfo {
                    name: env!("CARGO_PKG_NAME").to_owned(),
                    title: Some("codex-client-cw".to_owned()),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                },
                capabilities: None,
            },
            connect_timeout: Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS),
            handle_unrouted: false,
        }
    }

    /// 自报身份，出现在服务端日志里。
    pub fn with_client_info(mut self, client_info: ClientInfo) -> Self {
        self.initialize.client_info = client_info;
        self
    }

    /// 声明握手能力，例如实验 API 或通知过滤。
    pub fn with_capabilities(mut self, capabilities: InitializeCapabilities) -> Self {
        self.initialize.capabilities = Some(capabilities);
        self
    }

    /// 让服务端不要再推送这些通知。
    ///
    /// 一个 turn 里 `mcpServer/startupStatus/updated` 这类噪音能刷好几条，
    /// 不关心的话在握手时就关掉，比收到之后再过滤便宜。
    pub fn opt_out_notifications<I, S>(mut self, methods: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let methods = methods.into_iter().map(Into::into).collect();
        self.initialize
            .capabilities
            .get_or_insert_with(Default::default)
            .opt_out_notification_methods = Some(methods);
        self
    }

    /// 仅限制连接建立及初始化握手的等待时间，不限制 RPC 执行时间。
    pub fn with_connect_timeout(mut self, connect_timeout_ms: u64) -> Self {
        self.connect_timeout = Duration::from_millis(connect_timeout_ms);
        self
    }

    /// 接管没有被任何 thread 接收器认领的服务端请求，从
    /// [`AppServerClient::next_event`] 取。
    ///
    /// 默认**不**接管：这类请求一律当场拒绝。服务端发出请求后会阻塞等待，把它
    /// 放进一条尽力交付、可能根本没人读的流里，等于让那个 turn 永久卡住。开了
    /// 这个开关就得自己保证及时消费。消息被丢弃会关闭一次性 reply sender，actor
    /// 随即拒绝。
    pub fn handling_unrouted_requests(mut self) -> Self {
        self.handle_unrouted = true;
        self
    }

    /// 创建客户端并启动连接 actor；必须在 Tokio runtime 内调用。
    pub fn build(self) -> Arc<AppServerClient> {
        let initialize = serde_json::to_value(self.initialize)
            .expect("InitializeParams 只包含可序列化的生成类型");
        let (commands_tx, commands_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (global_tx, global_rx) = mpsc::channel(SERVER_EVENT_CAPACITY);
        let actor = Actor::new(
            self.endpoint.clone(),
            initialize,
            self.connect_timeout,
            self.handle_unrouted,
            commands_rx,
            global_tx,
        );
        tokio::spawn(actor.run());
        Arc::new(AppServerClient {
            endpoint: self.endpoint,
            commands: commands_tx,
            next_operation: AtomicU64::new(1),
            global: Mutex::new(global_rx),
        })
    }
}

impl AppServerClient {
    async fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .await
            .map_err(|_| Error::Closed {
                method: "(连接 actor 已退出)".to_owned(),
            })
    }

    /// 领一个新的路由代次，并配一对通道。
    fn attachment(&self) -> (Attach, mpsc::UnboundedReceiver<ThreadMessage>) {
        let epoch = self.next_operation.fetch_add(1, Ordering::Relaxed);
        let (events, messages) = mpsc::unbounded_channel();
        (Attach { epoch, events }, messages)
    }

    pub fn builder(endpoint: impl Into<String>) -> AppServerClientBuilder {
        AppServerClientBuilder::new(endpoint)
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// 发一个类型化的请求。
    ///
    /// 不设统一超时；连接断开时返回失败，不自动重放。断线不代表服务端未执行，
    /// 调用方应按操作语义核对结果。
    ///
    /// 参数类型自己实现了 [`Request`]，方法名和响应类型都由它决定，因此
    /// `client.request(ThreadListParams { .. })` 的返回类型是确定的：
    ///
    /// ```no_run
    /// # use codex_client_cw::{AppServerClient, types::*};
    /// # async fn demo(client: &AppServerClient) -> anyhow::Result<()> {
    /// let models = client.request(ModelListParams::default()).await?;
    /// # let _ = models;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// actor 根据 method 统一维护订阅与 turn 状态，通用入口与便捷方法语义一致。
    /// 启动 turn 前仍需建立并保留目标 thread 的事件接收器。
    pub async fn request<R: Request>(&self, params: R) -> Result<R::Response> {
        let result = self.call(R::METHOD, encode(params)?).await?;
        decode(result, R::METHOD)
    }

    /// 执行单次请求，返回未经解释的 `result`。
    ///
    /// 逃生舱：本客户端的生成代码落后于服务端时仍然能用。正常情况下用
    /// [`AppServerClient::request`]。
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let (responder, receiver) = oneshot::channel();
        self.send(Command::Call {
            request_id: self.next_operation.fetch_add(1, Ordering::Relaxed),
            method: method.to_owned(),
            params,
            responder,
        })
        .await?;
        receiver.await.map_err(|_| Error::Closed {
            method: method.to_owned(),
        })?
    }

    /// 读取下一条与任何 thread 都无关的全局消息。
    ///
    /// 这是单消费者的尽力交付流：服务端请求携带单次回复句柄，不能广播给多个
    /// 消费者竞争应答。并发调用按锁顺序逐条取走消息，不会广播；等待不阻塞 RPC。
    /// 返回 `None` 表示 actor 已退出。
    pub async fn next_event(&self) -> Option<GlobalMessage> {
        self.global.lock().await.recv().await
    }

    /// 创建 thread。本连接随之订阅它，返回独立的消息接收器。
    ///
    /// 接收器跨 turn 存活，`turn/completed` 不关闭它；订阅无法恢复或 actor 退出时
    /// 流才结束。消费任务应持有一份客户端 Arc，接收器本身不维持连接。
    ///
    /// 返回的响应里带着服务端最终采用的 model、sandbox、approvalPolicy 等——
    /// 它们不一定等于你传进去的值（缺省项由服务端配置决定），需要的话读它。
    pub async fn start_thread(
        &self,
        params: ThreadStartParams,
    ) -> Result<(ThreadStartResponse, mpsc::UnboundedReceiver<ThreadMessage>)> {
        self.request_with_receiver(params).await
    }

    /// 恢复 thread，并在响应交付前绑定独立消息接收器。
    /// 总是发送 thread/resume。
    pub async fn resume_thread(
        &self,
        params: ThreadResumeParams,
    ) -> Result<(ThreadResumeResponse, mpsc::UnboundedReceiver<ThreadMessage>)> {
        self.request_with_receiver(params).await
    }

    /// 为响应中的 thread 登记消息出口；RPC 本身仍然只走 Call。
    async fn request_with_receiver<R: Request>(
        &self,
        params: R,
    ) -> Result<(R::Response, mpsc::UnboundedReceiver<ThreadMessage>)> {
        let params = encode(params)?;
        let (attach, messages) = self.attachment();
        let request_id = attach.epoch;
        self.send(Command::RegisterRoute { request_id, attach })
            .await?;
        let (responder, receiver) = oneshot::channel();
        self.send(Command::Call {
            request_id,
            method: R::METHOD.to_owned(),
            params,
            responder,
        })
        .await?;
        let value = receiver.await.map_err(|_| Error::Closed {
            method: R::METHOD.to_owned(),
        })??;
        Ok((decode(value, R::METHOD)?, messages))
    }

    /// 与 request(TurnStartParams) 完全相同；actor 统一检查订阅并维护恢复状态。
    pub async fn start_turn(&self, params: TurnStartParams) -> Result<TurnStartResponse> {
        self.request(params).await
    }

    /// 追加输入；服务端校验 `expected_turn_id` 仍是当前 turn，否则拒绝请求。
    pub async fn steer(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        input: Vec<UserInput>,
    ) -> Result<TurnSteerResponse> {
        self.request(TurnSteerParams {
            thread_id: thread_id.to_owned(),
            expected_turn_id: expected_turn_id.to_owned(),
            input,
            client_user_message_id: None,
        })
        .await
    }

    /// 中断 turn；先补订阅，不替换现有事件接收端。
    pub async fn interrupt(&self, thread_id: &str, turn_id: &str) -> Result<()> {
        self.subscribe(ThreadResumeParams {
            thread_id: thread_id.to_owned(),
            ..Default::default()
        })
        .await?;
        self.request(TurnInterruptParams {
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
        })
        .await?;
        Ok(())
    }

    /// 校验 thread 属于 `expected_cwd`。
    ///
    /// 归属以服务端记录的 cwd 为准，而不是它出现在某个列表里：thread id 往往
    /// 来自用户输入，不能只凭列表来源就认为它属于当前项目。
    pub async fn thread_belongs_to(&self, thread_id: &str, expected_cwd: &str) -> Result<bool> {
        let response = self
            .request(ThreadReadParams {
                thread_id: thread_id.to_owned(),
                include_turns: Some(false),
            })
            .await?;
        Ok(response.thread.cwd == expected_cwd)
    }

    /// 某个目录下最近的 thread，原样交出协议给的 [`Thread`]。
    ///
    /// 需要分页、过滤、排序时用 `request(ThreadListParams { .. })` 传完整参数。
    pub async fn recent_threads(&self, cwd: &str, limit: u32) -> Result<Vec<Thread>> {
        let response = self
            .request(ThreadListParams {
                cwd: Some(ThreadListCwdFilter::Text(cwd.to_owned())),
                limit: Some(limit),
                ..Default::default()
            })
            .await?;
        Ok(response.data)
    }

    /// 让本连接加入一条 thread，**不**接管它的消息流。
    ///
    /// 服务端只在已加入活动 thread 的连接上路由 thread 操作（ADR 0005），因此
    /// `turn/interrupt` 这类请求发出之前必须先走这里。已经订阅时不发任何请求。
    ///
    /// 代价是没有路由：这条 thread 的通知在本连接上会变成孤儿消息，服务端请求
    /// 会被保守拒绝。需要接收消息时应保留 `start_thread` 或 `resume_thread` 返回的
    /// 接收器。
    pub async fn subscribe(&self, resume: ThreadResumeParams) -> Result<()> {
        let (responder, receiver) = oneshot::channel();
        self.send(Command::IsSubscribed {
            thread_id: resume.thread_id.clone(),
            responder,
        })
        .await?;
        if !receiver.await.map_err(|_| Error::Closed {
            method: "(查询订阅)".to_owned(),
        })?? {
            self.request(resume).await?;
        }
        Ok(())
    }

    /// 显式退订某个 thread，并卸下它的路由。
    ///
    /// 最后一个订阅者退出后，服务端在一段静默期后卸载 thread 并发出
    /// `thread/closed`。
    pub async fn unsubscribe(&self, thread_id: &str) -> Result<()> {
        self.request(ThreadUnsubscribeParams {
            thread_id: thread_id.to_owned(),
        })
        .await?;
        Ok(())
    }

    /// 通过 `thread/read` 读回某个 turn，原样交出协议给的 [`Turn`]。
    ///
    /// 作为推送丢失、连接中断和进程重启时的兜底——它不能代替订阅，因为服务端
    /// 请求只会走推送。
    ///
    /// `status`、`items`、`error`、耗时都在返回的 [`Turn`] 上；怎么把 items
    /// 拼成给人看的文本是调用方的事，不同产品的答案不一样。
    pub async fn poll_turn(&self, thread_id: &str, turn_id: &str) -> Result<Option<Turn>> {
        let response = self
            .request(ThreadReadParams {
                thread_id: thread_id.to_owned(),
                include_turns: Some(true),
            })
            .await?;
        Ok(response
            .thread
            .turns
            .into_iter()
            .find(|turn| turn.id == turn_id))
    }
}

/// 参数 → JSON。
fn encode<T: Serialize>(params: T) -> Result<Value> {
    serde_json::to_value(params).map_err(|source| Error::Encode { source })
}

/// JSON → 响应类型。
fn decode<T: DeserializeOwned>(value: Value, method: &str) -> Result<T> {
    serde_json::from_value(value).map_err(|source| Error::Decode {
        method: method.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn rpc_waits_for_a_long_running_request_without_a_deadline() {
        let (commands, mut commands_rx) = mpsc::channel(1);
        let (_global_tx, global) = mpsc::channel(1);
        let client = AppServerClient {
            endpoint: String::new(),
            commands,
            next_operation: AtomicU64::new(1),
            global: Mutex::new(global),
        };
        let request = client.call("test/longRunning", Value::Null);
        let server = async {
            let Command::Call { responder, .. } = commands_rx.recv().await.unwrap() else {
                panic!("应收到普通 RPC");
            };
            // 虚拟时间超过旧的 30 秒限制，调用仍应收到服务端的最终结果。
            tokio::time::sleep(Duration::from_secs(3600)).await;
            responder.send(Ok(Value::Bool(true))).unwrap();
        };
        let (result, ()) = tokio::join!(request, server);
        assert_eq!(result.unwrap(), Value::Bool(true));
    }

    #[tokio::test]
    async fn waiting_for_global_events_does_not_block_rpc_and_can_be_cancelled() {
        let (commands, mut commands_rx) = mpsc::channel(1);
        let (global_tx, global) = mpsc::channel(1);
        let client = Arc::new(AppServerClient {
            endpoint: String::new(),
            commands,
            next_operation: AtomicU64::new(1),
            global: Mutex::new(global),
        });
        // 先实际轮询接收 future，确保它已持锁并等待消息。
        let mut waiting = Box::pin(client.next_event());
        assert!(matches!(
            futures_util::poll!(&mut waiting),
            std::task::Poll::Pending
        ));
        let peer = client.clone();
        let request = peer.call("test", Value::Null);
        let respond = async {
            let Command::Call { responder, .. } = commands_rx.recv().await.unwrap() else {
                panic!("应收到普通 RPC");
            };
            responder.send(Ok(Value::Bool(true))).unwrap();
        };
        let (response, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(request, respond)
        })
        .await
        .unwrap();
        assert_eq!(response.unwrap(), Value::Bool(true));
        drop(waiting);
        global_tx
            .send(GlobalMessage::Unparsed(Value::Bool(true)))
            .await
            .unwrap();
        assert!(matches!(
            peer.next_event().await,
            Some(GlobalMessage::Unparsed(Value::Bool(true)))
        ));
        drop(global_tx);
        assert!(client.next_event().await.is_none());
    }
}
