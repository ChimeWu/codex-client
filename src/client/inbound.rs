//! Messages received from the App Server and handles for replying to its requests.

use crate::generated::types::{InitializeResponse, RequestId};
use crate::protocol::{GlobalNotification, GlobalRequest, ThreadNotification, ThreadRequest};
use serde_json::Value;
use tokio::sync::mpsc;

/// A message on the actor's internal reply channel.
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

/// A one-shot handle for answering a server request.
///
/// Send a JSON response with [`ReplySender::send`]. Dropping the handle refuses the
/// request. If the connection has closed, `send` returns the unsent response value.
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

    /// Send the response value to the server.
    ///
    /// Returns the value if the connection is already closed and the response could not
    /// be delivered.
    pub fn send(mut self, value: Value) -> std::result::Result<(), Value> {
        let target = self
            .target
            .take()
            .expect("reply sender can only be used once");
        target
            .replies
            .send(ServerReply {
                id: target.id,
                method: target.method,
                value: Some(value),
            })
            .map_err(|error| {
                error
                    .0
                    .value
                    .expect("successful replies must contain a value")
            })
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

/// A connection-level message returned by [`crate::AppServerClient::next_event`].
///
/// This stream has a single consumer. Dropping an unhandled server request refuses it.
#[derive(Debug)]
pub enum GlobalMessage {
    /// The handshake completed. This is emitted again after a reconnect.
    Connected(Box<InitializeResponse>),
    /// The connection closed. Active thread channels remain open and the actor resumes
    /// them after reconnecting.
    Disconnected,
    /// A server notification unrelated to any thread.
    Notification(Box<GlobalNotification>),
    /// A server request unrelated to any thread that must be answered.
    Request {
        /// The JSON-RPC ID of this request; see [`ThreadMessage::Request::id`].
        id: RequestId,
        method: &'static str,
        request: Box<GlobalRequest>,
        reply: ReplySender,
    },
    /// A server request owned by a thread that no active turn is listening to.
    OrphanedRequest {
        /// The JSON-RPC ID of this request; see [`ThreadMessage::Request::id`].
        id: RequestId,
        thread_id: String,
        method: &'static str,
        request: Box<ThreadRequest>,
        reply: ReplySender,
    },
    /// A server notification owned by a thread that no active turn is listening to.
    OrphanedNotification {
        thread_id: String,
        notification: Box<ThreadNotification>,
    },
    /// An inbound message this client cannot parse or associate with a thread.
    Unparsed(Value),
}

/// A message associated with a subscribed thread.
///
/// The receiver remains open across turns and transient disconnects. It closes if the
/// subscription cannot be restored, the caller unsubscribes, or the client exits.
#[derive(Debug)]
pub enum ThreadMessage {
    /// A server notification owned by this thread.
    Notification(Box<ThreadNotification>),
    /// A server request owned by this thread that must be answered.
    ///
    /// `reply.send(value)` sends a successful response. Dropping `reply` makes the actor
    /// construct a conservative refusal based on `method`.
    Request {
        /// The JSON-RPC ID of this request.
        ///
        /// Replies do **not** need this ID ([`ReplySender`] retains it). Callers can use
        /// it for correlation: `serverRequest/resolved` contains only `requestId`, so this
        /// ID can find and dismiss UI for a request resolved elsewhere.
        id: RequestId,
        method: &'static str,
        request: Box<ThreadRequest>,
        reply: ReplySender,
    },
    /// An inbound message this client could not parse but associated with this thread.
    Unparsed(Value),
}
