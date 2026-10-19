//! 客户端的错误类型。
//!
//! 库不该把失败原因拍成一句字符串：调用方要能分辨「连接断了，重连就好」、
//! 「服务端明确拒绝了这个请求」、「服务端过载，退避重试」和「响应解析不了，
//! 大概是协议变了」——这四种的处理方式完全不同。

use crate::envelope::ErrorObject;
use serde_json::Value;
use std::fmt;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, Error>;

/// JSON-RPC 的「服务端过载」，官方文档要求客户端退避重试。
pub const SERVER_OVERLOADED: i64 = -32001;
/// JSON-RPC 的「方法未实现」。
pub const METHOD_NOT_FOUND: i64 = -32601;

#[derive(Debug)]
pub enum Error {
    /// 连不上，或 WebSocket 握手没成功。
    Connect {
        endpoint: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// 连接在用的过程中断了。重连通常就能恢复。
    Transport {
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// 等响应的过程中对端关闭了连接。
    Closed { method: String },
    /// 初始化握手超过了连接等待上限；普通 RPC 不设统一超时。
    Timeout { method: String, after: Duration },
    /// 服务端返回了 JSON-RPC `error`。**结构保留**，不要拍成字符串。
    ///
    /// 装箱是因为 `Result` 在每个请求的热路径上：让最大的失败变体决定
    /// `Result` 的宽度，成功路径也要跟着一起变胖。
    Server(Box<ServerError>),
    /// 参数序列化不了。基本只会在调用方塞了非法 JSON 时出现。
    Encode { source: serde_json::Error },
    /// 响应解析不成预期类型——通常意味着协议变了，该重新生成类型了。
    Decode {
        method: String,
        source: serde_json::Error,
    },
    /// 协议语义上的问题，例如响应缺了必须有的字段。
    Protocol(String),
}

/// 服务端明确拒绝一个请求时给出的信息。
#[derive(Clone, Debug, PartialEq)]
pub struct ServerError {
    pub method: String,
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl Error {
    /// 服务端拒绝的详情。
    pub fn server(&self) -> Option<&ServerError> {
        match self {
            Self::Server(error) => Some(error),
            _ => None,
        }
    }

    /// 这个错误是否值得换一条连接重试。
    ///
    /// 连接层面的失败可以重试；服务端明确的拒绝不该重试，重试只会再被拒一次。
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, Self::Transport { .. } | Self::Closed { .. })
    }

    /// 服务端在喊过载，应当退避后重试（见 `reconnect_delay`）。
    pub fn is_overloaded(&self) -> bool {
        self.code() == Some(SERVER_OVERLOADED)
    }

    /// 服务端不认识这个方法——多半是本客户端比服务端新。
    pub fn is_method_not_found(&self) -> bool {
        self.code() == Some(METHOD_NOT_FOUND)
    }

    /// 服务端返回的错误码。
    pub fn code(&self) -> Option<i64> {
        self.server().map(|error| error.code)
    }

    pub(crate) fn transport(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Transport {
            source: source.into(),
        }
    }

    /// 把一个 JSON-RPC `error` 对象记到某个方法头上。
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
                write!(f, "连接 App Server（{endpoint}）失败：{source}")
            }
            Self::Transport { source } => write!(f, "App Server 连接中断：{source}"),
            Self::Closed { method } => {
                write!(f, "等待 {method} 的响应时 App Server 关闭了连接")
            }
            Self::Timeout { method, after } => {
                write!(f, "{method} 超过 {:?} 未返回", after)
            }
            Self::Server(error) => write!(
                f,
                "App Server 拒绝 {}：[{}] {}",
                error.method, error.code, error.message
            ),
            Self::Encode { source } => write!(f, "序列化请求参数失败：{source}"),
            Self::Decode { method, source } => {
                write!(f, "解析 {method} 的响应失败（协议可能已变）：{source}")
            }
            Self::Protocol(detail) => write!(f, "协议错误：{detail}"),
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
        assert!(error.is_overloaded(), "调用方要能据此决定退避重试");
        assert!(!error.is_connection_lost(), "服务端拒绝不该被当成连接问题");
        assert!(error.to_string().contains("Server overloaded"));
    }

    /// `Result` 在每个请求的热路径上，别让它被最大的失败变体撑胖。
    #[test]
    fn the_error_type_stays_small() {
        assert!(
            std::mem::size_of::<Error>() <= 64,
            "Error 变大到了 {} 字节，检查是不是有变体该装箱了",
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
