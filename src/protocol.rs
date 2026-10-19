//! Protocol parsing and conservative replies for server-initiated requests.

use crate::envelope::Reply;
use crate::generated::events::{ServerNotification, ServerRequest as EventServerRequest};
use crate::generated::types::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

/// Reassemble an inbound notification envelope into the generated tagged representation.
pub(crate) fn tagged(method: &str, params: Value) -> Value {
    json!({ "method": method, "params": params })
}

/// Parse a notification without making protocol evolution a connection error.
pub(crate) fn parse_notification(tagged: &Value) -> Option<ServerNotification> {
    ServerNotification::deserialize(tagged).ok()
}

/// Parse a server request when it is present in the generated protocol surface.
pub(crate) fn parse_server_request(tagged: &Value) -> Option<EventServerRequest> {
    EventServerRequest::deserialize(tagged).ok()
}

/// The reason for a fallback refusal, sent to the agent as part of the protocol (legacy
/// approval responses require a `rejection` field). This text is for cases where the
/// **client itself** cannot decide; it is not user-facing copy. Callers provide their own
/// wording through the corresponding response type.
const NO_DECISION: &str = "The client could not obtain a decision for this request.";

fn review_decision(approved: bool, rejection: &str) -> ReviewDecision {
    if approved {
        ReviewDecision::Approved
    } else {
        ReviewDecision::Denied(ReviewDecisionDenied {
            rejection: rejection.to_owned(),
        })
    }
}

/// Conservative reply for a server request the client cannot answer or no one handles.
///
/// Dropped one-shot reply senders use this path so the server is not left waiting. Known
/// approval methods receive a conservative refusal; unknown methods receive a JSON-RPC
/// error. Two separate handwritten `json!` tables once omitted the required `rejection`
/// field from legacy approval refusals. The server could not parse those responses,
/// leaving turns blocked.
///
/// Dispatch by method name rather than request type because unparseable requests still
/// need a reply and only the name is available. Return a JSON-RPC error for unknown
/// methods; fabricating a result would give the server false data.
///
/// Use `decline`, not `cancel`, for approvals: the agent can continue the turn and find an
/// alternative after a refusal, while cancellation interrupts the entire turn.
pub(crate) fn decline_by_method(method: &str) -> Reply {
    match method {
        "item/commandExecution/requestApproval" => {
            Reply::ok(CommandExecutionRequestApprovalResponse {
                decision: CommandExecutionApprovalDecision::Decline,
            })
        }
        "item/fileChange/requestApproval" => Reply::ok(FileChangeRequestApprovalResponse {
            decision: FileChangeApprovalDecision::Decline,
        }),
        "item/permissions/requestApproval" => Reply::ok(PermissionsRequestApprovalResponse {
            permissions: GrantedPermissionProfile::default(),
            scope: None,
            strict_auto_review: None,
        }),
        "applyPatchApproval" => Reply::ok(ApplyPatchApprovalResponse {
            decision: review_decision(false, NO_DECISION),
        }),
        "execCommandApproval" => Reply::ok(ExecCommandApprovalResponse {
            decision: review_decision(false, NO_DECISION),
        }),
        // These are not binary decisions. The client cannot supply the required content,
        // so it reports that it did not answer.
        "item/tool/requestUserInput" => Reply::ok(ToolRequestUserInputResponse {
            answers: std::collections::BTreeMap::new(),
        }),
        "item/tool/call" => Reply::ok(DynamicToolCallResponse {
            content_items: Vec::new(),
            success: false,
        }),
        "mcpServer/elicitation/request" => Reply::ok(McpServerElicitationRequestResponse {
            action: McpServerElicitationAction::Decline,
            content: None,
            _meta: None,
        }),
        // These two methods require real credentials from the client and have no refusal
        // response.
        "account/chatgptAuthTokens/refresh" => {
            Reply::unsupported("client cannot refresh ChatGPT auth tokens")
        }
        "attestation/generate" => Reply::unsupported("client cannot generate attestations"),
        other => Reply::unsupported(format!("client cannot answer {other}")),
    }
}

/// Return an exponentially increasing reconnect delay with jitter.
///
/// The cap doubles from 500 ms per attempt and is limited by `max_ms` (minimum 2 ms). The
/// returned delay ranges from half the cap to the cap. `attempt` starts at zero.
pub fn reconnect_delay(attempt: u32, max_ms: u64) -> Duration {
    let cap = 500u64
        .saturating_mul(2u64.saturating_pow(attempt.min(16)))
        .min(max_ms)
        .max(2);
    let half = cap / 2;
    // Derive jitter from low clock bits. This is not a random number generator; it only
    // spreads simultaneous reconnects, and `half` already guarantees the backoff floor.
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64
        % (half + 1);
    Duration::from_millis(half + jitter)
}
