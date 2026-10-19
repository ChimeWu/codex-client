//! Client error types.
//!
//! Errors stay structured so callers can distinguish a lost connection, an explicit
//! server rejection, an overloaded server that should be retried with backoff, and a
//! response that no longer matches the protocol.

use crate::envelope::ErrorObject;
use serde_json::Value;
use std::fmt;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, Error>;

/// JSON-RPC "server overloaded" error; the protocol recommends retrying with backoff.
pub const SERVER_OVERLOADED: i64 = -32001;
/// JSON-RPC "method not found" error.
pub const METHOD_NOT_FOUND: i64 = -32601;

#[derive(Debug)]
pub enum Error {
    /// Could not connect or complete the WebSocket handshake.
    Connect {
        endpoint: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The connection closed while in use. Reconnecting usually recovers.
    Transport {
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The peer closed the connection while waiting for a response.
    Closed { method: String },
    /// The initialization handshake exceeded its connection timeout; ordinary RPCs have
    /// no shared timeout.
    Timeout { method: String, after: Duration },
    /// The server returned a JSON-RPC `error`. Preserve its structure; do not flatten it
    /// into a string.
    ///
    /// Boxed because `Result` is on every request's hot path: otherwise the largest error
    /// variant would increase the size of successful results too.
    Server(Box<ServerError>),
    /// Could not serialize the parameters. This usually means the caller supplied invalid
    /// JSON.
    Encode { source: serde_json::Error },
    /// The response did not parse as the expected type, usually because the protocol
    /// changed and the generated types need to be refreshed.
    Decode {
        method: String,
        source: serde_json::Error,
    },
    /// A protocol-level issue, such as a response missing a required field.
    Protocol(String),
}

/// Details returned when the server explicitly rejects a request.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerError {
    pub method: String,
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl Error {
    /// Details of the server rejection.
    pub fn server(&self) -> Option<&ServerError> {
        match self {
            Self::Server(error) => Some(error),
            _ => None,
        }
    }

    /// Whether retrying on a new connection may help.
    ///
    /// Connection failures may be retried. An explicit server rejection should not be
    /// retried because the server will reject it again.
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, Self::Transport { .. } | Self::Closed { .. })
    }

    /// The server reported overload; retry with backoff (see `reconnect_delay`).
    pub fn is_overloaded(&self) -> bool {
        self.code() == Some(SERVER_OVERLOADED)
    }

    /// The server does not recognize this method, likely because this client is newer.
    pub fn is_method_not_found(&self) -> bool {
        self.code() == Some(METHOD_NOT_FOUND)
    }

    /// The error code returned by the server.
    pub fn code(&self) -> Option<i64> {
        self.server().map(|error| error.code)
    }

    pub(crate) fn transport(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Transport {
            source: source.into(),
        }
    }

    /// Associate a JSON-RPC `error` object with a method.
    pub(crate) fn rejected(method: &str, error: ErrorObject) -> Self {
        Self::Server(Box::new(ServerError {
            method: method.to_owned(),
            code: error.code,
            message: error.message,
            data: error.data,
        }))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect { endpoint, source } => {
                write!(f, "failed to connect to App Server at {endpoint}: {source}")
            }
            Self::Transport { source } => write!(f, "App Server connection interrupted: {source}"),
            Self::Closed { method } => {
                write!(
                    f,
                    "App Server closed the connection while waiting for {method}"
                )
            }
            Self::Timeout { method, after } => {
                write!(f, "{method} did not return within {:?}", after)
            }
            Self::Server(error) => write!(
                f,
                "App Server rejected {}: [{}] {}",
                error.method, error.code, error.message
            ),
            Self::Encode { source } => {
                write!(f, "failed to serialize request parameters: {source}")
            }
            Self::Decode { method, source } => {
                write!(
                    f,
                    "failed to parse the response for {method} (the protocol may have changed): {source}"
                )
            }
            Self::Protocol(detail) => write!(f, "protocol error: {detail}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect { source, .. } | Self::Transport { source } => Some(source.as_ref()),
            Self::Encode { source } | Self::Decode { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_errors_keep_their_structure() {
        let error = Error::rejected(
            "turn/start",
            ErrorObject {
                code: SERVER_OVERLOADED,
                message: "Server overloaded; retry later.".to_owned(),
                data: None,
            },
        );
        assert_eq!(error.code(), Some(SERVER_OVERLOADED));
        assert!(
            error.is_overloaded(),
            "callers need to know when to retry with backoff"
        );
        assert!(
            !error.is_connection_lost(),
            "server rejections are not connection failures"
        );
        assert!(error.to_string().contains("Server overloaded"));
    }

    /// `Result` is on every request's hot path; avoid inflating it with the largest error
    /// variant.
    #[test]
    fn the_error_type_stays_small() {
        assert!(
            std::mem::size_of::<Error>() <= 64,
            "Error grew to {} bytes; check whether a variant should be boxed",
            std::mem::size_of::<Error>()
        );
    }

    #[test]
    fn connection_failures_are_retryable_but_rejections_are_not() {
        assert!(Error::Closed { method: "x".into() }.is_connection_lost());
        assert!(
            !Error::Server(Box::new(ServerError {
                method: "x".into(),
                code: METHOD_NOT_FOUND,
                message: "nope".into(),
                data: None,
            }))
            .is_connection_lost()
        );
    }
}
