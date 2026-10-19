//! 入站消息，以及服务端请求的一次性回复句柄。
//!
//! 通知与请求仍然使用 schema 生成的 payload 类型；这里只按 thread 归属拆成
//! [`GlobalMessage`] 与 [`ThreadMessage`]。[`ReplySender`] 只允许发送一次；它把
//! 结果投进当前连接专属的 actor 内部回复频道，不需要为每条请求另建等待任务。

use crate::generated::types::{InitializeResponse, RequestId};
use crate::protocol::{GlobalNotification, GlobalRequest, ThreadNotification, ThreadRequest};
use serde_json::Value;
use tokio::sync::mpsc;

/// actor 内部回复频道的一条消息。
#[derive(Debug)]
pub(crate) struct ServerReply {
    pub(crate) id: RequestId,
    pub(crate) method: &'static str,
    pub(crate) value: Option<Value>,
}

#[derive(Debug)]
struct ReplyTarget {
    id: RequestId,
    method: &'static str,
    replies: mpsc::UnboundedSender<ServerReply>,
}

/// 一条服务端请求的单次回复句柄。
///
/// [`ReplySender::send`] 消费自身，因此同一请求最多回复一次。直接丢弃句柄会向
/// actor 投递该方法的保守拒绝；连接已经断开时发送会返回原响应值。
#[derive(Debug)]
pub struct ReplySender {
    target: Option<ReplyTarget>,
}

impl ReplySender {
    pub(crate) fn new(
        id: RequestId,
        method: &'static str,
        replies: mpsc::UnboundedSender<ServerReply>,
    ) -> Self {
        Self {
            target: Some(ReplyTarget {
                id,
                method,
                replies,
            }),
        }
    }

    pub fn send(mut self, value: Value) -> std::result::Result<(), Value> {
        let target = self.target.take().expect("reply sender 只能使用一次");
        target
            .replies
            .send(ServerReply {
                id: target.id,
                method: target.method,
                value: Some(value),
            })
            .map_err(|error| error.0.value.expect("成功回复必须携带响应值"))
    }
}

impl Drop for ReplySender {
    fn drop(&mut self) {
        let Some(target) = self.target.take() else {
            return;
        };
        let _ = target.replies.send(ServerReply {
            id: target.id,
            method: target.method,
            value: None,
        });
    }
}

/// 与任何 thread 都无关的入站消息，由 [`crate::AppServerClient::next_event`] 消费。
///
/// 服务端请求只能有一个回复者，因此全局流是单消费者，而不是广播。没人及时消费
/// 时消息会被丢弃，其中的 reply sender 随即关闭，actor 会立刻发出保守拒绝。
#[derive(Debug)]
pub enum GlobalMessage {
    /// 握手完成。重连之后会再来一次。
    Connected(Box<InitializeResponse>),
    /// 连接断开。活跃 thread 的频道保持打开，重连后 actor 会自动 resume。
    Disconnected,
    /// 与任何 thread 都无关的服务端通知。
    Notification(Box<GlobalNotification>),
    /// 与任何 thread 都无关、必须应答的服务端请求。
    Request {
        /// 这条请求的 JSON-RPC id，见 [`ThreadMessage::Request::id`]。
        id: RequestId,
        method: &'static str,
        request: Box<GlobalRequest>,
        reply: ReplySender,
    },
    /// 有 thread 归属、但没有任何 turn 在监听它的服务端请求。
    OrphanedRequest {
        /// 这条请求的 JSON-RPC id，见 [`ThreadMessage::Request::id`]。
        id: RequestId,
        thread_id: String,
        method: &'static str,
        request: Box<ThreadRequest>,
        reply: ReplySender,
    },
    /// 有 thread 归属、但没有任何 turn 在监听它的服务端通知。
    OrphanedNotification {
        thread_id: String,
        notification: Box<ThreadNotification>,
    },
    /// 本客户端解析不出、也猜不出 thread 归属的入站消息。
    Unparsed(Value),
}

/// 属于某个 thread 的入站消息。
///
/// 临时断线不会关闭频道：actor 成功重连后会用保存的 `thread/resume` 参数恢复
/// 订阅。turn 完成不关闭频道；恢复被服务端拒绝、消息出口被替换、调用方显式
/// 退订或客户端彻底退出时关闭。
#[derive(Debug)]
pub enum ThreadMessage {
    /// 属于这个 thread 的服务端通知。
    Notification(Box<ThreadNotification>),
    /// 属于这个 thread、必须应答的服务端请求。
    ///
    /// `reply.send(value)` 发送成功响应；丢弃 `reply` 则由 actor 按 `method`
    /// 构造保守拒绝。
    Request {
        /// 这条请求的 JSON-RPC id。
        ///
        /// 回复**不需要**它（[`ReplySender`] 自己记着），它是给调用方做关联用
        /// 的：`serverRequest/resolved` 通知只带 `requestId`，想撤掉一个已经被
        /// 别处解决的请求所对应的界面，就得按这个 id 反查。
        id: RequestId,
        method: &'static str,
        request: Box<ThreadRequest>,
        reply: ReplySender,
    },
    /// 本客户端解析不出、但猜出了属于这个 thread 的入站消息。
    Unparsed(Value),
}
