//! JSON-RPC 信封：入站四形态与出站四形态。
//!
//! 对齐 codex 仓库里 app-server 自己的 `JSONRPCMessage`——请求、通知、响应、
//! 错误四种形状是**方向无关**的：服务端发给我们的请求和我们发给服务端的请求
//! 是同一个形状，我们应答服务端请求和服务端应答我们的请求也是同一个形状。
//!
//! 这里是全 crate **唯一**拼装和拆解这四种形状的地方。别处不许再出现
//! `json!({"id": .., "method": ..})`：手拼一次就是一处没有编译期约束的协议实现。

use crate::generated::types::RequestId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 的「方法未实现」。
pub(crate) const METHOD_NOT_SUPPORTED: i64 = -32601;
/// JSON-RPC 的「内部错误」。
pub(crate) const INTERNAL_ERROR: i64 = -32603;

/// 一条入站消息。
///
/// 顺序即匹配优先级（`untagged`）：响应要有 `result`、错误要有 `error`、请求
/// 要有 `id` 和 `method`，都不满足才是通知。四种都对不上时反序列化失败，调用方
/// 按「读不懂的消息」原样交出，绝不让它拖垮连接。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Incoming {
    /// 我们发出的请求得到了成功响应。
    Response { id: RequestId, result: Value },
    /// 我们发出的请求被服务端拒绝。
    Error { id: RequestId, error: ErrorObject },
    /// 服务端反向发起的请求。**必须应答**，否则它会一直阻塞等待。
    Request {
        id: RequestId,
        method: String,
        #[serde(default)]
        params: Value,
    },
    /// 服务端通知，不需要应答。
    Notification {
        method: String,
        #[serde(default)]
        params: Value,
    },
}

/// 一条出站消息。
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

/// JSON-RPC `error` 对象。
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

/// 对一条服务端请求的应答。
///
/// `Error` 用于本客户端**无法**代为应答的请求——编造一个 result 会让服务端拿到
/// 假数据，明确报错才是诚实的。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Reply {
    Result(Value),
    Error(ErrorObject),
}

impl Reply {
    /// 用生成的响应类型构造一条成功应答。
    ///
    /// **这是构造应答的唯一入口**：形状由 schema 决定，不由手写的 `json!` 决定。
    /// 生成的响应类型里没有非字符串键的映射，序列化实际上不会失败；真失败了也
    /// 必须给出一条应答，否则服务端永久等待。
    pub(crate) fn ok<R: Serialize>(response: R) -> Self {
        match serde_json::to_value(response) {
            Ok(result) => Self::Result(result),
            Err(source) => Self::Error(ErrorObject::new(
                INTERNAL_ERROR,
                format!("client failed to encode its response: {source}"),
            )),
        }
    }

    /// 本客户端答不了这个方法。
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
        serde_json::from_value(value).expect("这四种形态都该认得")
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
        // 四种都不是：交给调用方按「读不懂」处理，而不是猜。
        assert!(serde_json::from_value::<Incoming>(json!({"x":1})).is_err());
    }

    #[test]
    fn server_request_id_zero_is_still_a_request() {
        // 实测服务端请求 id 可以是 0：任何用真值判断 id 的实现都会在这里
        // 静默丢掉一次审批，症状是 turn 永久卡住。
        let Incoming::Request { id, method, .. } =
            parse(json!({"id":0,"method":"item/commandExecution/requestApproval"}))
        else {
            panic!("id 为 0 的服务端请求必须仍然是请求");
        };
        assert_eq!(id, RequestId::Integer(0));
        assert_eq!(method, "item/commandExecution/requestApproval");
    }

    #[test]
    fn requests_without_params_are_accepted() {
        let Incoming::Notification { params, .. } = parse(json!({"method":"ping"})) else {
            panic!("没有 params 的通知也是通知");
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
