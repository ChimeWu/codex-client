//! A shared client sends RPC commands to one connection-owning actor.

pub(crate) mod actor;
pub mod inbound;

use crate::error::{Error, Result};
use crate::generated::methods::Request;
use crate::generated::types::{ClientInfo, InitializeCapabilities, InitializeParams};
use actor::{Actor, Command};
use inbound::AppServerEvent;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 3_000;
const COMMAND_CAPACITY: usize = 128;

/// A client connected to an already-running Codex App Server.
pub struct AppServerClient {
    endpoint: String,
    commands: mpsc::Sender<Command>,
}

/// Builder for [`AppServerClient`].
pub struct AppServerClientBuilder {
    endpoint: String,
    initialize: InitializeParams,
    connect_timeout: Duration,
}

impl AppServerClientBuilder {
    /// Create a builder for an App Server Unix socket endpoint.
    ///
    /// Use the `unix:///absolute/path/to/app-server-control.sock` form.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            initialize: InitializeParams {
                client_info: ClientInfo {
                    name: env!("CARGO_PKG_NAME").to_owned(),
                    title: Some("codex-client-cw".to_owned()),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                },
                capabilities: None,
            },
            connect_timeout: Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS),
        }
    }

    /// Set the client identity sent in the initialization handshake.
    pub fn with_client_info(mut self, client_info: ClientInfo) -> Self {
        self.initialize.client_info = client_info;
        self
    }

    /// Set the capabilities declared in the initialization handshake.
    pub fn with_capabilities(mut self, capabilities: InitializeCapabilities) -> Self {
        self.initialize.capabilities = Some(capabilities);
        self
    }

    /// Ask the server to omit the listed notifications from this connection.
    pub fn opt_out_notifications<I, S>(mut self, methods: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let methods = methods.into_iter().map(Into::into).collect();
        self.initialize
            .capabilities
            .get_or_insert_with(Default::default)
            .opt_out_notification_methods = Some(methods);
        self
    }

    /// Set the timeout for connecting and completing the initialization handshake.
    pub fn with_connect_timeout(mut self, connect_timeout_ms: u64) -> Self {
        self.connect_timeout = Duration::from_millis(connect_timeout_ms);
        self
    }

    /// Create the client and start its background connection task.
    ///
    /// Returns the shared RPC client and the single-consumer receiver for all server
    /// events. Callers must drain the unbounded event stream; queued events use memory
    /// until received. Server requests block their remote operation until answered.
    /// Call this from inside a Tokio runtime.
    pub fn build(
        self,
    ) -> (
        Arc<AppServerClient>,
        mpsc::UnboundedReceiver<AppServerEvent>,
    ) {
        let initialize = serde_json::to_value(self.initialize)
            .expect("InitializeParams contains only serializable generated types");
        let (commands_tx, commands_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let actor = Actor::new(
            self.endpoint.clone(),
            initialize,
            self.connect_timeout,
            commands_rx,
            events_tx,
        );
        tokio::spawn(actor.run());
        (
            Arc::new(AppServerClient {
                endpoint: self.endpoint,
                commands: commands_tx,
            }),
            events_rx,
        )
    }
}

impl AppServerClient {
    async fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .await
            .map_err(|_| Error::Closed {
                method: "(connection actor exited)".to_owned(),
            })
    }

    /// Create a builder for the given App Server endpoint.
    pub fn builder(endpoint: impl Into<String>) -> AppServerClientBuilder {
        AppServerClientBuilder::new(endpoint)
    }

    /// Return the endpoint configured for this client.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Send a typed App Server request.
    ///
    /// The parameter type implements [`Request`] and determines the method and response
    /// type. Requests are not replayed after a disconnect; a failed call does not establish
    /// whether the server processed it.
    pub async fn request<R: Request>(&self, params: R) -> Result<R::Response> {
        let result = self.call(R::METHOD, encode(params)?).await?;
        decode(result, R::METHOD)
    }

    /// Send a request by method name and return its JSON result.
    ///
    /// Use this for methods missing from the generated protocol types. Prefer
    /// [`AppServerClient::request`] when a typed parameter type is available.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let (responder, receiver) = oneshot::channel();
        self.send(Command::Call {
            method: method.to_owned(),
            params,
            responder,
        })
        .await?;
        receiver.await.map_err(|_| Error::Closed {
            method: method.to_owned(),
        })?
    }
}

fn encode<T: Serialize>(params: T) -> Result<Value> {
    serde_json::to_value(params).map_err(|source| Error::Encode { source })
}

fn decode<T: DeserializeOwned>(value: Value, method: &str) -> Result<T> {
    serde_json::from_value(value).map_err(|source| Error::Decode {
        method: method.to_owned(),
        source,
    })
}
