# codex-client-cw

[English](../../README.md)

用于连接 Codex App Server 的 Rust 客户端，提供类型化协议方法，并通过单一 Receiver 投递全部服务端事件。

## 安装

```sh
cargo add codex-client-cw
cargo add tokio --features macros,rt-multi-thread
```

连接前需要先启动 Codex App Server daemon。运行 `codex app-server daemon start` 启动，再用 `codex app-server daemon version` 获取 socket 路径。

## 示例

```rust
use codex_client_cw::{AppServerClient, AppServerEvent, types::ModelListParams};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (client, mut events) = AppServerClient::builder(
        "unix:///Users/me/.codex/app-server-control/app-server-control.sock",
    )
    .build();

    let models = client.request(ModelListParams::default()).await?;
    for model in models.data {
        println!("{} ({})", model.id, model.display_name);
    }

    while let Some(event) = events.recv().await {
        match event {
            AppServerEvent::Notification(notification) => println!("{notification:?}"),
            AppServerEvent::Request { method, reply, .. } => {
                eprintln!("没有处理服务端请求 {method}，将拒绝该请求");
                drop(reply);
            }
            _ => {}
        }
    }

    Ok(())
}
```

请将 socket 路径替换为 daemon 返回的实际路径。

## 功能

- 类型化访问 App Server 请求、通知和服务端请求载荷。
- 单一事件 Receiver 接收连接状态、通知、请求和未知消息。
- 共享连接支持并发请求。
- 可通过 `client.call` 访问协议 schema 更新后新增的方法。
- 消息分发和线程订阅生命周期由调用方管理。

事件 Receiver 使用无界队列，调用方需要持续读取以避免内存增长。客户端会自动重连，但不会恢复订阅或重放断线中的请求；应用应在收到 `Connected` 后自行重新订阅。

客户端不会替调用方制定审批或沙箱策略。调用方应决定向用户开放哪些 App Server 能力，并在可信任的一侧构造沙箱策略。

## 许可证

MIT OR Apache-2.0
