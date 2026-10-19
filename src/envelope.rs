//! JSON-RPC envelopes: four inbound shapes and four outbound shapes.
//!
//! Mirrors the Codex repository's `JSONRPCMessage`. Requests, notifications, responses,
//! and errors are **direction-independent**: a server request has the same shape as a
//! client request, and a reply to a server request has the same shape as a server reply.
//!
//! This is the **only** place in the crate that assembles and disassembles these shapes.
//! Do not construct `json!({"id": .., "method": ..})` elsewhere: every handwritten
//! envelope would be a protocol implementation without compile-time checks.

use crate::generated::types::RequestId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC "method not found" error code.
pub(crate) const METHOD_NOT_SUPPORTED: i64 = -32601;
/// JSON-RPC "internal error" code.
pub(crate) const INTERNAL_ERROR: i64 = -32603;

/// An inbound message.
///
/// Variant order is the matching priority for this untagged enum: responses require
/// `result`, errors require `error`, requests require `id` and `method`, and anything else
/// is a notification. If none match, deserialization fails and the caller passes the
/// message through as unparsed rather than terminating the connection.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Incoming {
    /// A request we sent received a successful response.
    Response { id: RequestId, result: Value },
    /// A request we sent was rejected by the server.
    Error { id: RequestId, error: ErrorObject },
    /// A request initiated by the server. **It must be answered**, or the server waits
    /// indefinitely.
    Request {
        id: RequestId,
        method: String,
        #[serde(default)]
        params: Value,
    },
    /// A server notification that requires no reply.
    Notification {
        method: String,
        #[serde(default)]
        params: Value,
    },
}

/// An outbound message.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum Outgoing<'a> {
    Request {
        id: u64,
        method: &'a str,
        params: &'a Value,
    },
    Notification {
        method: &'a str,
        params: Value,
    },
    Result {
        id: &'a RequestId,
        result: &'a Value,
    },
    Error {
        id: &'a RequestId,
        error: &'a ErrorObject,
    },
}

/// A JSON-RPC `error` object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ErrorObject {
    pub(crate) code: i64,
    pub(crate) message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<Value>,
}

impl ErrorObject {
    pub(crate) fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// A reply to a server request.
///
/// `Error` is for requests this client **cannot** answer on its own. Fabricating a result
/// would give the server false data; an explicit error is the honest response.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Reply {
    Result(Value),
    Error(ErrorObject),
}

impl Reply {
    /// Construct a successful reply from a generated response type.
    ///
    /// **The only entry point for constructing replies**: the schema determines their
    /// shape instead of handwritten `json!`. Generated response types have no maps with
    /// non-string keys, so serialization should not fail. If it does, the server still
    /// needs a reply or it will wait indefinitely.
    pub(crate) fn ok<R: Serialize>(response: R) -> Self {
        match serde_json::to_value(response) {
            Ok(result) => Self::Result(result),
            Err(source) => Self::Error(ErrorObject::new(
                INTERNAL_ERROR,
                format!("client failed to encode its response: {source}"),
            )),
        }
    }

    /// This client cannot answer the method.
    pub(crate) fn unsupported(reason: impl Into<String>) -> Self {
        Self::Error(ErrorObject::new(METHOD_NOT_SUPPORTED, reason))
    }

    pub(crate) fn as_outgoing<'a>(&'a self, id: &'a RequestId) -> Outgoing<'a> {
        match self {
            Self::Result(result) => Outgoing::Result { id, result },
            Self::Error(error) => Outgoing::Error { id, error },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: Value) -> Incoming {
        serde_json::from_value(value).expect("all four message shapes should be recognized")
    }

    #[test]
    fn the_four_shapes_are_told_apart() {
        assert!(matches!(
            parse(json!({"id":1,"result":{}})),
            Incoming::Response { .. }
        ));
        assert!(matches!(
            parse(json!({"id":1,"error":{"code":-1,"message":"no"}})),
            Incoming::Error { .. }
        ));
        assert!(matches!(
            parse(json!({"id":2,"method":"approval","params":{}})),
            Incoming::Request { .. }
        ));
        assert!(matches!(
            parse(json!({"method":"event","params":{}})),
            Incoming::Notification { .. }
        ));
        // None of the four shapes matched: let the caller handle it as unparsed rather
        // than guessing.
        assert!(serde_json::from_value::<Incoming>(json!({"x":1})).is_err());
    }

    #[test]
    fn server_request_id_zero_is_still_a_request() {
        // Server request IDs can be 0. Treating the ID as a boolean silently drops an
        // approval here and leaves the turn stuck.
        let Incoming::Request { id, method, .. } =
            parse(json!({"id":0,"method":"item/commandExecution/requestApproval"}))
        else {
            panic!("a server request with ID 0 must still be recognized as a request");
        };
        assert_eq!(id, RequestId::Integer(0));
        assert_eq!(method, "item/commandExecution/requestApproval");
    }

    #[test]
    fn requests_without_params_are_accepted() {
        let Incoming::Notification { params, .. } = parse(json!({"method":"ping"})) else {
            panic!("a notification without params must still be recognized as a notification");
        };
        assert_eq!(params, Value::Null);
    }

    #[test]
    fn replies_serialize_into_the_jsonrpc_shape() {
        let id = RequestId::Integer(0);
        let ok = Reply::Result(json!({"decision":"decline"}));
        assert_eq!(
            serde_json::to_value(ok.as_outgoing(&id)).unwrap(),
            json!({"id":0,"result":{"decision":"decline"}})
        );
        let err = Reply::unsupported("nope");
        assert_eq!(
            serde_json::to_value(err.as_outgoing(&id)).unwrap(),
            json!({"id":0,"error":{"code":METHOD_NOT_SUPPORTED,"message":"nope"}})
        );
    }
}
