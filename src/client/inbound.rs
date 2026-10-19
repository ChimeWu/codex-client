//! Inbound messages and one-shot reply handles for server requests.
//!
//! Notifications and requests keep their schema-generated payload types. This module
//! only splits them into [`GlobalMessage`] and [`ThreadMessage`] by thread ownership.
//! [`ReplySender`] can be used once and sends its result to the current connection's
//! actor reply channel; no per-request waiter task is needed.

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

/// A one-shot reply handle for a server request.
///
/// [`ReplySender::send`] consumes the handle, so a request can be answered at most once.
/// Dropping it sends the actor a conservative refusal for that method. If the connection
/// has already closed, sending returns the original response value.
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

/// An inbound message unrelated to any thread, consumed by [`crate::AppServerClient::next_event`].
///
/// A server request can have only one responder, so the global stream has one consumer
/// rather than broadcasting. If no one consumes a message in time, it is dropped and its
/// reply sender closes, causing the actor to send a conservative refusal immediately.
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

/// An inbound message owned by a thread.
///
/// A transient disconnect does not close the channel: after reconnecting, the actor
/// restores the subscription with the saved `thread/resume` parameters. A completed turn
/// does not close the channel. It closes only if the server rejects resume, the message
/// route is replaced, the caller unsubscribes, or the client exits.
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
