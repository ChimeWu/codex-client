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

/// A client connected to an App Server managed by the caller.
///
/// The client **never** starts, stops, or supervises the App Server process; it only
/// connects to it. Process lifecycle belongs to `codex app-server daemon` or an
/// equivalent systemd/launchd unit. Configure and create an instance with
/// [`AppServerClient::builder`].
///
/// All operations take `&self`; the builder returns a cloneable `Arc<AppServerClient>`.
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
    /// `endpoint` has the form `unix:///absolute/path/to/app-server-control.sock`.
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

    /// Self-reported identity shown in server logs.
    pub fn with_client_info(mut self, client_info: ClientInfo) -> Self {
        self.initialize.client_info = client_info;
        self
    }

    /// Declare handshake capabilities, such as experimental APIs or notification filters.
    pub fn with_capabilities(mut self, capabilities: InitializeCapabilities) -> Self {
        self.initialize.capabilities = Some(capabilities);
        self
    }

    /// Ask the server not to send these notifications.
    ///
    /// Notifications such as `mcpServer/startupStatus/updated` can appear several times
    /// per turn. If they are not needed, opting out during the handshake is cheaper than
    /// filtering them after receipt.
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

    /// Limit only connection establishment and initialization, not RPC execution time.
    pub fn with_connect_timeout(mut self, connect_timeout_ms: u64) -> Self {
        self.connect_timeout = Duration::from_millis(connect_timeout_ms);
        self
    }

    /// Take ownership of server requests not claimed by a thread receiver. Read them with
    /// [`AppServerClient::next_event`].
    ///
    /// By default, these requests are **not** handed off and are refused immediately.
    /// The server waits after issuing a request, so putting it on a best-effort stream
    /// that nobody reads could leave the turn stuck indefinitely. If this option is
    /// enabled, the caller must consume requests promptly. Dropping a message closes its
    /// one-shot reply sender and the actor refuses it.
    pub fn handling_unrouted_requests(mut self) -> Self {
        self.handle_unrouted = true;
        self
    }

    /// Create the client and start its connection actor. Must be called inside a Tokio
    /// runtime.
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

    pub fn builder(endpoint: impl Into<String>) -> AppServerClientBuilder {
        AppServerClientBuilder::new(endpoint)
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Send a typed request.
    ///
    /// There is no shared timeout. A disconnected request fails and is not replayed. A
    /// disconnect does not prove the server skipped the operation; check its result
    /// according to the operation's semantics.
    ///
    /// The parameter type implements [`Request`] and determines the method and response
    /// type, so `client.request(ThreadListParams { .. })` has a known return type:
    ///
    /// ```no_run
    /// # use codex_client_cw::{AppServerClient, types::*};
    /// # async fn demo(client: &AppServerClient) -> anyhow::Result<()> {
    /// let models = client.request(ModelListParams::default()).await?;
    /// # let _ = models;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// The actor maintains subscriptions and turn state from the method, keeping this
    /// generic entry point consistent with convenience methods. Establish and retain the
    /// target thread's event receiver before starting a turn.
    pub async fn request<R: Request>(&self, params: R) -> Result<R::Response> {
        let result = self.call(R::METHOD, encode(params)?).await?;
        decode(result, R::METHOD)
    }

    /// Execute one request and return its uninterpreted `result`.
    ///
    /// Escape hatch for methods added to the server after this client's generated
    /// protocol snapshot. Prefer [`AppServerClient::request`] when possible; it preserves
    /// each method's full type information.
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

    /// Read the next global message unrelated to any thread.
    ///
    /// This is a single-consumer, best-effort stream. Server requests carry one-shot reply
    /// handles and cannot be broadcast to competing responders. Concurrent calls consume
    /// messages one at a time in lock order; waiting does not block RPCs. Returns `None`
    /// after the actor exits.
    pub async fn next_event(&self) -> Option<GlobalMessage> {
        self.global.lock().await.recv().await
    }

    /// Create a thread, subscribe this connection to it, and return an independent
    /// message receiver.
    ///
    /// The receiver remains active across turns; `turn/completed` does not close it. The
    /// stream ends only if the subscription cannot be restored or the actor exits. The
    /// consuming task should hold a client `Arc`; the receiver alone does not keep the
    /// connection alive.
    ///
    /// The response contains the model, sandbox, `approvalPolicy`, and other settings
    /// ultimately selected by the server. They may differ from the supplied values
    /// because defaults come from server configuration; read the response when needed.
    pub async fn start_thread(
        &self,
        params: ThreadStartParams,
    ) -> Result<(ThreadStartResponse, mpsc::UnboundedReceiver<ThreadMessage>)> {
        self.request_with_receiver(params).await
    }

    /// Resume a thread and bind an independent message receiver before delivering the
    /// response. Always sends `thread/resume`.
    pub async fn resume_thread(
        &self,
        params: ThreadResumeParams,
    ) -> Result<(ThreadResumeResponse, mpsc::UnboundedReceiver<ThreadMessage>)> {
        self.request_with_receiver(params).await
    }

    /// Register a message route for the thread in the response. The RPC still uses
    /// `Call`.
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

    /// Equivalent to `request(TurnStartParams)`. The actor checks the subscription and
    /// maintains reconnect state in one place.
    pub async fn start_turn(&self, params: TurnStartParams) -> Result<TurnStartResponse> {
        self.request(params).await
    }

    /// Append input. The server rejects the request if `expected_turn_id` is no longer
    /// the current turn.
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

    /// Interrupt a turn. Subscribe first if needed without replacing the existing event
    /// receiver.
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

    /// Verify that a thread belongs to `expected_cwd`.
    ///
    /// Ownership is determined by the cwd recorded by the server, not by the list that
    /// returned the thread. Thread IDs often come from user input, so list membership
    /// alone does not prove that a thread belongs to the current project.
    pub async fn thread_belongs_to(&self, thread_id: &str, expected_cwd: &str) -> Result<bool> {
        let response = self
            .request(ThreadReadParams {
                thread_id: thread_id.to_owned(),
                include_turns: Some(false),
            })
            .await?;
        Ok(response.thread.cwd == expected_cwd)
    }

    /// Return the most recent thread under a directory as the protocol's [`Thread`].
    ///
    /// Use `request(ThreadListParams { .. })` with complete parameters for pagination,
    /// filtering, or sorting.
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

    /// Join a thread on this connection without taking ownership of its message stream.
    ///
    /// The server routes thread operations only to connections joined to the active
    /// thread (ADR 0005), so call this before operations such as `turn/interrupt`. No
    /// request is sent if the connection is already subscribed.
    ///
    /// The tradeoff is that no route is installed: notifications for this thread become
    /// orphan messages on this connection, and server requests are conservatively
    /// refused. Keep the receiver returned by `start_thread` or `resume_thread` when you
    /// need the message stream.
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

    /// Explicitly unsubscribe from a thread and remove its route.
    ///
    /// After the last subscriber leaves, the server unloads the thread after a quiet
    /// period and emits `thread/closed`.
    pub async fn unsubscribe(&self, thread_id: &str) -> Result<()> {
        self.request(ThreadUnsubscribeParams {
            thread_id: thread_id.to_owned(),
        })
        .await?;
        Ok(())
    }

    /// Read a turn through `thread/read` and return the protocol's [`Turn`].
    ///
    /// Use this as a fallback if a push is lost, the connection drops, or the process
    /// restarts. It cannot replace a subscription because server requests are push-only.
    ///
    /// The returned [`Turn`] contains `status`, `items`, `error`, and duration. The caller
    /// decides how to turn items into user-facing text.
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

/// Parameters → JSON.
fn encode<T: Serialize>(params: T) -> Result<Value> {
    serde_json::to_value(params).map_err(|source| Error::Encode { source })
}

/// JSON → response type.
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
