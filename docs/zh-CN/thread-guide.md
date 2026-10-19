# 管理 thread 和 turn

[English](../thread-guide.md)

本指南介绍 `codex-client-cw` 的核心流程：创建或恢复 thread、发送用户输入、消费通知，以及处理服务端请求。

客户端刻意保持为轻量协议客户端。它不会为每个 thread 单独创建 Receiver，不会按 thread 分类通知，不会替调用方选择审批策略，也不会在重连后恢复订阅。App Server 协议类型是事实来源；消息分发和生命周期决策由应用负责。

## 前置条件

连接前需要先运行 App Server daemon。可用 `codex app-server daemon version` 获取 socket 路径，并使用 `unix:///...` 格式：

```sh
export CODEX_APP_SERVER_ENDPOINT="unix:///Users/me/.codex/app-server-control/app-server-control.sock"
```

下面的示例使用当前目录作为 thread 的工作目录。请确认 daemon 能访问该目录，并根据应用场景配置合适的审批和沙箱设置。

## 连接与事件流

`build()` 返回共享 RPC client，以及该连接上所有事件共用的一个 Receiver：

```rust,ignore
use codex_client_cw::{AppServerClient, AppServerEvent};

let (client, mut events) = AppServerClient::builder(endpoint).build();
```

Receiver 是单消费者的 Tokio 无界通道。应由一个任务持有并持续读取。队列没有容量上限，消费速度低于事件产生速度时会持续占用更多内存。RPC 和事件读取相互独立：等待 RPC 不会锁住 Receiver。

事件包括：

| 事件 | 含义 |
| --- | --- |
| `Connected(response)` | 初始化握手完成。首次连接以及每次重连成功后都会发送。 |
| `Disconnected` | 已建立的连接断开，actor 会尝试重连。 |
| `Notification(notification)` | 按生成的 `ServerNotification` 枚举解析的通知，可能属于当前连接关联的任意 thread。 |
| `Request { ... }` | 服务端发起且必须回复的请求，始终携带 `ReplySender`。 |
| `Unparsed(value)` | 未知通知，或未匹配已知 JSON-RPC envelope 的消息。 |

服务端请求的 `request` 在方法和参数符合已生成类型时为 `Ok(ServerRequest)`；否则为 `Err(raw_params)`。两种情况都会提供 `method`。不要丢弃未知请求：服务端正在等待回复。调用 `reply.send(json_value)` 会发送成功结果；丢弃 `reply` 会对已知方法发送保守拒绝，对未知方法发送 JSON-RPC 错误。

## 创建 thread 并启动 turn

`thread/start` 创建 thread，并在 `ThreadStartResponse.thread.id` 返回 ID。App Server 会将发起请求的连接关联到该 thread，因此该 thread 的通知会进入此连接的事件 Receiver。服务端会先返回 `thread/start` 响应，再发送 `thread/started` 通知；应使用 RPC 响应获取 thread ID。

随后用该 ID 调用 `turn/start`，并提供至少一个 `UserInput`：

```rust,ignore
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

`turn/start` 返回 turn 记录，但不会等待 assistant 完成。要观察流式输出和完成状态，应继续读取 Receiver。可运行的完整示例见 [`examples/thread.rs`](../../examples/thread.rs)：

```sh
cargo run --example thread -- "Reply with a short greeting."
```

该示例会拒绝所有服务端请求。若 turn 可能需要工具或用户审批，请先用明确的应用策略替换这个行为。

## 分发多个 thread 的通知

Receiver 是连接级别的。客户端不会将通知拆分到不同 thread 的 channel，因此需要按生成通知载荷中的 ID 过滤。例如：

```rust,ignore
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
                println!("turn 结束状态：{:?}", completed.turn.status);
                break;
            }
            _ => {}
        },
        AppServerEvent::Request { method, request, reply, .. } => {
            eprintln!("服务端请求：{method} ({request:?})");
            // 先执行应用自己的策略，再发送生成响应类型对应的 JSON。
            // 丢弃 `reply` 会拒绝请求。
            drop(reply);
        }
        AppServerEvent::Unparsed(value) => eprintln!("无法解析的消息：{value}"),
        AppServerEvent::Connected(_) | AppServerEvent::Disconnected => {}
    }
}
```

通知不只有 `turn/completed` 和文本增量。服务端还可能发送 item 生命周期、计划、命令、文件变更、审批结果、用量等通知。按需匹配生成的 `ServerNotification` 变体，其余变体可以忽略。未知通知应作为兼容未来协议的数据处理，不应导致连接失败。

部分通知不包含 `turnId`，这类通知只能按 `threadId` 过滤。`thread/started` 中的 ID 位于 `notification.thread.id`。生成协议结构保持原样，因此没有统一的 ID 访问器。

## 恢复 thread

在新连接上继续已有 thread 时，使用 thread ID 调用 `thread/resume`：

```rust,ignore
use codex_client_cw::{AppServerClient, types::ThreadResumeParams};

let (client, events) = AppServerClient::builder(endpoint).build();
let resumed = client
    .request(ThreadResumeParams {
        thread_id: saved_thread_id,
        ..Default::default()
    })
    .await?;
println!("已恢复 {}", resumed.thread.id);
```

恢复操作会将当前 App Server 连接关联到该 thread，之后该 thread 的通知会通过此连接的 Receiver 投递。响应中包含服务端生效的设置和按请求加载的历史记录。大型或分页历史记录请查看生成的 `ThreadResumeParams`、`ThreadResumeResponse` 以及 `thread/turns/list`、`thread/items/list` 类型；历史加载与实时事件接收是两个不同流程。

这里没有独立的 `thread/subscribe` 步骤：应在需要接收通知的连接上启动或恢复 thread。若要让该连接停止接收某个 thread 的通知，可调用 `thread/unsubscribe`。取消订阅不会删除已保存的对话。

## 重连与恢复订阅

actor 会重连传输层，并再次发出 `Connected`，但不会重放失败的 RPC，也不会记住 thread 订阅。应用应跟踪仍需接收通知的 thread ID，并在重连后逐个调用 `thread/resume`。连接断开前服务端可能已经处理了某个 RPC；重试非幂等操作前应先检查服务端状态。

由一个任务负责读取 `events`，并通过应用自己的 channel 将类型化事件分发给其他任务。重连时，该任务可以观察 `Connected`、恢复已跟踪的 thread，然后继续分发通知。不要对同一个 Receiver 启动多个消费者；Receiver 只有一个所有者，事件分发由应用负责。

例如，在同一个事件循环中维护需要接收事件的 thread ID，并在重连后恢复：

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
                    eprintln!("无法恢复 thread {id}：{error}");
                }
            }
        }
        AppServerEvent::Notification(notification) => {
            // 按上一节的方式分发。
            let _ = notification;
        }
        _ => {}
    }
}
```

成功调用 `thread/start` 或 `thread/resume` 后，将 ID 加入 `tracked_threads`；应用不再需要实时事件时将其移除。断线期间错过的通知不会重放。如果 turn 可能在断线期间完成，恢复后应通过 `thread/read` 或相应的历史/list 请求检查状态，不要一直等待已经发出过的完成通知。

## 继续、追加输入、中断和结束 turn

- 用 `turn/start` 发送新的用户输入。响应包含 turn ID，后续通知报告进展。
- 已知活动 turn 的 ID 时，用 `turn/steer` 追加输入。`expected_turn_id` 是前置条件：如果该 ID 不再是当前活动 turn，服务端会拒绝请求。
- 用 `thread_id` 和 `turn_id` 调用 `turn/interrupt` 请求中断。此 client 不会因此自动订阅或恢复 thread；若该连接还需要接收中断结果，应先恢复 thread。
- 将 `turn/completed` 作为 turn 的终态通知，并检查 `turn.status`，因为状态可能是完成、中断或失败。

这些操作都可以使用生成的请求类型：

```rust,ignore
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

thread 列表、读取、命名、归档和历史分页也使用对应生成类型调用 `client.request`。若协议类型尚未包含某个方法，可使用 `client.call(method, params)`。

## 审批和工具请求

服务端请求属于 turn 控制流程，可能要求客户端审批命令或文件变更、收集用户输入、调用客户端提供的工具或处理其他协议操作。App Server 会等待每个请求的回复。

应用需要解释每个请求并判断是否安全。对于已知请求，匹配生成的 `ServerRequest` 变体，执行自己的 UI 和策略，将对应的生成响应类型用 `serde_json::to_value` 序列化，再传给 `reply.send`。用户拒绝时，发送协议定义的拒绝响应。遇到未知方法或丢弃回复句柄时，crate 会发送保守回退响应。不要为了让示例 turn 顺利结束而无条件批准请求。

## 事件流和错误处理清单

- 在连接需要接收通知期间，持续持有并读取事件 Receiver。
- 跟踪关联到该连接的 thread ID；每次重连后恢复这些 thread。
- 通知同时携带 `threadId` 和 `turnId` 时，按两个 ID 一起过滤；同一个连接可能承载多个 thread 和 turn。
- 处理 `Disconnected` 和 RPC 错误。RPC 因断线返回错误，不代表服务端一定没有处理请求。
- 回复每一个 `Request`，或有意丢弃其 `ReplySender`；否则远端操作可能一直等待。
- 在应用中决定审批、沙箱、能力开放、显示和持久化策略，不要将这些策略放进 client crate。

## 参考

请求和事件类型由 App Server 协议生成。当前字段定义见 [`src/generated/SURFACE.md`](../../src/generated/SURFACE.md) 和生成的 Rust 类型。本指南遵循 App Server 的 thread 生命周期：start/resume 将连接关联到 thread，turn 方法操作返回的 ID，通知则通过连接级事件流投递。
