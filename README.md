# codex-client-cw

[Chinese (Simplified)](docs/zh-CN/README.md)

A Rust client for the Codex App Server. It provides typed protocol methods and delivers all server events through one receiver.

## Install

```sh
cargo add codex-client-cw
cargo add tokio --features macros,rt-multi-thread
```

The Codex App Server daemon must be running before connecting. Start it with `codex app-server daemon start`, then get its socket path with `codex app-server daemon version`.

## Example

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
                eprintln!("No handler for server request {method}; refusing it");
                drop(reply);
            }
            _ => {}
        }
    }

    Ok(())
}
```

Replace the socket path with the one reported by your daemon.

## Thread guide

For thread creation and resume, turns, event dispatch, server requests, and reconnect handling, see the [thread guide](docs/thread-guide.md). A runnable example is available at [`examples/thread.rs`](examples/thread.rs).

## Features

- Typed access to App Server requests and generated notification/request payloads.
- One event receiver for connection state, notifications, requests, and unknown messages.
- A shared connection for concurrent requests.
- `client.call` for methods added after the bundled protocol schema.
- Caller-owned message dispatch and thread subscription lifecycle.

The event receiver is unbounded and must be drained to avoid memory growth. The client reconnects but does not restore subscriptions or replay in-flight requests; applications should resubscribe after `Connected`.

The client does not make approval or sandbox policy decisions. Callers should decide which App Server capabilities to expose and construct sandbox policies on the trusted side.

## License

MIT OR Apache-2.0
