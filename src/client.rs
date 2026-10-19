//! A shared client handles RPCs, while independent receivers handle thread events. One
//! actor manages each connection.

pub(crate) mod actor;
pub mod inbound;

use crate::error::{Error, Result};
use crate::generated::methods::Request;
use crate::generated::types::{
    ClientInfo, InitializeCapabilities, InitializeParams, Thread, ThreadListCwdFilter,
    ThreadListParams, ThreadReadParams, ThreadResumeParams, ThreadResumeResponse,
    ThreadStartParams, ThreadStartResponse, ThreadUnsubscribeParams, Turn, TurnInterruptParams,
    TurnStartParams, TurnStartResponse, TurnSteerParams, TurnSteerResponse, UserInput,
};
use actor::{Actor, Attach, Command};
use inbound::{GlobalMessage, ThreadMessage};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot};

const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 3_000;
/// Capacity of the best-effort global message stream. Messages are dropped when full.
const SERVER_EVENT_CAPACITY: usize = 256;
/// Capacity of the client command channel.
const COMMAND_CAPACITY: usize = 128;

/// A client connected to an already-running Codex App Server.
///
/// The client does not manage the server process. Create it with [`AppServerClient::builder`].
pub struct AppServerClient {
    endpoint: String,
    commands: mpsc::Sender<Command>,
    /// Local operation ID used to associate requests with pre-registered message routes
    /// and to identify the route generation.
    next_operation: AtomicU64,
    global: Mutex<mpsc::Receiver<GlobalMessage>>,
}

/// Builder for [`AppServerClient`].
pub struct AppServerClientBuilder {
    endpoint: String,
    initialize: InitializeParams,
    connect_timeout: Duration,
    handle_unrouted: bool,
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
            handle_unrouted: false,
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
    ///
    /// This can reduce traffic when the application does not use those events.
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
    ///
    /// This does not limit the time spent waiting for an RPC response.
    pub fn with_connect_timeout(mut self, connect_timeout_ms: u64) -> Self {
        self.connect_timeout = Duration::from_millis(connect_timeout_ms);
        self
    }

    /// Deliver unclaimed server requests through [`AppServerClient::next_event`].
    ///
    /// By default, unclaimed requests are refused immediately. When enabled, the caller
    /// must read and answer them promptly; dropping a message refuses its request.
    pub fn handling_unrouted_requests(mut self) -> Self {
        self.handle_unrouted = true;
        self
    }

    /// Create the client and start its background connection task.
    ///
    /// Call this from inside a Tokio runtime.
    pub fn build(self) -> Arc<AppServerClient> {
        let initialize = serde_json::to_value(self.initialize)
            .expect("InitializeParams contains only serializable generated types");
        let (commands_tx, commands_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (global_tx, global_rx) = mpsc::channel(SERVER_EVENT_CAPACITY);
        let actor = Actor::new(
            self.endpoint.clone(),
            initialize,
            self.connect_timeout,
            self.handle_unrouted,
            commands_rx,
            global_tx,
        );
        tokio::spawn(actor.run());
        Arc::new(AppServerClient {
            endpoint: self.endpoint,
            commands: commands_tx,
            next_operation: AtomicU64::new(1),
            global: Mutex::new(global_rx),
        })
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

    /// Allocate a new route generation and its channel pair.
    fn attachment(&self) -> (Attach, mpsc::UnboundedReceiver<ThreadMessage>) {
        let epoch = self.next_operation.fetch_add(1, Ordering::Relaxed);
        let (events, messages) = mpsc::unbounded_channel();
        (Attach { epoch, events }, messages)
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
    /// type. Requests have no shared timeout and are not replayed after a disconnect; a
    /// failed call does not establish whether the server processed it.
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
            request_id: self.next_operation.fetch_add(1, Ordering::Relaxed),
            method: method.to_owned(),
            params,
            responder,
        })
        .await?;
        receiver.await.map_err(|_| Error::Closed {
            method: method.to_owned(),
        })?
    }

    /// Read the next message on the global event stream.
    ///
    /// This is a single-consumer, best-effort stream. It may contain connection events,
    /// global notifications, and unclaimed server requests; messages can be dropped when
    /// its buffer is full. Waiting for an event does not block RPCs. Returns `None` after
    /// the client task exits.
    pub async fn next_event(&self) -> Option<GlobalMessage> {
        self.global.lock().await.recv().await
    }

    /// Start a thread and return its message receiver.
    ///
    /// The receiver remains open across turns and reconnects. Keep a client handle alive
    /// while consuming events. The response reports the settings selected by the server.
    pub async fn start_thread(
        &self,
        params: ThreadStartParams,
    ) -> Result<(ThreadStartResponse, mpsc::UnboundedReceiver<ThreadMessage>)> {
        self.request_with_receiver(params).await
    }

    /// Resume a thread and return its message receiver.
    pub async fn resume_thread(
        &self,
        params: ThreadResumeParams,
    ) -> Result<(ThreadResumeResponse, mpsc::UnboundedReceiver<ThreadMessage>)> {
        self.request_with_receiver(params).await
    }

    /// Send a typed request and attach a receiver to the returned thread.
    async fn request_with_receiver<R: Request>(
        &self,
        params: R,
    ) -> Result<(R::Response, mpsc::UnboundedReceiver<ThreadMessage>)> {
        let params = encode(params)?;
        let (attach, messages) = self.attachment();
        let request_id = attach.epoch;
        self.send(Command::RegisterRoute { request_id, attach })
            .await?;
        let (responder, receiver) = oneshot::channel();
        self.send(Command::Call {
            request_id,
            method: R::METHOD.to_owned(),
            params,
            responder,
        })
        .await?;
        let value = receiver.await.map_err(|_| Error::Closed {
            method: R::METHOD.to_owned(),
        })??;
        Ok((decode(value, R::METHOD)?, messages))
    }

    /// Start or continue a turn with the supplied parameters.
    pub async fn start_turn(&self, params: TurnStartParams) -> Result<TurnStartResponse> {
        self.request(params).await
    }

    /// Append input to the active turn if `expected_turn_id` still matches.
    pub async fn steer(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        input: Vec<UserInput>,
    ) -> Result<TurnSteerResponse> {
        self.request(TurnSteerParams {
            thread_id: thread_id.to_owned(),
            expected_turn_id: expected_turn_id.to_owned(),
            input,
            client_user_message_id: None,
        })
        .await
    }

    /// Interrupt a turn, subscribing to its thread first if necessary.
    pub async fn interrupt(&self, thread_id: &str, turn_id: &str) -> Result<()> {
        self.subscribe(ThreadResumeParams {
            thread_id: thread_id.to_owned(),
            ..Default::default()
        })
        .await?;
        self.request(TurnInterruptParams {
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
        })
        .await?;
        Ok(())
    }

    /// Check whether the server records `expected_cwd` as the thread's working directory.
    pub async fn thread_belongs_to(&self, thread_id: &str, expected_cwd: &str) -> Result<bool> {
        let response = self
            .request(ThreadReadParams {
                thread_id: thread_id.to_owned(),
                include_turns: Some(false),
            })
            .await?;
        Ok(response.thread.cwd == expected_cwd)
    }

    /// Return up to `limit` recent threads for a working directory.
    ///
    /// Use [`AppServerClient::request`] for pagination, filtering, or sorting.
    pub async fn recent_threads(&self, cwd: &str, limit: u32) -> Result<Vec<Thread>> {
        let response = self
            .request(ThreadListParams {
                cwd: Some(ThreadListCwdFilter::Text(cwd.to_owned())),
                limit: Some(limit),
                ..Default::default()
            })
            .await?;
        Ok(response.data)
    }

    /// Subscribe this connection to a thread without creating a message receiver.
    ///
    /// Use this before thread operations when this client does not already have a
    /// subscription. Events remain available only through an existing receiver; otherwise
    /// notifications are unclaimed and server requests are refused.
    pub async fn subscribe(&self, resume: ThreadResumeParams) -> Result<()> {
        let (responder, receiver) = oneshot::channel();
        self.send(Command::IsSubscribed {
            thread_id: resume.thread_id.clone(),
            responder,
        })
        .await?;
        if !receiver.await.map_err(|_| Error::Closed {
            method: "(subscription query)".to_owned(),
        })?? {
            self.request(resume).await?;
        }
        Ok(())
    }

    /// Unsubscribe this connection from a thread and close its message route.
    pub async fn unsubscribe(&self, thread_id: &str) -> Result<()> {
        self.request(ThreadUnsubscribeParams {
            thread_id: thread_id.to_owned(),
        })
        .await?;
        Ok(())
    }

    /// Read a turn's latest stored state from the server.
    ///
    /// Returns `None` if the thread does not contain the requested turn. Polling does not
    /// deliver push notifications or server requests; use a thread receiver for those.
    pub async fn poll_turn(&self, thread_id: &str, turn_id: &str) -> Result<Option<Turn>> {
        let response = self
            .request(ThreadReadParams {
                thread_id: thread_id.to_owned(),
                include_turns: Some(true),
            })
            .await?;
        Ok(response
            .thread
            .turns
            .into_iter()
            .find(|turn| turn.id == turn_id))
    }
}

// Serialize request parameters for the JSON-RPC envelope.
fn encode<T: Serialize>(params: T) -> Result<Value> {
    serde_json::to_value(params).map_err(|source| Error::Encode { source })
}

// Decode the JSON-RPC result as the response type for the requested method.
fn decode<T: DeserializeOwned>(value: Value, method: &str) -> Result<T> {
    serde_json::from_value(value).map_err(|source| Error::Decode {
        method: method.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn rpc_waits_for_a_long_running_request_without_a_deadline() {
        let (commands, mut commands_rx) = mpsc::channel(1);
        let (_global_tx, global) = mpsc::channel(1);
        let client = AppServerClient {
            endpoint: String::new(),
            commands,
            next_operation: AtomicU64::new(1),
            global: Mutex::new(global),
        };
        let request = client.call("test/longRunning", Value::Null);
        let server = async {
            let Command::Call { responder, .. } = commands_rx.recv().await.unwrap() else {
                panic!("expected an ordinary RPC");
            };
            // The call must still receive the server's final result after virtual time
            // exceeds the old 30-second limit.
            tokio::time::sleep(Duration::from_secs(3600)).await;
            responder.send(Ok(Value::Bool(true))).unwrap();
        };
        let (result, ()) = tokio::join!(request, server);
        assert_eq!(result.unwrap(), Value::Bool(true));
    }

    #[tokio::test]
    async fn waiting_for_global_events_does_not_block_rpc_and_can_be_cancelled() {
        let (commands, mut commands_rx) = mpsc::channel(1);
        let (global_tx, global) = mpsc::channel(1);
        let client = Arc::new(AppServerClient {
            endpoint: String::new(),
            commands,
            next_operation: AtomicU64::new(1),
            global: Mutex::new(global),
        });
        // Poll the receiver future first so it holds the lock and waits for a message.
        let mut waiting = Box::pin(client.next_event());
        assert!(matches!(
            futures_util::poll!(&mut waiting),
            std::task::Poll::Pending
        ));
        let peer = client.clone();
        let request = peer.call("test", Value::Null);
        let respond = async {
            let Command::Call { responder, .. } = commands_rx.recv().await.unwrap() else {
                panic!("expected an ordinary RPC");
            };
            responder.send(Ok(Value::Bool(true))).unwrap();
        };
        let (response, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(request, respond)
        })
        .await
        .unwrap();
        assert_eq!(response.unwrap(), Value::Bool(true));
        drop(waiting);
        global_tx
            .send(GlobalMessage::Unparsed(Value::Bool(true)))
            .await
            .unwrap();
        assert!(matches!(
            peer.next_event().await,
            Some(GlobalMessage::Unparsed(Value::Bool(true)))
        ));
        drop(global_tx);
        assert!(client.next_event().await.is_none());
    }
}
