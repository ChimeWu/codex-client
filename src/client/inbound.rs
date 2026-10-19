//! Events received from the App Server and handles for replying to its requests.

use crate::generated::{
    events::{ServerNotification, ServerRequest},
    types::{InitializeResponse, RequestId},
};
use serde_json::Value;
use tokio::sync::mpsc;

/// A message received from the App Server connection.
#[derive(Debug)]
pub enum AppServerEvent {
    /// The initialization handshake completed. Emitted again after each reconnect.
    Connected(Box<InitializeResponse>),
    /// The connection closed. The client will attempt to reconnect.
    Disconnected,
    /// A notification parsed using the generated protocol types.
    Notification(Box<ServerNotification>),
    /// A server request that requires exactly one response.
    Request {
        /// JSON-RPC request identifier.
        id: RequestId,
        /// Protocol method name.
        method: String,
        /// Parsed generated request, or the original params if no generated request type can parse it.
        request: std::result::Result<Box<ServerRequest>, Value>,
        /// One-shot response handle. Dropping it sends a conservative refusal.
        reply: ReplySender,
    },
    /// A message that did not match a known JSON-RPC envelope or notification schema.
    Unparsed(Value),
}

/// A message posted to the actor's reply channel.
#[derive(Debug)]
pub(crate) struct ServerReply {
    pub(crate) id: RequestId,
    pub(crate) method: String,
    pub(crate) value: Option<Value>,
}

#[derive(Debug)]
struct ReplyTarget {
    id: RequestId,
    method: String,
    replies: mpsc::UnboundedSender<ServerReply>,
}

/// A one-shot handle for answering a server request.
#[derive(Debug)]
pub struct ReplySender {
    target: Option<ReplyTarget>,
}

impl ReplySender {
    pub(crate) fn new(
        id: RequestId,
        method: String,
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

    /// Send a successful JSON result to the server.
    ///
    /// Returns the value if the connection is already closed and the response could not be delivered.
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
