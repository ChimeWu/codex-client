# codex-client-cw

[Chinese (Simplified)](docs/zh-CN/README.md)

A Rust client for the Codex App Server. It provides typed protocol methods and streams thread events over a shared connection.

## Install

```sh
cargo add codex-client-cw
cargo add tokio --features macros,rt-multi-thread
```

The Codex App Server daemon must be running before connecting. Start it with `codex app-server daemon start`, then get its socket path with `codex app-server daemon version`.

## Example

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

Replace the socket path with the one reported by your daemon.

## Features

- Typed access to App Server requests, notifications, and server requests.
- Thread event streams from `start_thread` and `resume_thread`.
- A shared connection for concurrent requests and multiple threads.
- `client.call` for methods added after the bundled protocol schema.
- Unknown inbound protocol messages are preserved as JSON.

The client does not make approval or sandbox policy decisions. Callers should decide which App Server capabilities to expose and construct sandbox policies on the trusted side.

## License

MIT OR Apache-2.0
