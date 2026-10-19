//! Connection-owning actor for full-duplex JSON-RPC over one WebSocket.

use crate::client::inbound::{AppServerEvent, ReplySender, ServerReply};
use crate::envelope::{ErrorObject, Incoming, Outgoing, Reply};
use crate::error::{Error, Result};
use crate::generated::types::{InitializeResponse, RequestId};
use crate::protocol::{
    decline_by_method, parse_notification, parse_server_request, reconnect_delay, tagged,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::client_async;
use tokio_tungstenite::tungstenite::Message;

const MAX_BACKOFF_MS: u64 = 30_000;
const INITIALIZE_ID: u64 = 0;

pub(crate) enum Command {
    Call {
        method: String,
        params: Value,
        responder: oneshot::Sender<Result<Value>>,
    },
}

struct Pending {
    method: String,
    responder: oneshot::Sender<Result<Value>>,
}

pub(crate) struct Actor {
    endpoint: String,
    initialize: Value,
    connect_timeout: Duration,
    commands_rx: mpsc::Receiver<Command>,
    events: mpsc::UnboundedSender<AppServerEvent>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
}

impl Actor {
    pub(crate) fn new(
        endpoint: String,
        initialize: Value,
        connect_timeout: Duration,
        commands_rx: mpsc::Receiver<Command>,
        events: mpsc::UnboundedSender<AppServerEvent>,
    ) -> Self {
        Self {
            endpoint,
            initialize,
            connect_timeout,
            commands_rx,
            events,
            next_id: 1,
            pending: HashMap::new(),
        }
    }

    pub(crate) async fn run(mut self) {
        let mut attempt = 0u32;
        loop {
            match self.dial().await {
                Ok((mut ws, response)) => {
                    attempt = 0;
                    self.next_id = 1;
                    let _ = self
                        .events
                        .send(AppServerEvent::Connected(Box::new(response)));
                    let reconnect = self.serve(&mut ws).await;
                    self.fail_pending(&Error::Closed {
                        method: "(connection disconnected)".to_owned(),
                    });
                    let _ = self.events.send(AppServerEvent::Disconnected);
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
        self.fail_pending(&Error::Closed {
            method: "(client closed)".to_owned(),
        });
    }

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
                params: Value::Object(Default::default()),
            },
        )
        .await?;
        Ok((ws, response))
    }

    async fn serve(&mut self, ws: &mut WebSocket) -> bool {
        let (replies_tx, mut replies_rx) = mpsc::unbounded_channel::<ServerReply>();
        loop {
            tokio::select! {
                reply = replies_rx.recv() => {
                    let Some(ServerReply { id, method, value }) = reply else { continue };
                    let reply = value.map_or_else(|| decline_by_method(&method), Reply::ok);
                    if send(ws, &reply.as_outgoing(&id)).await.is_err() { return true; }
                }
                command = self.commands_rx.recv() => {
                    let Some(command) = command else { return false };
                    if self.dispatch(ws, command).await.is_err() { return true; }
                }
                message = ws.next() => match message {
                    Some(Ok(message)) => if self.on_message(ws, message, &replies_tx).await.is_err() { return true; },
                    Some(Err(error)) => { tracing::warn!(%error, "App Server connection interrupted"); return true; }
                    None => return true,
                }
            }
        }
    }

    async fn idle(&mut self, error: &Error, delay: Duration) -> bool {
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return true,
                command = self.commands_rx.recv() => match command {
                    Some(Command::Call { method, responder, .. }) => { let _ = responder.send(Err(clone_error(error))); tracing::debug!(%method, "RPC rejected while disconnected"); }
                    None => return false,
                }
            }
        }
    }

    async fn dispatch(&mut self, ws: &mut WebSocket, command: Command) -> Result<()> {
        let Command::Call {
            method,
            params,
            responder,
        } = command;
        let id = self.take_id();
        self.pending.insert(
            id,
            Pending {
                method: method.clone(),
                responder,
            },
        );
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

    async fn on_message(
        &mut self,
        ws: &mut WebSocket,
        message: Message,
        replies: &mpsc::UnboundedSender<ServerReply>,
    ) -> Result<()> {
        match message {
            Message::Text(text) => match serde_json::from_str::<Incoming>(&text) {
                Ok(Incoming::Response { id, result }) => self.on_response(&id, Ok(result)),
                Ok(Incoming::Error { id, error }) => self.on_response(&id, Err(error)),
                Ok(Incoming::Request { id, method, params }) => {
                    let tagged = tagged(&method, params.clone());
                    let request = parse_server_request(&tagged).map(Box::new).ok_or(params);
                    let reply = ReplySender::new(id.clone(), method.clone(), replies.clone());
                    if self
                        .events
                        .send(AppServerEvent::Request {
                            id,
                            method: method.clone(),
                            request,
                            reply,
                        })
                        .is_err()
                    {
                        tracing::debug!(%method, "event receiver is closed; refusing server request");
                    }
                }
                Ok(Incoming::Notification { method, params }) => {
                    let value = tagged(&method, params);
                    match parse_notification(&value) {
                        Some(notification) => {
                            let _ = self
                                .events
                                .send(AppServerEvent::Notification(Box::new(notification)));
                        }
                        None => {
                            let _ = self.events.send(AppServerEvent::Unparsed(value));
                        }
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "unrecognized App Server message");
                    let raw = serde_json::from_str::<Value>(&text)
                        .unwrap_or_else(|_| Value::String(text.to_string()));
                    let _ = self.events.send(AppServerEvent::Unparsed(raw));
                }
            },
            Message::Ping(payload) => ws
                .send(Message::Pong(payload))
                .await
                .map_err(Error::transport)?,
            Message::Close(_) => {
                return Err(Error::Closed {
                    method: "(server closed connection)".to_owned(),
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn on_response(&mut self, id: &RequestId, outcome: std::result::Result<Value, ErrorObject>) {
        let RequestId::Integer(id) = id else {
            tracing::debug!("ignoring response with an unexpected string ID");
            return;
        };
        let Ok(id) = u64::try_from(*id) else {
            return;
        };
        let Some(pending) = self.pending.remove(&id) else {
            tracing::debug!(id, "ignoring response with no matching request");
            return;
        };
        let result = outcome.map_err(|error| Error::rejected(&pending.method, error));
        let _ = pending.responder.send(result);
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn fail_pending(&mut self, error: &Error) {
        for (_, pending) in self.pending.drain() {
            let _ = pending.responder.send(Err(clone_error(error)));
        }
    }
}

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
                    other => tracing::debug!(?other, "ignoring message received during handshake"),
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

type WebSocket = tokio_tungstenite::WebSocketStream<UnixStream>;

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

async fn send(ws: &mut WebSocket, message: &Outgoing<'_>) -> Result<()> {
    let text = serde_json::to_string(message).map_err(|source| Error::Encode { source })?;
    ws.send(Message::Text(text.into()))
        .await
        .map_err(Error::transport)
}
