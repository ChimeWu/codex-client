# Managing threads and turns

[简体中文](zh-CN/thread-guide.md)

This guide shows how to use `codex-client-cw` for the App Server's central workflow: create or resume a thread, send user input, consume notifications, and handle server requests.

The client is intentionally a thin protocol client. It does not keep a per-thread receiver, classify notifications by thread, choose approval policies, or restore subscriptions for you. The App Server protocol types are the source of truth; your application owns dispatch and lifecycle decisions.

## Prerequisites

The App Server daemon must already be running. Obtain its socket path from `codex app-server daemon version` and pass it in the `unix:///...` form:

```sh
export CODEX_APP_SERVER_ENDPOINT="unix:///Users/me/.codex/app-server-control/app-server-control.sock"
```

The example below uses the current directory as the thread's working directory. Make sure the daemon can access that directory and that your application supplies the approval and sandbox settings appropriate for its use case.

## Connection and event stream

`build()` returns a shared RPC client and one receiver for every event on that connection:

```rust,no_run
use codex_client_cw::{AppServerClient, AppServerEvent};

let (client, mut events) = AppServerClient::builder(endpoint).build();
```

The receiver is a single-consumer, unbounded Tokio channel. Keep exactly one owner for it and drain it continuously. Since events accumulate without a capacity limit, a slow or stopped consumer can use increasing memory. RPCs and event reads are independent: awaiting an RPC does not lock the receiver.

The stream can contain:

| Event | Meaning |
| --- | --- |
| `Connected(response)` | The initialize handshake completed. Emitted after initial connection and each successful reconnect. |
| `Disconnected` | An established connection closed. The actor will retry the connection. |
| `Notification(notification)` | A notification parsed as the generated `ServerNotification` enum. It may belong to any thread associated with this connection. |
| `Request { ... }` | A server initiated request. It always has a `ReplySender`; the generated request parse is optional in practice. |
| `Unparsed(value)` | An unknown notification or a message that did not match a recognized JSON-RPC envelope. |

For a server request, `request` is `Ok(ServerRequest)` when the method and parameters match a generated variant, or `Err(raw_params)` when they do not. The `method` is provided separately in both cases. Do not treat an unknown request as disposable: the server is waiting for a response. Calling `reply.send(json_value)` answers it successfully. Dropping `reply` sends the crate's conservative refusal for known request methods and a JSON-RPC error for unknown methods.

## Start a thread and a turn

`thread/start` creates a new thread and returns its ID in `ThreadStartResponse.thread.id`. The App Server associates the creating connection with the new thread, so notifications for it arrive on the connection's event receiver. The server sends the `thread/started` notification after the `thread/start` response; use the response as the immediate source of the thread ID.

Then send a `turn/start` request with that ID and at least one `UserInput` value:

```rust,no_run
use codex_client_cw::{
    AppServerClient,
    types::{ThreadStartParams, TurnStartParams, UserInput},
};

let (client, mut events) = AppServerClient::builder(endpoint).build();
let cwd = std::env::current_dir()?.to_string_lossy().into_owned();

let started = client
    .request(ThreadStartParams {
        cwd: Some(cwd.clone()),
        ..Default::default()
    })
    .await?;
let thread_id = started.thread.id;

let turn = client
    .request(TurnStartParams {
        thread_id: thread_id.clone(),
        cwd: Some(cwd),
        input: vec![UserInput::Text {
            text: "Summarize the current project.".to_owned(),
            text_elements: None,
        }],
        ..Default::default()
    })
    .await?;
let turn_id = turn.turn.id;
```

`turn/start` returns the turn record; it does not wait for the assistant to finish. Consume the receiver to observe streaming output and completion. A complete runnable version is in [`examples/thread.rs`](../examples/thread.rs):

```sh
cargo run --example thread -- "Reply with a short greeting."
```

The example declines all server requests. Replace that behavior with explicit application logic before relying on the example for turns that may need tools or user approval.

## Dispatch notifications for multiple threads

The receiver is connection-wide. The client does not split notifications into thread-specific channels, so filter by the IDs carried in generated notification payloads. For example:

```rust,no_run
use codex_client_cw::{AppServerEvent, ServerNotification};

while let Some(event) = events.recv().await {
    match event {
        AppServerEvent::Notification(notification) => match *notification {
            ServerNotification::ItemAgentMessageDelta(delta)
                if delta.thread_id == thread_id && delta.turn_id == turn_id =>
            {
                print!("{}", delta.delta);
            }
            ServerNotification::TurnCompleted(completed)
                if completed.thread_id == thread_id && completed.turn.id == turn_id =>
            {
                println!("turn ended with {:?}", completed.turn.status);
                break;
            }
            _ => {}
        },
        AppServerEvent::Request { method, request, reply, .. } => {
            // Make a policy decision, then send a generated response as JSON.
            // Dropping `reply` refuses the request.
            eprintln!("server request: {method} ({request:?})");
            drop(reply);
        }
        AppServerEvent::Unparsed(value) => eprintln!("unparsed message: {value}"),
        AppServerEvent::Connected(_) | AppServerEvent::Disconnected => {}
    }
}
```

Notifications are not limited to `turn/completed` and text deltas. Depending on the turn, the server may send item lifecycle, plan, command, file-change, approval-resolution, usage, or other notifications. Match the generated `ServerNotification` variants your application needs and ignore the rest. Treat unknown notifications as forward-compatible data rather than connection failures.

Some notifications do not include a `turnId`; filter those by `threadId` alone. The `thread/started` notification carries the ID under `notification.thread.id`. There is no shared field accessor because the generated protocol shapes are kept intact.

## Resume a thread

To continue a persisted thread on a new connection, call `thread/resume` with its ID:

```rust,no_run
use codex_client_cw::{AppServerClient, types::ThreadResumeParams};

let (client, events) = AppServerClient::builder(endpoint).build();
let resumed = client
    .request(ThreadResumeParams {
        thread_id: saved_thread_id,
        ..Default::default()
    })
    .await?;
println!("resumed {}", resumed.thread.id);
```

Resuming also associates the current App Server connection with that thread, enabling its notifications to be delivered through this connection's receiver. Use the response for the effective settings and any history included by the server. For large or paginated histories, consult the generated `ThreadResumeParams`, `ThreadResumeResponse`, and `thread/turns/list` / `thread/items/list` types; history loading is separate from receiving live events.

There is no standalone `thread/subscribe` method in this protocol flow: start or resume the thread on the connection that should receive its events. Use `thread/unsubscribe` when that connection should stop receiving notifications for the thread. Unsubscribing does not mean that the persisted conversation is deleted.

## Reconnect and restore subscriptions

The actor reconnects the transport and emits `Connected` again, but it does not replay failed RPCs or remember thread subscriptions. Keep the IDs of threads your application still needs, and issue `thread/resume` for each one after reconnection. A failed RPC may have been processed before the connection dropped; inspect server state before retrying non-idempotent operations.

Keep one task responsible for reading `events`. It can forward typed events to other application tasks using your own channels. During reconnection, that task can observe `Connected`, resume the tracked threads, and then continue routing notifications. Avoid starting separate consumers on the same receiver: Tokio receivers have one owner, and distributing events is the application's responsibility.

For example, keep the thread IDs that this connection should follow and restore them from that same event loop:

```rust,ignore
use std::collections::HashSet;
use codex_client_cw::{AppServerEvent, types::ThreadResumeParams};

let mut tracked_threads = HashSet::from([thread_id.clone()]);
let mut restore_after_reconnect = false;

while let Some(event) = events.recv().await {
    match event {
        AppServerEvent::Disconnected => restore_after_reconnect = true,
        AppServerEvent::Connected(_) if restore_after_reconnect => {
            restore_after_reconnect = false;
            for id in tracked_threads.iter().cloned() {
                if let Err(error) = client
                    .request(ThreadResumeParams {
                        thread_id: id.clone(),
                        ..Default::default()
                    })
                    .await
                {
                    eprintln!("could not resume thread {id}: {error}");
                }
            }
        }
        AppServerEvent::Notification(notification) => {
            // Dispatch as in the previous section.
            let _ = notification;
        }
        _ => {}
    }
}
```

Add IDs to `tracked_threads` after successful `thread/start` or `thread/resume`; remove them when the application no longer needs live events. Notifications missed while disconnected are not replayed. If a turn may have finished during the outage, inspect it with `thread/read` (or the relevant history/list request) after resuming instead of waiting forever for a completion notification that was already sent.

## Continue, steer, interrupt, and finish turns

- Use `turn/start` for new user input. The response identifies the turn, and notifications report its progress.
- Use `turn/steer` to add input to an active turn when you know its ID. `expected_turn_id` is a precondition: the server rejects the request if that is no longer the active turn.
- Use `turn/interrupt` with both `thread_id` and `turn_id` to request interruption. This client does not subscribe or resume the thread as a side effect; resume first if this connection also needs the resulting notifications.
- Treat `turn/completed` as the terminal notification for the turn, and inspect `turn.status` because it can be completed, interrupted, or failed.

All of these are ordinary generated requests:

```rust,no_run
use codex_client_cw::types::{TurnInterruptParams, TurnSteerParams, UserInput};

client.request(TurnSteerParams {
    thread_id: thread_id.clone(),
    expected_turn_id: active_turn_id.clone(),
    input: vec![UserInput::Text {
        text: "Also include the test command.".to_owned(),
        text_elements: None,
    }],
    client_user_message_id: None,
}).await?;

client.request(TurnInterruptParams {
    thread_id,
    turn_id: active_turn_id,
}).await?;
```

Use `client.request` with other generated types for thread listing, reading, naming, archiving, and history pagination. `client.call(method, params)` is available when the bundled schema does not yet include a method.

## Approval and tool requests

Server requests are part of turn control flow. They can ask the client to approve command execution or file changes, collect user input, invoke a client-provided tool, or handle another protocol operation. The App Server waits for each request's response.

The application must decide what each request means and whether it is safe to approve. For a known request, match the generated `ServerRequest` variant, apply your UI and policy, serialize the corresponding generated response type with `serde_json::to_value`, and pass it to `reply.send`. If the user declines, send the protocol's refusal response. If the method is unknown or the reply handle is dropped, the crate sends a conservative fallback. Do not approve requests merely to make a sample turn complete.

## Event stream and error handling checklist

- Keep the event receiver alive and drain it for the entire period in which the connection should receive notifications.
- Track the thread IDs attached to this connection; resume them after every reconnect.
- Use both `threadId` and `turnId` when a notification carries both, since one connection can serve multiple threads and turns.
- Handle `Disconnected` and RPC errors. Do not assume an RPC that returned a disconnect error was never processed.
- Handle every `Request` by responding or deliberately dropping its `ReplySender`; otherwise the remote operation may remain blocked.
- Make approval, sandbox, capability exposure, display, and persistence choices in your application, not in this client crate.

## Reference

The request and event types are generated from the App Server protocol. For the exact current field definitions, see [`src/generated/SURFACE.md`](../src/generated/SURFACE.md) and the generated Rust types. The guide follows the App Server's thread lifecycle: start/resume associates a connection with a thread, turn methods operate on the returned IDs, and notifications are delivered on the connection-wide stream.
