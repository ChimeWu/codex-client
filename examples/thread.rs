//! Start a thread, run one turn, and consume events from the shared connection.
//!
//! Set `CODEX_APP_SERVER_ENDPOINT` to the daemon's `unix://` socket endpoint. Pass a
//! prompt as the first argument. This example declines every server request by dropping
//! its reply handle; applications should replace that behavior with their own policy.

use codex_client_cw::{
    AppServerClient, AppServerEvent, ServerNotification,
    types::{ThreadStartParams, TurnStartParams, TurnStatus, UserInput},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::var("CODEX_APP_SERVER_ENDPOINT")?;
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Reply with a short greeting.".to_owned());
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();

    let (client, mut events) = AppServerClient::builder(endpoint).build();

    // `thread/start` creates the thread and associates this connection with it.
    let started = client
        .request(ThreadStartParams {
            cwd: Some(cwd.clone()),
            ..Default::default()
        })
        .await?;
    let thread_id = started.thread.id;
    println!("thread: {thread_id}");

    let turn = client
        .request(TurnStartParams {
            thread_id: thread_id.clone(),
            cwd: Some(cwd),
            input: vec![UserInput::Text {
                text: prompt,
                text_elements: None,
            }],
            ..Default::default()
        })
        .await?;
    let turn_id = turn.turn.id;

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
                    println!("\nturn finished: {:?}", completed.turn.status);
                    if completed.turn.status != TurnStatus::Completed {
                        eprintln!("turn ended without the completed status");
                    }
                    break;
                }
                _ => {}
            },
            AppServerEvent::Request {
                method,
                request,
                reply,
                ..
            } => {
                match request {
                    Ok(request) => eprintln!("declining unhandled request {method}: {request:?}"),
                    Err(params) => eprintln!("declining unrecognized request {method}: {params}"),
                }
                // Dropping the one-shot handle sends the client's conservative refusal.
                drop(reply);
            }
            AppServerEvent::Connected(_) => eprintln!("App Server connected"),
            AppServerEvent::Disconnected => {
                return Err(std::io::Error::other(
                    "connection dropped; this example does not restore its thread subscription",
                )
                .into());
            }
            AppServerEvent::Unparsed(message) => {
                eprintln!("unparsed App Server message: {message}")
            }
        }
    }

    Ok(())
}
