# AGENTS.md

## 项目定位

`codex-client-cw` 是 Codex App Server 的通用 Rust 客户端 crate。它通过 Unix domain socket 上的 WebSocket 连接到**已经运行**的 App Server；不负责启动、停止或监管 daemon，也不包含 Telegram 等产品层逻辑。

客户端提供通用 JSON-RPC 调用、统一的服务端事件 Receiver 和协议类型。消息分发、thread 订阅、审批策略、沙箱策略、消息展示以及哪些能力对终端用户开放，均由调用方决定。修改时应保持这个边界，不要把应用特定的产品决策放进 crate。

## 主要目录

- `src/client/`：客户端 actor、连接管理、RPC 处理和统一事件流。
- `src/generated/`：由 App Server JSON Schema 生成并纳入版本控制的协议类型和协议表面清单。
- `src/bin/protocol-codegen/`：协议类型生成器。
- `src/tests.rs`：序列化单测和需要真实 daemon 的忽略测试。
- `docs/protocol-codegen.md`：协议升级与代码生成的权威维护说明。
- `.agents/skills/protocol-codegen/`：Codex 的协议升级 skill。

## 开发约定

- 遵循 Rust 2024 edition 和 `Cargo.toml` 中声明的 MSRV（1.85）。
- 协议类型以 `src/generated/` 中的生成结果为准。升级协议或调整生成器前，先读 `docs/protocol-codegen.md`；不要手工编辑生成文件来修补生成结果。
- 改动协议生成结果时，检查 `src/generated/SURFACE.md` 和 Rust 类型文件的 diff，留意方法、响应类型及字段变化。
- 保持依赖和实现聚焦在客户端职责；新增依赖前确认标准库或现有依赖无法满足需求。
- 在公开 API 中保留 App Server 生成消息类型。不要添加基于 thread 的事件分类或便利路由；消费者自行分发统一事件流。
- actor 只负责传输、RPC 响应关联和事件投递。重连后不要恢复订阅或重放请求。
- 事件流是无界的单消费者队列。服务端请求必须带有一次性回复句柄；丢弃句柄会发送保守拒绝。
- 不添加 fake App Server、fake-server 测试支持或公开的 `testing` feature。默认测试不得依赖正在运行的 Codex daemon。
- 真实 daemon 测试必须保持 `#[ignore]`，仅连接调用者预先运行的 daemon；测试本身不得启动或停止 daemon。
- crate 文档、源码注释和日志消息使用英文；API 名称、协议方法名和代码标识符保持原样。中文翻译文档放在 `docs/zh-CN/`。

## 验证命令

按改动范围运行验证；纯文档改动无需运行 Rust 测试。

```sh
cargo fmt --all -- --check
cargo check --all-features
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

`cargo test` 运行默认测试，不会运行真实 daemon 测试。需要手动验证真实 daemon 时，先设置 `CODEX_APP_SERVER_ENDPOINT`，再显式运行：

```sh
cargo test real_daemon_ -- --ignored --nocapture
```

这些集成测试需要真实环境并可能消耗额度；不要在常规自动验证中解除忽略标记或自动启动 daemon。
