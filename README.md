# codex-client-cw

[Chinese (Simplified)](docs/zh-CN/README.md)

A general-purpose Rust client for the Codex App Server. It covers the complete generated protocol surface: 105 client requests, 85 server notifications, 10 server requests, and 724 data types. All protocol types are generated from `codex app-server generate-json-schema` output.

This crate was extracted from a Telegram application and has no Telegram or product-specific dependencies. Payloads come directly from the schema. The client adds only connection state, unknown-message handling, thread/global routing, and one-shot reply channels. Presentation and approval policy belong to the caller.

## Quick start

An App Server daemon must already be running. This client never starts, stops, or supervises it; daemon lifecycle belongs to `codex app-server daemon` or an equivalent systemd/launchd unit.

```sh
codex app-server daemon start
codex app-server daemon version   # read socketPath
```

### Generic access to every protocol method

Each parameter type implements `Request` and determines the method and response types:

```rust
use codex_client_cw::{AppServerClient, types::*};

let client = AppServerClient::builder(
    "unix:///Users/me/.codex/app-server-control/app-server-control.sock",
).build();

let models = client.request(ModelListParams::default()).await?;
for model in models.data {
    println!("{} ({})", model.id, model.display_name);
}

let config = client.request(ConfigReadParams::default()).await?;
```

Methods added by the server after this crate's schema snapshot remain available through the escape hatch: `client.call("some/new/method", json!({...}))`.

### Thread streams and a shared client

`start_thread` and `resume_thread` return independent `mpsc::UnboundedReceiver<ThreadMessage>` streams. RPCs go through the shared client, and a receiver can be used across turns. The actor maintains subscription and turn state from the actual RPC method, so generic `request`/`call` and convenience methods behave consistently. `turn/start` and inline `review/start` require an active thread subscription with a live receiver. `start_turn_with` has been removed; use `request(ReviewStartParams)` for reviews. To change a reviewer, apply the configuration through a real `thread/resume` call first.

```rust
use codex_client_cw::{AppServerClient, ThreadMessage, ThreadNotification, types::*};

let repo = "/path/to/repo".to_owned();
let (thread, mut handle) = client
    .start_thread(ThreadStartParams { cwd: Some(repo.clone()), ..Default::default() })
    .await?;

let _turn = client
    .start_turn(TurnStartParams {
        thread_id: thread.thread.id.clone(),
        input: vec![serde_json::from_value(serde_json::json!({
            "type": "text", "text": "Fix the spelling errors in the README"
        }))?],
        cwd: Some(repo.clone()),
        sandbox_policy: Some(SandboxPolicy::WorkspaceWrite {
            writable_roots: Some(vec![repo.clone()]),
            network_access: Some(false),
            exclude_slash_tmp: None,
            exclude_tmpdir_env_var: None,
        }),
        approval_policy: Some(AskForApproval::Untrusted),
        approvals_reviewer: Some(ApprovalsReviewer::User),
        ..Default::default()
    })
    .await?;

while let Some(message) = handle.recv().await {
    match message {
        ThreadMessage::Notification(notification) => match *notification {
            ThreadNotification::ItemAgentMessageDelta(payload) => print!("{}", payload.delta),
            ThreadNotification::TurnCompleted(payload) => {
                println!("{:?}", payload.turn.status);
                break;
            }
            _ => {}
        },
        ThreadMessage::Request { method, request, reply, .. } => {
            println!("Server requested {method}: {request:?}");
            drop(reply); // The actor sends a conservative refusal immediately.
        }
        ThreadMessage::Unparsed(_) => {}
    }
}
```

## Connection and errors

The client opens one connection and gives it to a dedicated actor. Requests, notifications, and server requests are multiplexed over that connection. A client instance can serve multiple threads, routing each message by `threadId`. The socket, WebSocket, `initialize`, and `initialized` handshake is expensive; polling calls such as `poll_turn` should not reconnect on every iteration.

All client methods take `&self`; `build()` returns an `Arc<AppServerClient>` that can be cloned. A Tokio mutex protects the global event receiver separately, so waiting in `next_event()` does not block RPCs. Concurrent readers compete for individual messages; messages are not broadcast. The connection remains alive until the last client `Arc` is dropped. Background event tasks must hold an `Arc`; a thread receiver alone does not keep the connection alive.

RPCs have no shared deadline. If the connection is healthy, a call waits for the server response. If the connection drops, the actor fails all pending calls and does not replay them. A failure does not prove the server operation did not run; callers should check the result according to the operation's semantics. `with_connect_timeout(ms)` only limits connection establishment and initialization; callers control operation deadlines.

After a disconnect, the actor reconnects with backoff capped at 30 seconds. New requests fail immediately while reconnecting instead of waiting indefinitely. Active thread routes and resume parameters are retained, and subscriptions are restored after the new handshake. The original receivers remain usable. If a turn ends during the disconnect, the actor emits a `turn/completed` message after reconnect. The global stream emits `GlobalMessage::Disconnected` and `GlobalMessage::Connected` in order.

Errors are structured, and server rejection codes and messages are preserved:

```rust
match client.request(ModelListParams::default()).await {
    Ok(models) => { /* ... */ }
    Err(error) if error.is_overloaded() => {
        tokio::time::sleep(reconnect_delay(1, 30_000)).await;
    }
    Err(error) if error.is_method_not_found() => { /* server is older than this crate */ }
    Err(error) => eprintln!("{error}"),
}
```

The handshake can opt out of noisy notifications. In practice this can remove more than ten notifications per turn:

```rust
let client = AppServerClient::builder(endpoint)
    .opt_out_notifications([
        "mcpServer/startupStatus/updated",
        "account/rateLimits/updated",
    ])
    .build();
```

## Three protocol rules to know

**1. Notifications go only to connections subscribed to that thread.** `thread/start` and `thread/resume` subscribe the connection that made the request. A `turn/start` on another connection can succeed and run, while that connection receives no deltas, approvals, or completion events. Keep the receiver returned by `thread/start` or `thread/resume` before starting a turn. Dropping it gives up that thread's message route. One connection can subscribe to many threads, and active subscriptions are restored after reconnect.

Use `subscribe` when you only need to operate on a thread without taking ownership of its message stream, such as interrupting a turn from another task. It joins the thread without attaching a receiver:

```rust
client.subscribe(ThreadResumeParams { thread_id: thread_id.clone(), ..Default::default() }).await?;
client.request(TurnInterruptParams { thread_id, turn_id }).await?;
```

**2. Every server request needs a reply.** The server can send JSON-RPC requests and block while waiting. If a method is unknown or no turn claims it, the client sends an explicit refusal, or a JSON-RPC error when it cannot answer at all (for example, `attestation/generate` requires real credentials). Response shapes are generated from the schema rather than handwritten JSON.

Each request carries a one-shot reply sender. Sending a response value answers successfully; dropping the sender makes the actor send a conservative refusal immediately. There is no reply timeout: holding the sender means the server continues to wait.

Requests are passed through unchanged. `item/commandExecution/requestApproval` may be answered with a simple approval decision, `item/tool/call` requires a tool result, and `attestation/generate` requires a real credential. They have separate variants and response types in `ServerRequest`. The caller decides what to ask users and how to present it. Server request IDs can be `0`; code that treats IDs as booleans will lose such requests.

**3. Usage is not part of `turn/completed`.** The protocol's `Turn` has no usage fields. Token usage arrives in the separate `thread/tokenUsage/updated` notification, which includes `modelContextWindow`. Keep the latest notification if it is needed at turn completion. Do not infer context-window size from the model name; `model/list` does not provide it. `last` is the latest request's usage, while `total` is cumulative for the thread.

## Safety boundary

This client does not make policy decisions. Methods such as `thread/shellCommand`, `command/exec*`, `fs/writeFile`, `config/value/write`, and plugin installation are available. Each caller decides which capabilities to expose to its users.

Two protocol constraints matter:

- `SandboxPolicy` determines what the agent may change. Construct it on the trusted server side; never let untrusted input assemble it.
- `ApprovalsReviewer` and `approvalPolicy` must be paired: `user` with `untrusted`, and `auto_review` with `on-request`. Pairing `user` with `on-request` lets the model decide which commands are trusted, so many requests will never reach your application.

## Generated protocol types

Files under `src/generated/` are generated by this crate's `protocol-codegen` binary and checked in. Users of the crate do not need the Codex CLI, and code generation does not run during the build.

- `types.rs`: all data types (724)
- `methods.rs`: `Request` implementations for 105 client requests
- `events.rs`: 85 `ServerNotification` variants and 10 `ServerRequest` variants
- `SURFACE.md`: a protocol inventory for reviewing method-level changes

After upgrading Codex, regenerate the schema and types:

```sh
codex app-server generate-json-schema --out /tmp/codex-schema
cargo run --bin protocol-codegen -- /tmp/codex-schema src/generated
cargo test --all-features
```

Read the [protocol-codegen maintenance guide](docs/protocol-codegen.md) before upgrading. It describes the full workflow, schema-to-Rust mapping, and troubleshooting steps. A Chinese version is available [here](docs/zh-CN/protocol-codegen.md).

Generated enums include only values known at generation time. Unknown inbound messages are returned as `Event::Unparsed` with their original JSON, so protocol changes do not break the connection and are not silently treated as known values.

## Testing

```sh
cargo test
```

The default tests cover parameter serialization and client behavior without requiring a daemon. Real-daemon integration tests are ignored by default and never start or stop a daemon. They require a running daemon and can consume quota:

```sh
export CODEX_APP_SERVER_ENDPOINT="unix://$HOME/.codex/app-server-control/app-server-control.sock"
export CODEX_APP_SERVER_TEST_CWD="/path/to/some/repo"   # optional; defaults to the current directory
cargo test real_daemon_ -- --ignored --nocapture
```

## Version

Types are generated from the Codex CLI **0.163.0** schema and have been tested against App Server **0.163.0**.

## License

MIT OR Apache-2.0
