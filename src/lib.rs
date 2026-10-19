//! A Rust client for the Codex App Server.
//!
//! The App Server must already be running. The client returns one receiver for every
//! server event; applications own message dispatch and any thread subscription lifecycle.
//!
//! ```no_run
//! # use codex_client_cw::{AppServerClient, types::ModelListParams};
//! # async fn example() -> codex_client_cw::Result<()> {
//! let (client, mut events) = AppServerClient::builder(
//!     "unix:///Users/me/.codex/app-server-control/app-server-control.sock",
//! ).build();
//! let models = client.request(ModelListParams::default()).await?;
//! for model in models.data {
//!     println!("{} ({})", model.id, model.display_name);
//! }
//! while let Some(event) = events.recv().await {
//!     match event {
//!         codex_client_cw::AppServerEvent::Notification(notification) => {
//!             // Dispatch by generated notification variant, including threadId fields.
//!             println!("notification: {notification:?}");
//!         }
//!         codex_client_cw::AppServerEvent::Request { reply, .. } => drop(reply),
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Server requests arrive with a one-shot [`ReplySender`]. Dropping it sends a conservative
//! refusal. Subscribe and unsubscribe by using the generated request types and process all
//! notifications from the returned receiver.
//!
//! The client reconnects but does not replay in-flight requests or restore subscriptions.
//! A failed request may already have been processed by the server. The unbounded event
//! receiver must be drained to avoid unbounded memory growth.
//! Connection setup has a timeout; RPCs do not have a shared deadline.
//!
//! Callers choose which App Server capabilities to expose and are responsible for
//! constructing [`types::SandboxPolicy`] from trusted input and pairing
//! [`types::ApprovalsReviewer`] with a compatible approval policy.

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

pub use client::inbound::{AppServerEvent, ReplySender};
pub use client::{AppServerClient, AppServerClientBuilder};
pub use error::{Error, Result, ServerError};
pub use generated::events::{ServerNotification, ServerRequest};
// `requests` exposes `Request` and parameter types for nullary methods (such as
// `requests::AccountLogoutParams`). Parameter types for methods with parameters live in
// `types`.
pub use generated::methods::{self as requests, Request};
pub use generated::types;
pub use protocol::reconnect_delay;
