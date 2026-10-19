//! A Rust client for the Codex App Server.
//!
//! Connects to an **already-running** App Server over a WebSocket on a Unix domain
//! socket. The generated protocol surface includes 105 client requests, 85 server
//! notifications, 10 server requests, and 724 data types; see [`generated`].
//!
//! # API layers
//!
//! Protocol payloads use schema-generated types and are passed through unchanged. The
//! crate adds connection lifecycle events, unparsed-message handling, thread/global
//! routing, and one-shot reply channels. Product choices such as notification display,
//! approval UX, and usage calculations belong to the caller.
//!
//! The generic [`AppServerClient::request`] method can call every generated method. Each
//! parameter type implements [`Request`] and determines both the method and response type:
//!
//! ```no_run
//! # use codex_client_cw::{AppServerClient, types::*};
//! # async fn demo(client: &AppServerClient) -> anyhow::Result<()> {
//! let models = client.request(ModelListParams::default()).await?;
//! for model in models.data {
//!     println!("{} ({})", model.id, model.display_name);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Thread subscription methods return independent event receivers, while RPCs share the
//! client. Receivers can be used across turns. The event-consuming task should hold a
//! client `Arc` to keep the connection alive.
//!
//! ```no_run
//! use codex_client_cw::{AppServerClient, ThreadMessage, ThreadNotification, types::*};
//!
//! # async fn demo() -> anyhow::Result<()> {
//! let repo = "/path/to/repo".to_owned();
//! let client = AppServerClient::builder(
//!     "unix:///Users/me/.codex/app-server-control/app-server-control.sock",
//! ).build();
//! let (thread, mut events) = client
//!     .start_thread(ThreadStartParams { cwd: Some(repo.clone()), ..Default::default() })
//!     .await?;
//! let _turn = client.start_turn(TurnStartParams {
//!     thread_id: thread.thread.id.clone(),
//!     input: vec![serde_json::from_value(serde_json::json!({
//!         "type": "text", "text": "Fix the spelling errors in the README"
//!     }))?],
//!     cwd: Some(repo.clone()),
//!     sandbox_policy: Some(SandboxPolicy::WorkspaceWrite {
//!         writable_roots: Some(vec![repo.clone()]),
//!         network_access: Some(false),
//!         exclude_slash_tmp: None,
//!         exclude_tmpdir_env_var: None,
//!     }),
//!     approval_policy: Some(AskForApproval::Untrusted),
//!     approvals_reviewer: Some(ApprovalsReviewer::User),
//!     ..Default::default()
//! }).await?;
//!
//! while let Some(message) = events.recv().await {
//!     match message {
//!         ThreadMessage::Notification(notification) => match *notification {
//!             ThreadNotification::ItemAgentMessageDelta(payload) => print!("{}", payload.delta),
//!             ThreadNotification::TurnCompleted(payload) => {
//!                 println!("{:?}", payload.turn.status);
//!                 break;
//!             }
//!             _ => {}
//!         },
//!         ThreadMessage::Request { method, request, reply, .. } => {
//!             println!("Server requested {method}: {request:?}");
//!             drop(reply); // Dropping the sender makes the actor refuse conservatively.
//!         }
//!         ThreadMessage::Unparsed(_) => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Connection and errors
//!
//! One actor owns one connection and multiplexes RPCs, notifications, and server requests.
//! Multiple threads can share the connection; events are routed by `threadId`. The
//! connection reconnects with backoff, restores active subscriptions, and fails pending
//! requests without replaying them. A failed call does not prove the server skipped the
//! operation. RPCs have no shared timeout; `with_connect_timeout` applies only to
//! connection establishment and initialization.
//!
//! Errors are structured. Server error codes and messages are preserved, and callers can
//! distinguish connection failures, server rejections, overload, and response decoding
//! failures.
//!
//! # Important protocol behavior
//!
//! - Notifications go only to connections subscribed to their thread. Keep the receiver
//!   returned by `start_thread` or `resume_thread` before starting a turn.
//! - Server requests block until answered. Send a generated response through
//!   [`ReplySender`], or drop the sender to trigger a conservative refusal.
//! - Token usage arrives in `thread/tokenUsage/updated`, not in `turn/completed`.
//!
//! # Safety boundary
//!
//! This crate does not choose approval or sandbox policy. Callers must construct
//! [`types::SandboxPolicy`] from trusted inputs and pair
//! [`types::ApprovalsReviewer`] with the matching `approvalPolicy`.
//!
//! # Protocol version
//!
//! Types are generated from Codex CLI 0.163.0 and have been tested against App Server
//! 0.163.0. Unknown inbound messages are returned through
//! [`GlobalMessage::Unparsed`] or [`ThreadMessage::Unparsed`] without terminating the
//! connection. See `docs/protocol-codegen.md` for the upgrade process.

pub mod client;
pub mod error;
pub mod generated;
// Envelope and protocol details are implementation, not public contract. The crate
// commits only to the exports below and the methods on `AppServerClient`; exposing parsing,
// classification, or reply construction would make internal refactors breaking changes.
mod envelope;
mod protocol;

#[cfg(test)]
mod tests;

pub use client::inbound::{GlobalMessage, ReplySender, ThreadMessage};
pub use client::{AppServerClient, AppServerClientBuilder};
pub use error::{Error, Result, ServerError};
pub use generated::events::{ServerNotification, ServerRequest};
// `requests` exposes `Request` and parameter types for nullary methods (such as
// `requests::AccountLogoutParams`). Parameter types for methods with parameters live in
// `types`.
pub use generated::methods::{self as requests, Request};
pub use generated::types;
pub use protocol::{
    GlobalNotification, GlobalRequest, ThreadNotification, ThreadRequest, reconnect_delay,
};
