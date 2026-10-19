//! A typed Rust client for the Codex App Server.
//!
//! The App Server must already be running. Create a client with its Unix socket endpoint,
//! then use generated parameter types to call protocol methods:
//!
//! ```no_run
//! # use codex_client_cw::{AppServerClient, types::ModelListParams};
//! # async fn example() -> codex_client_cw::Result<()> {
//! let client = AppServerClient::builder(
//!     "unix:///Users/me/.codex/app-server-control/app-server-control.sock",
//! ).build();
//! let models = client.request(ModelListParams::default()).await?;
//! for model in models.data {
//!     println!("{} ({})", model.id, model.display_name);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! `start_thread` and `resume_thread` return a receiver for that thread's events. Keep the
//! receiver while handling a turn, and answer server requests through [`ReplySender`].
//! Dropping a reply handle sends a conservative refusal.
//!
//! The client reconnects and restores active subscriptions, but it does not replay
//! in-flight requests. A failed request may already have been processed by the server.
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
