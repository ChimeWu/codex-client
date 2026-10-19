// 由 protocol-codegen 生成，请勿手工修改。
// 重新生成：
//   codex app-server generate-json-schema --out <dir>
//   cargo run -p codex-client-cw --bin protocol-codegen -- <dir> src/generated

//! 从 JSON Schema 生成的协议定义。
//!
//! 这些类型覆盖 App Server 协议的全部表面：`types` 是数据类型，
//! `methods` 是每个客户端请求的方法名与参数/响应类型，`events` 是
//! 服务端通知与服务端请求。

pub mod events;
pub mod methods;
pub mod types;
