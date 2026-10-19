//! Connection-owning actor: full-duplex JSON-RPC over one WebSocket.
//!
//! Client handles do not touch the connection; they send commands through [`Command`].
//! This actor is the only code that reads or writes the socket, allowing one connection
//! to handle four operations that otherwise compete: waiting for multiple responses,
//! receiving notifications, answering server-initiated requests, and reconnecting after a
//! disconnect. The previous two-connection design could still run only one turn at a time.
//!
//! # Two channels, not one
//!
//! Inbound messages are routed by thread ownership into two channels with different
//! semantics. [`crate::protocol::split_notification`] and
//! [`crate::protocol::split_request`] determine ownership:
//!
//! - **`routes`**: `threadId → Route`, with one exclusive channel per thread. A route is
//!   attached by `thread/start`, `thread/resume`, or an additional subscription, and is
//!   removed only when the caller drops its receiver. It **survives turns**. A thread has
//!   at most one unfinished turn (`turn/steer` uses an `expectedTurnId` precondition, and
//!   `turn/start` becomes a steer when a turn is active), so routing by `threadId` is
//!   sufficient; there is no need to include `turnId`.
//! - **`events`**: a bounded `mpsc::Sender` for connection state and global messages.
//!   Server requests carry one-shot reply channels and can have only one consumer, so
//!   this channel cannot broadcast.
//!
//! # Maintain state from protocol messages
//!
//! All RPCs share `Call` and `Pending`. Extract context from the method before sending,
//! then recover the method by ID when the response arrives and update subscription and
//! turn state before delivering the result. Generic and convenience calls use the same
//! path. Local message routes are registered separately; a new thread's route is first
//! stored under a local request ID and bound once the response provides its thread ID.
//! Attaching a route does not imply a server subscription, and cancelling a wait does not
//! undo an operation already completed by the server.
//!
//! Thread messages without a claiming route (when `handling_unrouted_requests` is
//! enabled) share the global channel with actual global messages, but remain `Orphaned*`
//! variants rather than `Global*`: they still belong to a thread; this connection simply
//! has no current listener.
//!
//! # Disconnects
//!
//! Subscriptions are **connection-scoped** (ADR 0005), while routes belong to the client.
//! A disconnect clears `pending` and `subscribed`, but retains routes with consumers and
//! their resume parameters. After the new handshake, the actor sends `thread/resume` for
//! each route so the original receivers continue across reconnects.
//!
//! If a resume response shows that a turn ended during the disconnect, the actor emits a
//! `turn/completed` message so the caller does not wait forever. It does this **only for
//! routes awaiting a turn** (`Route::awaiting_turn`): routes now survive turns, so
//! synthesizing a completion for an idle route would report a turn that was never started.
//!
//! # Transport
//!
//! Connection setup and frame I/O live at the bottom of this module: one WebSocket over a
//! Unix socket, accessed only by this actor, with no need for another abstraction layer.

use crate::client::inbound::{GlobalMessage, ReplySender, ServerReply, ThreadMessage};
use crate::envelope::{ErrorObject, Incoming, Outgoing, Reply};
use crate::error::{Error, Result};
use crate::generated::types::{
    InitializeResponse, RequestId, ThreadResumeParams, Turn, TurnCompletedNotification, TurnStatus,
};
use crate::protocol::{
    NotificationScope, RequestScope, ThreadNotification, decline_by_method, method_of,
    parse_notification, parse_server_request, reconnect_delay, sniff_thread_id, split_notification,
    split_request, tagged,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::client_async;
use tokio_tungstenite::tungstenite::Message;

/// Maximum reconnect backoff.
const MAX_BACKOFF_MS: u64 = 30_000;
/// Fixed ID for `initialize`, which is sent before any other request.
const INITIALIZE_ID: u64 = 0;
const THREAD_START: &str = "thread/start";
const THREAD_RESUME: &str = "thread/resume";

/// Local message route. The epoch prevents an old response from changing a newer route.
pub(crate) struct Attach {
    pub(crate) epoch: u64,
    pub(crate) events: mpsc::UnboundedSender<ThreadMessage>,
}

/// RPCs use `Call`; other commands manipulate local routes or query connection state.
pub(crate) enum Command {
    Call {
        request_id: u64,
        method: String,
        params: Value,
        responder: oneshot::Sender<Result<Value>>,
    },
    /// Bind the message route after the request returns its thread ID.
    RegisterRoute { request_id: u64, attach: Attach },
    IsSubscribed {
        thread_id: String,
        responder: oneshot::Sender<Result<bool>>,
    },
}

/// Shared request context extracted by the actor from the method before sending.
struct Pending {
    method: String,
    thread_id: Option<String>,
    resume: Option<Value>,
    epoch: Option<u64>,
    previous_awaiting: bool,
    previous_resume: Option<Value>,
    attach: Option<Attach>,
    responder: Option<oneshot::Sender<Result<Value>>>,
}

struct Route {
    epoch: u64,
    resume: Value,
    events: mpsc::UnboundedSender<ThreadMessage>,
    /// A turn has been sent and is still awaiting `turn/completed`. Only then should a
    /// reconnect synthesize a completion if the server no longer reports an active turn.
    awaiting_turn: bool,
}

pub(crate) struct Actor {
    endpoint: String,
    initialize: Value,
    connect_timeout: Duration,
    /// Whether to hand server requests unclaimed by a turn to the caller; see
    /// [`crate::AppServerClientBuilder::handling_unrouted_requests`].
    handle_unrouted: bool,
    commands_rx: mpsc::Receiver<Command>,
    /// Global messages unrelated to any thread. A server request can be answered by only
    /// one consumer.
    events: mpsc::Sender<GlobalMessage>,

    // The following fields are connection-scoped and cleared on disconnect; routes are
    // client-scoped and retained across reconnects.
    next_id: u64,
    pending: HashMap<u64, Pending>,
    /// Per-thread message routes keyed by `threadId`; retained across turns.
    routes: HashMap<String, Route>,
    subscribed: HashMap<String, Value>,
    registrations: HashMap<u64, Attach>,
}

impl Actor {
    pub(crate) fn new(
        endpoint: String,
        initialize: Value,
        connect_timeout: Duration,
        handle_unrouted: bool,
        commands_rx: mpsc::Receiver<Command>,
        events: mpsc::Sender<GlobalMessage>,
    ) -> Self {
        Self {
            endpoint,
            initialize,
            connect_timeout,
            handle_unrouted,
            commands_rx,
            events,
            next_id: 1,
            pending: HashMap::new(),
            routes: HashMap::new(),
            subscribed: HashMap::new(),
            registrations: HashMap::new(),
        }
    }

    /// Connect → serve → disconnect → back off → reconnect until the last client Arc is
    /// dropped.
    pub(crate) async fn run(mut self) {
        let mut attempt = 0u32;
        loop {
            match self.dial().await {
                Ok((ws, response)) => {
                    attempt = 0;
                    self.on_connected(response);
                    let mut ws = ws;
                    let reconnect = match self.restore_routes(&mut ws).await {
                        Ok(()) => self.serve(ws).await,
                        Err(error) => {
                            tracing::warn!(%error, "connection interrupted while restoring thread subscriptions");
                            true
                        }
                    };
                    self.reset(&Error::Closed {
                        method: "(connection disconnected)".to_owned(),
                    });
                    self.emit_global(GlobalMessage::Disconnected);
                    if !reconnect {
                        break;
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, endpoint = %self.endpoint, "failed to connect to App Server");
                    attempt = attempt.saturating_add(1);
                    if !self
                        .idle(&error, reconnect_delay(attempt, MAX_BACKOFF_MS))
                        .await
                    {
                        break;
                    }
                }
            }
        }
        self.reset(&Error::Closed {
            method: "(client closed)".to_owned(),
        });
    }

    /// Connect and complete the `initialize` / `initialized` handshake.
    async fn dial(&mut self) -> Result<(WebSocket, InitializeResponse)> {
        let mut ws = connect(&self.endpoint, self.connect_timeout).await?;
        send(
            &mut ws,
            &Outgoing::Request {
                id: INITIALIZE_ID,
                method: "initialize",
                params: &self.initialize,
            },
        )
        .await?;
        let response = tokio::time::timeout(self.connect_timeout, await_initialize(&mut ws))
            .await
            .map_err(|_| Error::Timeout {
                method: "initialize".to_owned(),
                after: self.connect_timeout,
            })??;
        send(
            &mut ws,
            &Outgoing::Notification {
                method: "initialized",
                params: Value::Object(serde_json::Map::new()),
            },
        )
        .await?;
        Ok((ws, response))
    }

    fn on_connected(&mut self, response: InitializeResponse) {
        self.next_id = 1;
        self.emit_global(GlobalMessage::Connected(Box::new(response)));
    }

    /// Restore subscriptions for threads with active consumers after the new handshake.
    async fn restore_routes(&mut self, ws: &mut WebSocket) -> Result<()> {
        self.routes.retain(|_, route| !route.events.is_closed());
        let routes = self
            .routes
            .iter()
            .map(|(thread_id, route)| (thread_id.clone(), route.resume.clone()))
            .collect::<Vec<_>>();
        for (_, resume) in routes {
            self.call(ws, THREAD_RESUME.to_owned(), resume, None, None)
                .await?;
        }
        Ok(())
    }

    /// Main loop. Returns `true` when a disconnect should trigger a reconnect, and `false`
    /// when the client has been dropped and the actor should exit.
    async fn serve(&mut self, mut ws: WebSocket) -> bool {
        // Each connection has one reply mailbox. ReplySender posts a single result here;
        // replies for the old connection become invalid when the connection ends and its
        // receiver is dropped.
        let (replies_tx, mut replies_rx) = mpsc::unbounded_channel::<ServerReply>();
        loop {
            tokio::select! {
                reply = replies_rx.recv() => {
                    let Some(ServerReply { id, method, value }) = reply else { continue };
                    let reply = value.map_or_else(|| decline_by_method(method), Reply::ok);
                    if send(&mut ws, &reply.as_outgoing(&id)).await.is_err() {
                        return true;
                    }
                }
                command = self.commands_rx.recv() => {
                    let Some(command) = command else { return false };
                    if self.dispatch(&mut ws, command).await.is_err() {
                        return true;
                    }
                }
                message = ws.next() => {
                    match message {
                        Some(Ok(message)) => {
                            if self.on_message(&mut ws, message, &replies_tx).await.is_err() {
                                return true;
                            }
                        }
                        Some(Err(error)) => {
                            tracing::warn!(%error, "App Server connection interrupted");
                            return true;
                        }
                        None => return true,
                    }
                }
            }
        }
    }

    /// During backoff, fail commands immediately instead of making callers wait for a
    /// server that is not connected.
    ///
    /// Returns `false` when the actor should exit.
    async fn idle(&mut self, error: &Error, delay: Duration) -> bool {
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return true,
                command = self.commands_rx.recv() => match command {
                    Some(command) => self.fail(command, error),
                    None => return false,
                },
            }
        }
    }

    async fn dispatch(&mut self, ws: &mut WebSocket, command: Command) -> Result<()> {
        match command {
            Command::Call {
                request_id,
                method,
                params,
                responder,
            } => {
                let attach = self.registrations.remove(&request_id);
                self.call(ws, method, params, Some(responder), attach).await
            }
            local => {
                self.local(local);
                Ok(())
            }
        }
    }

    /// Local commands do not send RPCs or claim that a server subscription exists.
    fn local(&mut self, command: Command) {
        match command {
            Command::RegisterRoute { request_id, attach } => {
                self.registrations
                    .retain(|_, attach| !attach.events.is_closed());
                self.registrations.insert(request_id, attach);
            }
            Command::IsSubscribed {
                thread_id,
                responder,
            } => {
                let _ = responder.send(Ok(self.subscribed.contains_key(&thread_id)));
            }
            Command::Call { .. } => unreachable!("RPCs are handled by the connection"),
        }
    }

    /// All requests, including reconnect restoration, use the same send path.
    async fn call(
        &mut self,
        ws: &mut WebSocket,
        method: String,
        params: Value,
        responder: Option<oneshot::Sender<Result<Value>>>,
        attach: Option<Attach>,
    ) -> Result<()> {
        let mut pending = Pending {
            method,
            thread_id: None,
            resume: None,
            epoch: None,
            previous_awaiting: false,
            previous_resume: None,
            attach,
            responder,
        };
        if let Err(error) = self.before_request(&params, &mut pending) {
            if let Some(responder) = pending.responder {
                let _ = responder.send(Err(error));
                return Ok(());
            }
            return Err(error);
        }
        let id = self.take_id();
        let method = pending.method.clone();
        self.pending.insert(id, pending);
        send(
            ws,
            &Outgoing::Request {
                id,
                method: &method,
                params: &params,
            },
        )
        .await
    }

    /// Identify state changes from the method actually sent; ordinary requests do not
    /// retain a copy of their parameters.
    fn before_request(&mut self, params: &Value, pending: &mut Pending) -> Result<()> {
        match pending.method.as_str() {
            THREAD_START | "thread/fork" => {}
            THREAD_RESUME | "thread/unsubscribe" | "turn/start" | "review/start" => {
                let thread = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        Error::Protocol(format!("{} is missing threadId", pending.method))
                    })?;
                pending.thread_id = Some(thread.to_owned());
                pending.epoch = self.routes.get(thread).map(|route| route.epoch);
                if pending.method == THREAD_RESUME {
                    pending.resume = Some(params.clone());
                } else if pending.method == "turn/start" || pending.method == "review/start" {
                    // A detached review belongs to a new thread; do not record its turn
                    // state against the original thread.
                    if pending.method == "review/start"
                        && params.get("delivery").and_then(Value::as_str) == Some("detached")
                    {
                        return Ok(());
                    }
                    let Some(route) = self
                        .routes
                        .get_mut(thread)
                        .filter(|route| !route.events.is_closed())
                    else {
                        return Err(Error::Closed {
                            method: pending.method.clone(),
                        });
                    };
                    if !self.subscribed.contains_key(thread) {
                        return Err(Error::Protocol(format!(
                            "thread {thread} is not fully subscribed"
                        )));
                    }
                    pending.previous_awaiting = route.awaiting_turn;
                    route.awaiting_turn = true;
                    // Save supplied overrides; retain the original resume configuration
                    // for omitted fields.
                    let mut resume = route.resume.clone();
                    for field in ["approvalsReviewer", "approvalPolicy"] {
                        if let Some(value) = params.get(field).filter(|value| !value.is_null()) {
                            resume[field] = value.clone();
                        }
                    }
                    pending.previous_resume =
                        Some(std::mem::replace(&mut route.resume, resume.clone()));
                    pending.resume = Some(resume);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Bind only the local route; do not change server subscription state.
    fn mount(&mut self, thread_id: String, resume: Value, attach: Attach) {
        if attach.events.is_closed()
            || self
                .routes
                .get(&thread_id)
                .is_some_and(|route| route.epoch > attach.epoch)
        {
            return;
        }
        let awaiting_turn = self
            .routes
            .get(&thread_id)
            .is_some_and(|route| route.awaiting_turn);
        self.routes.insert(
            thread_id,
            Route {
                epoch: attach.epoch,
                resume,
                events: attach.events,
                awaiting_turn,
            },
        );
    }

    async fn on_message(
        &mut self,
        ws: &mut WebSocket,
        message: Message,
        replies: &mpsc::UnboundedSender<ServerReply>,
    ) -> Result<()> {
        match message {
            Message::Text(text) => {
                match serde_json::from_str::<Incoming>(&text) {
                    Ok(Incoming::Response { id, result }) => self.on_response(&id, Ok(result)),
                    Ok(Incoming::Error { id, error }) => self.on_response(&id, Err(error)),
                    Ok(Incoming::Request { id, method, params }) => {
                        self.on_server_request(ws, id, &method, params, replies)
                            .await?;
                    }
                    Ok(Incoming::Notification { method, params }) => {
                        self.on_notification(&method, params);
                    }
                    // None of the four shapes matched. Pass the message through so an
                    // unrecognized message cannot take down the connection.
                    Err(error) => {
                        tracing::debug!(%error, "unrecognized App Server message");
                        let raw = serde_json::from_str::<Value>(&text)
                            .unwrap_or_else(|_| Value::String(text.to_string()));
                        self.emit_global(GlobalMessage::Unparsed(raw));
                    }
                }
                Ok(())
            }
            Message::Ping(payload) => ws
                .send(Message::Pong(payload))
                .await
                .map_err(Error::transport),
            Message::Close(_) => Err(Error::Closed {
                method: "(server closed connection)".to_owned(),
            }),
            _ => Ok(()),
        }
    }

    /// A request we sent completed with a successful `result` or a server `error`.
    fn on_response(&mut self, id: &RequestId, outcome: std::result::Result<Value, ErrorObject>) {
        // We use only integer IDs for requests, so a response with a string ID is not ours.
        let RequestId::Integer(id) = id else {
            tracing::debug!("ignoring response with an unexpected string ID");
            return;
        };
        let Ok(id) = u64::try_from(*id) else {
            return;
        };
        let Some(mut pending) = self.pending.remove(&id) else {
            tracing::debug!(id, "ignoring response with no matching request");
            return;
        };
        let result = outcome.map_err(|error| Error::rejected(&pending.method, error));
        let result = match result {
            Ok(value) => self.after_response(&mut pending, &value).map(|()| value),
            Err(error) => Err(error),
        };
        if result.is_err() {
            self.failed_response(&pending);
        }
        if let Some(responder) = pending.responder {
            let _ = responder.send(result);
        }
    }

    /// Apply response side effects on the request's connection before delivering the
    /// result to the caller.
    fn after_response(&mut self, pending: &mut Pending, value: &Value) -> Result<()> {
        match pending.method.as_str() {
            THREAD_START | "thread/fork" => {
                let thread = value
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        Error::Protocol(format!("{} response is missing thread.id", pending.method))
                    })?;
                let mut resume = resume_params(thread);
                for field in ["approvalsReviewer", "approvalPolicy"] {
                    if let Some(setting) = value.get(field) {
                        resume[field] = setting.clone();
                    }
                }
                self.subscribed.insert(thread.to_owned(), resume.clone());
                if let Some(attach) = pending.attach.take() {
                    self.mount(thread.to_owned(), resume, attach);
                }
            }
            THREAD_RESUME => {
                let thread = pending
                    .thread_id
                    .as_ref()
                    .expect("threadId was extracted before sending");
                let resume = pending
                    .resume
                    .as_ref()
                    .expect("resume parameters were saved before sending")
                    .clone();
                let completion = if self
                    .routes
                    .get(thread)
                    .is_some_and(|route| Some(route.epoch) == pending.epoch && route.awaiting_turn)
                {
                    let turns = value.pointer("/thread/turns").ok_or_else(|| {
                        Error::Protocol("thread/resume response is missing thread.turns".to_owned())
                    })?;
                    Some(
                        Vec::<Turn>::deserialize(turns).map_err(|source| Error::Decode {
                            method: THREAD_RESUME.to_owned(),
                            source,
                        })?,
                    )
                } else {
                    None
                };
                self.subscribed.insert(thread.clone(), resume.clone());
                if let Some(route) = self.routes.get_mut(thread) {
                    route.resume = resume.clone();
                }
                if let Some(attach) = pending.attach.take() {
                    self.mount(thread.clone(), resume, attach);
                }
                if let Some(turns) = completion {
                    self.recover_completion(thread, turns);
                }
            }
            "thread/unsubscribe" => {
                let thread = pending
                    .thread_id
                    .as_ref()
                    .expect("threadId was extracted before sending");
                self.subscribed.remove(thread);
                self.routes.remove(thread);
            }
            "turn/start" | "review/start" => {
                if let (Some(thread), Some(resume)) = (&pending.thread_id, &pending.resume) {
                    if let Some(route) = self
                        .routes
                        .get_mut(thread)
                        .filter(|route| Some(route.epoch) == pending.epoch)
                    {
                        route.resume = resume.clone();
                    }
                    if let Some(saved) = self.subscribed.get_mut(thread) {
                        *saved = resume.clone();
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn failed_response(&mut self, pending: &Pending) {
        let Some(thread) = &pending.thread_id else {
            return;
        };
        if !self
            .routes
            .get(thread)
            .is_some_and(|route| Some(route.epoch) == pending.epoch)
        {
            return;
        }
        if pending.method == THREAD_RESUME && !self.subscribed.contains_key(thread) {
            self.subscribed.remove(thread);
            self.routes.remove(thread);
        } else if matches!(pending.method.as_str(), "turn/start" | "review/start")
            && pending.resume.is_some()
            && let Some(route) = self.routes.get_mut(thread)
        {
            route.awaiting_turn = pending.previous_awaiting;
            if let Some(resume) = &pending.previous_resume {
                route.resume = resume.clone();
            }
        }
    }

    /// Turns completed during a disconnect will not be pushed again, so synthesize their
    /// completion notifications here.
    ///
    /// Only synthesize for routes **currently awaiting a turn**. Routes survive across
    /// turns; completing an idle route would report a turn that was never started.
    fn recover_completion(&mut self, thread_id: &str, turns: Vec<Turn>) {
        let Some(route) = self.routes.get_mut(thread_id) else {
            return;
        };
        if !route.awaiting_turn {
            return;
        }
        if turns
            .iter()
            .any(|turn| turn.status == TurnStatus::InProgress)
        {
            return;
        }
        let Some(turn) = turns.into_iter().max_by_key(|turn| turn.started_at) else {
            return;
        };
        route.awaiting_turn = false;
        self.push(
            thread_id,
            ThreadMessage::Notification(Box::new(ThreadNotification::TurnCompleted(
                TurnCompletedNotification {
                    thread_id: thread_id.to_owned(),
                    turn,
                },
            ))),
        );
    }

    /// Handle a server-initiated request. **Always reply**, or the turn remains blocked
    /// until it times out.
    async fn on_server_request(
        &mut self,
        ws: &mut WebSocket,
        id: RequestId,
        method: &str,
        params: Value,
        replies: &mpsc::UnboundedSender<ServerReply>,
    ) -> Result<()> {
        let Some(request) = parse_server_request(&tagged(method, params)) else {
            // An unrecognized request still needs a reply, but it cannot be handed to the
            // caller for a decision. It is not emitted as `Unparsed`: that stream is
            // best-effort, while this request has a waiting peer.
            tracing::warn!(%method, "automatically refusing an unparseable App Server request");
            send(ws, &decline_by_method(method).as_outgoing(&id)).await?;
            return Ok(());
        };
        let method = method_of(&request);
        let scope = split_request(request);

        // The server **blocks while waiting** for this reply, so it cannot use the
        // best-effort global channel: an unconsumed request would hang indefinitely.
        // Refuse requests with no claiming turn immediately unless the caller explicitly
        // opts in to handling them. Truly global requests follow the same setting because
        // no route can claim them.
        let claimed = matches!(&scope,
            RequestScope::Thread { thread_id, .. } if self.routes.contains_key(thread_id)
        );
        if !claimed && !self.handle_unrouted {
            tracing::warn!(%method, "no turn claimed this App Server request; refusing it automatically");
            send(ws, &decline_by_method(method).as_outgoing(&id)).await?;
            return Ok(());
        }
        let reply = ReplySender::new(id.clone(), method, replies.clone());
        match scope {
            RequestScope::Thread { thread_id, request } => match self.routes.get(&thread_id) {
                Some(route) => {
                    let message = ThreadMessage::Request {
                        id: id.clone(),
                        method,
                        request: Box::new(request),
                        reply,
                    };
                    if route.events.send(message).is_err() {
                        // No one is listening to this route. This is the main case where
                        // the caller has gone away: SendError drops the message, and its
                        // ReplySender then sends a refusal.
                        tracing::debug!(%method, "thread message route is closed; refusing request");
                        return Ok(());
                    }
                }
                // `claimed` ensures this path is reached only when `handle_unrouted` is enabled.
                None => self.emit_global(GlobalMessage::OrphanedRequest {
                    id: id.clone(),
                    thread_id,
                    method,
                    request: Box::new(request),
                    reply,
                }),
            },
            RequestScope::Global(request) => self.emit_global(GlobalMessage::Request {
                id: id.clone(),
                method,
                request: Box::new(request),
                reply,
            }),
        }
        Ok(())
    }

    fn on_notification(&mut self, method: &str, params: Value) {
        let tagged = tagged(method, params);
        match parse_notification(&tagged) {
            // Route thread notifications to their claiming route; send the rest to the
            // global stream. Do not normalize or filter the 85 variants: callers decide
            // which notifications matter and receive each one unchanged.
            Some(notification) => match split_notification(notification) {
                NotificationScope::Thread {
                    thread_id,
                    notification,
                } => {
                    if let Some(route) = self.routes.get_mut(&thread_id) {
                        // Turn completion clears the awaiting-turn state but does **not**
                        // close the route: it belongs to the thread, not one turn, so the
                        // caller can use the same handle for the next turn.
                        if matches!(notification, ThreadNotification::TurnCompleted(_)) {
                            route.awaiting_turn = false;
                        }
                        self.push(
                            &thread_id,
                            ThreadMessage::Notification(Box::new(notification)),
                        );
                    } else {
                        self.emit_global(GlobalMessage::OrphanedNotification {
                            thread_id,
                            notification: Box::new(notification),
                        });
                    }
                }
                NotificationScope::Global(notification) => {
                    self.emit_global(GlobalMessage::Notification(Box::new(notification)));
                }
            },
            // This client does not recognize the notification. Try to infer its thread
            // ownership; route it there if a listener exists, otherwise use the global stream.
            None => match sniff_thread_id(&tagged).map(str::to_owned) {
                Some(thread_id) if self.routes.contains_key(&thread_id) => {
                    self.push(&thread_id, ThreadMessage::Unparsed(tagged));
                }
                _ => self.emit_global(GlobalMessage::Unparsed(tagged)),
            },
        }
    }

    fn push(&mut self, thread: &str, message: ThreadMessage) {
        let Some(route) = self.routes.get(thread) else {
            return;
        };
        if route.events.send(message).is_err() {
            // The caller is no longer listening.
            self.routes.remove(thread);
        }
    }

    /// Deliver to the best-effort global diagnostics channel. If delivery fails, dropping
    /// the request's ReplySender sends a conservative refusal back over this connection.
    fn emit_global(&self, message: GlobalMessage) {
        let _ = self.events.try_send(message);
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Answer a command while disconnected: fail it immediately.
    ///
    /// Subscription commands fail instead of being queued for reconnect. Subscriptions
    /// are connection-scoped (ADR 0005), so recording one while disconnected would create
    /// false state for the next connection.
    fn fail(&mut self, command: Command, error: &Error) {
        match command {
            Command::Call {
                request_id,
                responder,
                ..
            } => {
                self.registrations.remove(&request_id);
                let _ = responder.send(Err(clone_error(error)));
            }
            Command::RegisterRoute { .. } => self.local(command),
            Command::IsSubscribed { responder, .. } => {
                let _ = responder.send(Err(clone_error(error)));
            }
        }
    }

    /// Clear connection-scoped state on disconnect. Keep active routes for restoration;
    /// never replay requests that were already sent.
    fn reset(&mut self, error: &Error) {
        for (_, pending) in self.pending.drain() {
            if let Some(responder) = pending.responder {
                let _ = responder.send(Err(clone_error(error)));
            }
        }
        self.routes.retain(|_, route| !route.events.is_closed());
        self.registrations
            .retain(|_, attach| !attach.events.is_closed());
        self.subscribed.clear();
    }
}

/// Read until the `initialize` response arrives. Forwarding notifications received during
/// this short handshake window to the global stream would add complexity for little
/// benefit, so log them instead.
async fn await_initialize(ws: &mut WebSocket) -> Result<InitializeResponse> {
    while let Some(message) = ws.next().await {
        match message.map_err(Error::transport)? {
            Message::Text(text) => {
                let incoming =
                    serde_json::from_str::<Incoming>(&text).map_err(|source| Error::Decode {
                        method: "initialize".to_owned(),
                        source,
                    })?;
                let expected = RequestId::Integer(INITIALIZE_ID as i64);
                match incoming {
                    Incoming::Response { id, result } if id == expected => {
                        return serde_json::from_value(result).map_err(|source| Error::Decode {
                            method: "initialize".to_owned(),
                            source,
                        });
                    }
                    Incoming::Error { id, error } if id == expected => {
                        return Err(Error::rejected("initialize", error));
                    }
                    other => {
                        tracing::debug!(?other, "ignoring message received during handshake");
                        continue;
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    Err(Error::Closed {
        method: "initialize".to_owned(),
    })
}

/// Default resume parameters for a newly created thread.
///
/// Use the generated type instead of handwritten `json!`. If the server cannot parse the
/// parameters, the subscription is not restored and the thread stops receiving events
/// after reconnect. Overrides actually used by the thread (reviewer and `approvalPolicy`)
/// are added by inspecting the first turn method before sending it.
fn resume_params(thread_id: &str) -> Value {
    serde_json::to_value(ThreadResumeParams {
        thread_id: thread_id.to_owned(),
        ..Default::default()
    })
    .expect("ThreadResumeParams contains only serializable generated types")
}

/// [`Error`] contains a non-cloneable source, but disconnects must send the same cause to
/// multiple waiters.
fn clone_error(error: &Error) -> Error {
    match error {
        Error::Server(server) => Error::Server(server.clone()),
        Error::Timeout { method, after } => Error::Timeout {
            method: method.clone(),
            after: *after,
        },
        Error::Closed { method } => Error::Closed {
            method: method.clone(),
        },
        Error::Connect { endpoint, source } => Error::Connect {
            endpoint: endpoint.clone(),
            source: source.to_string().into(),
        },
        other => Error::Protocol(other.to_string()),
    }
}

/// An App Server connection: a WebSocket over a Unix socket.
type WebSocket = tokio_tungstenite::WebSocketStream<UnixStream>;

/// Establish a WebSocket connection to the App Server.
///
/// `endpoint` has the form `unix:///absolute/path/to/app-server-control.sock`. On return,
/// the WebSocket handshake is complete but **`initialize` has not been sent**. The server
/// rejects requests before initialization; [`Actor::dial`] performs that next step.
async fn connect(endpoint: &str, timeout: Duration) -> Result<WebSocket> {
    let fail = |source: Box<dyn std::error::Error + Send + Sync>| Error::Connect {
        endpoint: endpoint.to_owned(),
        source,
    };
    let path = endpoint
        .strip_prefix("unix://")
        .ok_or_else(|| fail("endpoint must have the form unix:///absolute/path/to.sock".into()))?;
    let stream = tokio::time::timeout(timeout, UnixStream::connect(path))
        .await
        .map_err(|_| fail("timed out connecting to Unix socket".into()))?
        .map_err(|error| fail(Box::new(error)))?;
    let (ws, _) = tokio::time::timeout(timeout, client_async("ws://localhost/", stream))
        .await
        .map_err(|_| fail("WebSocket handshake timed out".into()))?
        .map_err(|error| fail(Box::new(error)))?;
    Ok(ws)
}

/// Send a JSON-RPC text frame.
///
/// Accept only [`Outgoing`]: the envelope type defines the outbound shape rather than a
/// `json!` expression at the call site.
async fn send(ws: &mut WebSocket, message: &Outgoing<'_>) -> Result<()> {
    let text = serde_json::to_string(message).map_err(|source| Error::Encode { source })?;
    ws.send(Message::Text(text.into()))
        .await
        .map_err(Error::transport)
}
