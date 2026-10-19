# codex-client-cw

[English](../../README.md)

用于连接 Codex App Server 的 Rust 客户端，提供类型化协议方法，并通过共享连接传递线程事件流。

## 安装

```sh
cargo add codex-client-cw
cargo add tokio --features macros,rt-multi-thread
```

连接前需要先启动 Codex App Server daemon。运行 `codex app-server daemon start` 启动，再用 `codex app-server daemon version` 获取 socket 路径。

## 示例

```rust
use codex_client_cw::{AppServerClient, types::ModelListParams};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = AppServerClient::builder(
        "unix:///Users/me/.codex/app-server-control/app-server-control.sock",
    )
    .build();

    let models = client.request(ModelListParams::default()).await?;
    for model in models.data {
        println!("{} ({})", model.id, model.display_name);
    }

    Ok(())
}
```

请将 socket 路径替换为 daemon 返回的实际路径。

## 功能

- 类型化访问 App Server 请求、通知和服务端请求。
- 通过 `start_thread` 和 `resume_thread` 获取线程事件流。
- 共享连接支持并发请求和多个线程。
- 可通过 `client.call` 访问协议 schema 更新后新增的方法。
- 未知的入站协议消息会以 JSON 形式保留。

客户端不会替调用方制定审批或沙箱策略。调用方应决定向用户开放哪些 App Server 能力，并在可信任的一侧构造沙箱策略。

## 许可证

MIT OR Apache-2.0
