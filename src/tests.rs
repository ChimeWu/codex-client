//! Protocol serialization tests and real-daemon integration tests.
//!
//! Real-daemon tests are ignored and require a daemon managed by the caller. Set
//! `CODEX_APP_SERVER_ENDPOINT=unix:///…/app-server-control.sock` to run them explicitly.

use crate::client::inbound::{ReplySender, ServerReply};
use crate::generated::methods::{self as requests, Request};
use crate::generated::types::{GetAccountRateLimitsParams, ModelListParams};
use crate::protocol::{parse_notification, parse_server_request, tagged};
use crate::{AppServerClient, AppServerEvent};
use serde_json::{Value, json};
use tokio::sync::mpsc;

#[test]
fn nullary_params_serialize_to_null() {
    assert_eq!(
        serde_json::to_value(requests::AccountLogoutParams).unwrap(),
        Value::Null
    );
    assert_eq!(
        serde_json::to_value(requests::ConfigRequirementsReadParams).unwrap(),
        Value::Null
    );
    assert_eq!(
        serde_json::to_value(None::<GetAccountRateLimitsParams>).unwrap(),
        Value::Null
    );
    assert_eq!(
        <Option<GetAccountRateLimitsParams> as Request>::METHOD,
        "account/rateLimits/read"
    );
}

#[test]
fn generated_events_are_parsed_without_scope_reclassification() {
    let notification = parse_notification(&tagged(
        "turn/completed",
        json!({"threadId":"t","turn":{"id":"x","items":[],"status":"completed"}}),
    ));
    assert!(notification.is_some());
    let request = parse_server_request(&tagged(
        "item/tool/call",
        json!({"threadId":"t","turnId":"turn-x","callId":"call-x","tool":"x","arguments":{}}),
    ));
    assert!(request.is_some());
    assert!(parse_notification(&tagged("future/notification", json!({"threadId":"t"}))).is_none());
    assert!(parse_server_request(&tagged("future/request", json!({}))).is_none());
}

#[tokio::test]
async fn dropping_a_server_request_reply_handle_posts_a_refusal_request() {
    let (replies_tx, mut replies_rx) = mpsc::unbounded_channel::<ServerReply>();
    drop(ReplySender::new(
        crate::generated::types::RequestId::Integer(7),
        "future/request".to_owned(),
        replies_tx,
    ));
    let reply = replies_rx
        .recv()
        .await
        .expect("drop should schedule a refusal");
    assert_eq!(reply.id, crate::generated::types::RequestId::Integer(7));
    assert_eq!(reply.method, "future/request");
    assert!(reply.value.is_none());
}

fn daemon_endpoint() -> Option<String> {
    std::env::var("CODEX_APP_SERVER_ENDPOINT").ok()
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon"]
async fn real_daemon_accepts_nullary_params() {
    let Some(endpoint) = daemon_endpoint() else {
        return;
    };
    let (client, _events) = AppServerClient::builder(endpoint).build();
    client
        .request(None::<GetAccountRateLimitsParams>)
        .await
        .expect("daemon account/rateLimits/read");
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon"]
async fn real_daemon_emits_initialize_event_and_accepts_requests() {
    let Some(endpoint) = daemon_endpoint() else {
        return;
    };
    let (client, mut events) = AppServerClient::builder(endpoint).build();
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
        .await
        .expect("daemon initialize timeout")
        .expect("event stream closed");
    assert!(matches!(event, AppServerEvent::Connected(_)));
    let models = client
        .request(ModelListParams::default())
        .await
        .expect("daemon model/list");
    assert!(
        !models.data.is_empty(),
        "daemon should publish at least one model"
    );
}
