//! Typed parsing of inbound messages, splitting by thread ownership, and constructing
//! replies to server requests.
//!
//! # Build replies from generated response types
//!
//! Every server request has a schema-generated response type (such as
//! `CommandExecutionRequestApprovalResponse` and `PermissionsRequestApprovalResponse`).
//! This module constructs replies **only** from those types, never handwritten `json!`.
//! A handwritten shape has no compile-time checks and could silently become invalid when
//! the schema changes; the server would then never receive a valid reply and the turn
//! could remain blocked indefinitely.
//!
//! # Split by thread ownership
//!
//! [`split_notification`] and [`split_request`] split generated `ServerNotification` and
//! `ServerRequest` values into narrower thread-owned and global types for
//! [`crate::client::inbound::ThreadMessage`] and
//! [`crate::client::inbound::GlobalMessage`]. Both functions use exhaustive matches, so a
//! new protocol variant requires an explicit ownership decision. This is intentional:
//! inferring ownership from `params.get("threadId")` previously missed two real shapes.
//! Legacy approvals (`ApplyPatchApproval` and `ExecCommandApproval`) use
//! `conversationId`, while `ThreadStarted` nests its ID in `thread.id`. Those messages
//! were silently routed to the global stream instead of their thread channel. Exhaustive
//! matches use typed fields and prevent similar omissions when the protocol evolves.

use crate::envelope::Reply;
use crate::generated::events::{ServerNotification, ServerRequest};
use crate::generated::types::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

/// Reassemble the `method` and `params` extracted from an envelope into a tagged value.
///
/// Generated `ServerNotification` and `ServerRequest` types use
/// `#[serde(tag = "method", content = "params")]` and require this shape for parsing.
pub(crate) fn tagged(method: &str, params: Value) -> Value {
    json!({ "method": method, "params": params })
}

/// Parse a server notification.
///
/// A parse failure is **not** an error: the server may add fields or notifications at any
/// time, and an unrecognized notification must not fail the connection. Returns `None`
/// for an unknown notification so the caller can handle it as such.
///
/// Deserialize from `&Value` rather than `from_value(value.clone())`: the failure path
/// must preserve the original JSON, and the success path should not clone the entire tree.
pub(crate) fn parse_notification(tagged: &Value) -> Option<ServerNotification> {
    ServerNotification::deserialize(tagged).ok()
}

/// Parse a server request.
///
/// Parsing is best-effort, but the consequence differs: an unrecognized request **still
/// requires a reply** or the server remains blocked. If this returns `None`, the caller
/// should use [`decline_by_method`] with the method name.
pub(crate) fn parse_server_request(tagged: &Value) -> Option<ServerRequest> {
    ServerRequest::deserialize(tagged).ok()
}

/// Best-effort thread ownership inference for a notification this client cannot parse.
///
/// Use this only when `parse_notification` fails because the protocol added a notification
/// this client does not know. Known notifications use typed fields in
/// [`split_notification`] instead of inference. Given the `{method, params}` shape from
/// [`tagged`], try the three known ownership shapes: top-level `threadId`,
/// `conversationId` on legacy approvals, and nested `thread.id` on `thread/started`. If
/// none is present, treat the notification as global.
pub(crate) fn sniff_thread_id(tagged: &Value) -> Option<&str> {
    let params = tagged.get("params")?;
    params
        .get("threadId")
        .or_else(|| params.get("conversationId"))
        .or_else(|| params.get("thread").and_then(|thread| thread.get("id")))
        .and_then(Value::as_str)
}

/// Server notifications unrelated to any thread, such as account state, processes, MCP
/// server lifecycle, and import tasks. Notifications with `Option<threadId>` fields
/// (`McpServerOauthLoginCompleted`, `McpServerStartupStatusUpdated`, and `Warning`) have
/// variants on both sides; `split_notification` chooses the scope from the runtime
/// value rather than the type.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "method", content = "params")]
pub enum GlobalNotification {
    #[serde(rename = "skills/changed")]
    SkillsChanged(SkillsChangedNotification),
    #[serde(rename = "project/changed")]
    ProjectChanged(ProjectChangedNotification),
    #[serde(rename = "command/exec/outputDelta")]
    CommandExecOutputDelta(CommandExecOutputDeltaNotification),
    #[serde(rename = "process/outputDelta")]
    ProcessOutputDelta(ProcessOutputDeltaNotification),
    #[serde(rename = "process/exited")]
    ProcessExited(ProcessExitedNotification),
    #[serde(rename = "mcpServer/oauthLogin/completed")]
    McpServerOauthLoginCompleted(McpServerOauthLoginCompletedNotification),
    #[serde(rename = "mcpServer/startupStatus/updated")]
    McpServerStartupStatusUpdated(McpServerStatusUpdatedNotification),
    #[serde(rename = "mcpServer/event/stream/notification")]
    McpServerEventStreamNotification(McpServerEventStreamNotification),
    #[serde(rename = "account/updated")]
    AccountUpdated(AccountUpdatedNotification),
    #[serde(rename = "account/rateLimits/updated")]
    AccountRateLimitsUpdated(AccountRateLimitsUpdatedNotification),
    #[serde(rename = "account/gatewayOAuth/changed")]
    AccountGatewayOAuthChanged(GatewayOAuthChangedNotification),
    #[serde(rename = "app/list/updated")]
    AppListUpdated(AppListUpdatedNotification),
    #[serde(rename = "remoteControl/status/changed")]
    RemoteControlStatusChanged(RemoteControlStatusChangedNotification),
    #[serde(rename = "externalAgentConfig/import/progress")]
    ExternalAgentConfigImportProgress(ExternalAgentConfigImportProgressNotification),
    #[serde(rename = "externalAgentConfig/import/completed")]
    ExternalAgentConfigImportCompleted(ExternalAgentConfigImportCompletedNotification),
    #[serde(rename = "fs/changed")]
    FsChanged(FsChangedNotification),
    #[serde(rename = "warning")]
    Warning(WarningNotification),
    #[serde(rename = "deprecationNotice")]
    DeprecationNotice(DeprecationNoticeNotification),
    #[serde(rename = "configWarning")]
    ConfigWarning(ConfigWarningNotification),
    #[serde(rename = "fuzzyFileSearch/sessionUpdated")]
    FuzzyFileSearchSessionUpdated(FuzzyFileSearchSessionUpdatedNotification),
    #[serde(rename = "fuzzyFileSearch/sessionCompleted")]
    FuzzyFileSearchSessionCompleted(FuzzyFileSearchSessionCompletedNotification),
    #[serde(rename = "windows/worldWritableWarning")]
    WindowsWorldWritableWarning(WindowsWorldWritableWarningNotification),
    #[serde(rename = "windowsSandbox/setupCompleted")]
    WindowsSandboxSetupCompleted(WindowsSandboxSetupCompletedNotification),
    #[serde(rename = "account/login/completed")]
    AccountLoginCompleted(AccountLoginCompletedNotification),
}

/// Server notifications owned by a thread, such as item/turn event streams, thread
/// lifecycle, and realtime sessions. `split_notification` reads ownership from each
/// payload; most use a top-level `threadId`, while `ThreadStarted` nests it in `thread.id`.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "method", content = "params")]
pub enum ThreadNotification {
    #[serde(rename = "error")]
    Error(ErrorNotification),
    #[serde(rename = "thread/started")]
    ThreadStarted(ThreadStartedNotification),
    #[serde(rename = "thread/status/changed")]
    ThreadStatusChanged(ThreadStatusChangedNotification),
    #[serde(rename = "thread/readState/changed")]
    ThreadReadStateChanged(ThreadReadStateChangedNotification),
    #[serde(rename = "thread/archived")]
    ThreadArchived(ThreadArchivedNotification),
    #[serde(rename = "thread/deleted")]
    ThreadDeleted(ThreadDeletedNotification),
    #[serde(rename = "thread/unarchived")]
    ThreadUnarchived(ThreadUnarchivedNotification),
    #[serde(rename = "thread/closed")]
    ThreadClosed(ThreadClosedNotification),
    #[serde(rename = "thread/attachment/updated")]
    ThreadAttachmentUpdated(ThreadAttachmentUpdatedNotification),
    #[serde(rename = "thread/reverted")]
    ThreadReverted(ThreadRevertedNotification),
    #[serde(rename = "thread/name/updated")]
    ThreadNameUpdated(ThreadNameUpdatedNotification),
    #[serde(rename = "thread/goal/updated")]
    ThreadGoalUpdated(ThreadGoalUpdatedNotification),
    #[serde(rename = "thread/prediction/updated")]
    ThreadPredictionUpdated(ThreadPredictionUpdatedNotification),
    #[serde(rename = "thread/goal/cleared")]
    ThreadGoalCleared(ThreadGoalClearedNotification),
    #[serde(rename = "thread/queue/changed")]
    ThreadQueueChanged(ThreadQueueChangedNotification),
    #[serde(rename = "thread/project/updated")]
    ThreadProjectUpdated(ThreadProjectUpdatedNotification),
    #[serde(rename = "thread/environment/connected")]
    ThreadEnvironmentConnected(EnvironmentConnectionNotification),
    #[serde(rename = "thread/environment/disconnected")]
    ThreadEnvironmentDisconnected(EnvironmentConnectionNotification),
    #[serde(rename = "thread/settings/updated")]
    ThreadSettingsUpdated(ThreadSettingsUpdatedNotification),
    #[serde(rename = "thread/tokenUsage/updated")]
    ThreadTokenUsageUpdated(ThreadTokenUsageUpdatedNotification),
    #[serde(rename = "turn/started")]
    TurnStarted(TurnStartedNotification),
    #[serde(rename = "hook/started")]
    HookStarted(HookStartedNotification),
    #[serde(rename = "turn/completed")]
    TurnCompleted(TurnCompletedNotification),
    #[serde(rename = "hook/completed")]
    HookCompleted(HookCompletedNotification),
    #[serde(rename = "turn/diff/updated")]
    TurnDiffUpdated(TurnDiffUpdatedNotification),
    #[serde(rename = "turn/plan/updated")]
    TurnPlanUpdated(TurnPlanUpdatedNotification),
    #[serde(rename = "item/started")]
    ItemStarted(ItemStartedNotification),
    #[serde(rename = "item/autoApprovalReview/started")]
    ItemAutoApprovalReviewStarted(ItemGuardianApprovalReviewStartedNotification),
    #[serde(rename = "item/autoApprovalReview/completed")]
    ItemAutoApprovalReviewCompleted(ItemGuardianApprovalReviewCompletedNotification),
    #[serde(rename = "autoApprovalReview/strictReviewRequired")]
    AutoApprovalReviewStrictReviewRequired(StrictReviewRequiredNotification),
    #[serde(rename = "item/completed")]
    ItemCompleted(ItemCompletedNotification),
    #[serde(rename = "item/agentMessage/delta")]
    ItemAgentMessageDelta(AgentMessageDeltaNotification),
    #[serde(rename = "item/plan/delta")]
    ItemPlanDelta(PlanDeltaNotification),
    #[serde(rename = "item/commandExecution/outputDelta")]
    ItemCommandExecutionOutputDelta(CommandExecutionOutputDeltaNotification),
    #[serde(rename = "item/commandExecution/terminalInteraction")]
    ItemCommandExecutionTerminalInteraction(TerminalInteractionNotification),
    #[serde(rename = "item/fileChange/outputDelta")]
    ItemFileChangeOutputDelta(FileChangeOutputDeltaNotification),
    #[serde(rename = "item/fileChange/patchUpdated")]
    ItemFileChangePatchUpdated(FileChangePatchUpdatedNotification),
    #[serde(rename = "serverRequest/resolved")]
    ServerRequestResolved(ServerRequestResolvedNotification),
    #[serde(rename = "item/mcpToolCall/progress")]
    ItemMcpToolCallProgress(McpToolCallProgressNotification),
    #[serde(rename = "mcpServer/oauthLogin/completed")]
    McpServerOauthLoginCompleted(McpServerOauthLoginCompletedNotification),
    #[serde(rename = "mcpServer/startupStatus/updated")]
    McpServerStartupStatusUpdated(McpServerStatusUpdatedNotification),
    #[serde(rename = "item/reasoning/summaryTextDelta")]
    ItemReasoningSummaryTextDelta(ReasoningSummaryTextDeltaNotification),
    #[serde(rename = "item/reasoning/summaryPartAdded")]
    ItemReasoningSummaryPartAdded(ReasoningSummaryPartAddedNotification),
    #[serde(rename = "item/reasoning/textDelta")]
    ItemReasoningTextDelta(ReasoningTextDeltaNotification),
    #[serde(rename = "thread/compacted")]
    ThreadCompacted(ContextCompactedNotification),
    #[serde(rename = "model/rerouted")]
    ModelRerouted(ModelReroutedNotification),
    #[serde(rename = "model/verification")]
    ModelVerification(ModelVerificationNotification),
    #[serde(rename = "modelProvider/authRecoveryStarted")]
    ModelProviderAuthRecoveryStarted(AuthRecoveryNotification),
    #[serde(rename = "modelProvider/authRecoveryCompleted")]
    ModelProviderAuthRecoveryCompleted(AuthRecoveryNotification),
    #[serde(rename = "turn/moderationMetadata")]
    TurnModerationMetadata(TurnModerationMetadataNotification),
    #[serde(rename = "model/safetyBuffering/updated")]
    ModelSafetyBufferingUpdated(ModelSafetyBufferingUpdatedNotification),
    #[serde(rename = "warning")]
    Warning(WarningNotification),
    #[serde(rename = "guardianWarning")]
    GuardianWarning(GuardianWarningNotification),
    #[serde(rename = "thread/realtime/started")]
    ThreadRealtimeStarted(ThreadRealtimeStartedNotification),
    #[serde(rename = "thread/realtime/itemAdded")]
    ThreadRealtimeItemAdded(ThreadRealtimeItemAddedNotification),
    #[serde(rename = "thread/realtime/item/started")]
    ThreadRealtimeItemStarted(ThreadRealtimeItemStartedNotification),
    #[serde(rename = "thread/realtime/item/transcript/delta")]
    ThreadRealtimeItemTranscriptDelta(ThreadRealtimeItemTranscriptDeltaNotification),
    #[serde(rename = "thread/realtime/item/completed")]
    ThreadRealtimeItemCompleted(ThreadRealtimeItemCompletedNotification),
    #[serde(rename = "thread/realtime/transcript/delta")]
    ThreadRealtimeTranscriptDelta(ThreadRealtimeTranscriptDeltaNotification),
    #[serde(rename = "thread/realtime/transcript/done")]
    ThreadRealtimeTranscriptDone(ThreadRealtimeTranscriptDoneNotification),
    #[serde(rename = "thread/realtime/outputAudio/delta")]
    ThreadRealtimeOutputAudioDelta(ThreadRealtimeOutputAudioDeltaNotification),
    #[serde(rename = "thread/realtime/sdp")]
    ThreadRealtimeSdp(ThreadRealtimeSdpNotification),
    #[serde(rename = "thread/realtime/error")]
    ThreadRealtimeError(ThreadRealtimeErrorNotification),
    #[serde(rename = "thread/realtime/closed")]
    ThreadRealtimeClosed(ThreadRealtimeClosedNotification),
}

/// The result of [`split_notification`].
#[allow(clippy::large_enum_variant)]
pub(crate) enum NotificationScope {
    Global(GlobalNotification),
    Thread {
        thread_id: String,
        notification: ThreadNotification,
    },
}

/// Split a server notification by thread ownership.
///
/// Uses an **exhaustive match** so adding a notification fails compilation until its
/// scope is chosen as `GlobalNotification` or `ThreadNotification`. Do not guess with
/// `params.get("threadId")`: legacy `ApplyPatchApproval` and `ExecCommandApproval` use
/// `conversationId`, and `ThreadStarted` nests its ID in `thread.id`.
pub(crate) fn split_notification(notification: ServerNotification) -> NotificationScope {
    match notification {
        ServerNotification::Error(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::Error(p),
            }
        }
        ServerNotification::ThreadStarted(p) => {
            let thread_id = p.thread.id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadStarted(p),
            }
        }
        ServerNotification::ThreadStatusChanged(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadStatusChanged(p),
            }
        }
        ServerNotification::ThreadReadStateChanged(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadReadStateChanged(p),
            }
        }
        ServerNotification::ThreadArchived(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadArchived(p),
            }
        }
        ServerNotification::ThreadDeleted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadDeleted(p),
            }
        }
        ServerNotification::ThreadUnarchived(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadUnarchived(p),
            }
        }
        ServerNotification::ThreadClosed(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadClosed(p),
            }
        }
        ServerNotification::ThreadAttachmentUpdated(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadAttachmentUpdated(p),
            }
        }
        ServerNotification::ThreadReverted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadReverted(p),
            }
        }
        ServerNotification::SkillsChanged(p) => {
            NotificationScope::Global(GlobalNotification::SkillsChanged(p))
        }
        ServerNotification::ThreadNameUpdated(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadNameUpdated(p),
            }
        }
        ServerNotification::ThreadGoalUpdated(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadGoalUpdated(p),
            }
        }
        ServerNotification::ThreadPredictionUpdated(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadPredictionUpdated(p),
            }
        }
        ServerNotification::ThreadGoalCleared(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadGoalCleared(p),
            }
        }
        ServerNotification::ThreadQueueChanged(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadQueueChanged(p),
            }
        }
        ServerNotification::ProjectChanged(p) => {
            NotificationScope::Global(GlobalNotification::ProjectChanged(p))
        }
        ServerNotification::ThreadProjectUpdated(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadProjectUpdated(p),
            }
        }
        ServerNotification::ThreadEnvironmentConnected(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadEnvironmentConnected(p),
            }
        }
        ServerNotification::ThreadEnvironmentDisconnected(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadEnvironmentDisconnected(p),
            }
        }
        ServerNotification::ThreadSettingsUpdated(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadSettingsUpdated(p),
            }
        }
        ServerNotification::ThreadTokenUsageUpdated(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadTokenUsageUpdated(p),
            }
        }
        ServerNotification::TurnStarted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::TurnStarted(p),
            }
        }
        ServerNotification::HookStarted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::HookStarted(p),
            }
        }
        ServerNotification::TurnCompleted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::TurnCompleted(p),
            }
        }
        ServerNotification::HookCompleted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::HookCompleted(p),
            }
        }
        ServerNotification::TurnDiffUpdated(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::TurnDiffUpdated(p),
            }
        }
        ServerNotification::TurnPlanUpdated(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::TurnPlanUpdated(p),
            }
        }
        ServerNotification::ItemStarted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemStarted(p),
            }
        }
        ServerNotification::ItemAutoApprovalReviewStarted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemAutoApprovalReviewStarted(p),
            }
        }
        ServerNotification::ItemAutoApprovalReviewCompleted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemAutoApprovalReviewCompleted(p),
            }
        }
        ServerNotification::AutoApprovalReviewStrictReviewRequired(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::AutoApprovalReviewStrictReviewRequired(p),
            }
        }
        ServerNotification::ItemCompleted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemCompleted(p),
            }
        }
        ServerNotification::ItemAgentMessageDelta(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemAgentMessageDelta(p),
            }
        }
        ServerNotification::ItemPlanDelta(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemPlanDelta(p),
            }
        }
        ServerNotification::CommandExecOutputDelta(p) => {
            NotificationScope::Global(GlobalNotification::CommandExecOutputDelta(p))
        }
        ServerNotification::ProcessOutputDelta(p) => {
            NotificationScope::Global(GlobalNotification::ProcessOutputDelta(p))
        }
        ServerNotification::ProcessExited(p) => {
            NotificationScope::Global(GlobalNotification::ProcessExited(p))
        }
        ServerNotification::ItemCommandExecutionOutputDelta(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemCommandExecutionOutputDelta(p),
            }
        }
        ServerNotification::ItemCommandExecutionTerminalInteraction(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemCommandExecutionTerminalInteraction(p),
            }
        }
        ServerNotification::ItemFileChangeOutputDelta(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemFileChangeOutputDelta(p),
            }
        }
        ServerNotification::ItemFileChangePatchUpdated(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemFileChangePatchUpdated(p),
            }
        }
        ServerNotification::ServerRequestResolved(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ServerRequestResolved(p),
            }
        }
        ServerNotification::ItemMcpToolCallProgress(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemMcpToolCallProgress(p),
            }
        }
        ServerNotification::McpServerOauthLoginCompleted(p) => match p.thread_id.clone() {
            Some(thread_id) => NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::McpServerOauthLoginCompleted(p),
            },
            None => NotificationScope::Global(GlobalNotification::McpServerOauthLoginCompleted(p)),
        },
        ServerNotification::McpServerStartupStatusUpdated(p) => match p.thread_id.clone() {
            Some(thread_id) => NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::McpServerStartupStatusUpdated(p),
            },
            None => NotificationScope::Global(GlobalNotification::McpServerStartupStatusUpdated(p)),
        },
        ServerNotification::McpServerEventStreamNotification(p) => {
            NotificationScope::Global(GlobalNotification::McpServerEventStreamNotification(p))
        }
        ServerNotification::AccountUpdated(p) => {
            NotificationScope::Global(GlobalNotification::AccountUpdated(p))
        }
        ServerNotification::AccountRateLimitsUpdated(p) => {
            NotificationScope::Global(GlobalNotification::AccountRateLimitsUpdated(p))
        }
        ServerNotification::AccountGatewayOAuthChanged(p) => {
            NotificationScope::Global(GlobalNotification::AccountGatewayOAuthChanged(p))
        }
        ServerNotification::AppListUpdated(p) => {
            NotificationScope::Global(GlobalNotification::AppListUpdated(p))
        }
        ServerNotification::RemoteControlStatusChanged(p) => {
            NotificationScope::Global(GlobalNotification::RemoteControlStatusChanged(p))
        }
        ServerNotification::ExternalAgentConfigImportProgress(p) => {
            NotificationScope::Global(GlobalNotification::ExternalAgentConfigImportProgress(p))
        }
        ServerNotification::ExternalAgentConfigImportCompleted(p) => {
            NotificationScope::Global(GlobalNotification::ExternalAgentConfigImportCompleted(p))
        }
        ServerNotification::FsChanged(p) => {
            NotificationScope::Global(GlobalNotification::FsChanged(p))
        }
        ServerNotification::ItemReasoningSummaryTextDelta(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemReasoningSummaryTextDelta(p),
            }
        }
        ServerNotification::ItemReasoningSummaryPartAdded(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemReasoningSummaryPartAdded(p),
            }
        }
        ServerNotification::ItemReasoningTextDelta(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ItemReasoningTextDelta(p),
            }
        }
        ServerNotification::ThreadCompacted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadCompacted(p),
            }
        }
        ServerNotification::ModelRerouted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ModelRerouted(p),
            }
        }
        ServerNotification::ModelVerification(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ModelVerification(p),
            }
        }
        ServerNotification::ModelProviderAuthRecoveryStarted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ModelProviderAuthRecoveryStarted(p),
            }
        }
        ServerNotification::ModelProviderAuthRecoveryCompleted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ModelProviderAuthRecoveryCompleted(p),
            }
        }
        ServerNotification::TurnModerationMetadata(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::TurnModerationMetadata(p),
            }
        }
        ServerNotification::ModelSafetyBufferingUpdated(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ModelSafetyBufferingUpdated(p),
            }
        }
        ServerNotification::Warning(p) => match p.thread_id.clone() {
            Some(thread_id) => NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::Warning(p),
            },
            None => NotificationScope::Global(GlobalNotification::Warning(p)),
        },
        ServerNotification::GuardianWarning(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::GuardianWarning(p),
            }
        }
        ServerNotification::DeprecationNotice(p) => {
            NotificationScope::Global(GlobalNotification::DeprecationNotice(p))
        }
        ServerNotification::ConfigWarning(p) => {
            NotificationScope::Global(GlobalNotification::ConfigWarning(p))
        }
        ServerNotification::FuzzyFileSearchSessionUpdated(p) => {
            NotificationScope::Global(GlobalNotification::FuzzyFileSearchSessionUpdated(p))
        }
        ServerNotification::FuzzyFileSearchSessionCompleted(p) => {
            NotificationScope::Global(GlobalNotification::FuzzyFileSearchSessionCompleted(p))
        }
        ServerNotification::ThreadRealtimeStarted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadRealtimeStarted(p),
            }
        }
        ServerNotification::ThreadRealtimeItemAdded(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadRealtimeItemAdded(p),
            }
        }
        ServerNotification::ThreadRealtimeItemStarted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadRealtimeItemStarted(p),
            }
        }
        ServerNotification::ThreadRealtimeItemTranscriptDelta(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadRealtimeItemTranscriptDelta(p),
            }
        }
        ServerNotification::ThreadRealtimeItemCompleted(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadRealtimeItemCompleted(p),
            }
        }
        ServerNotification::ThreadRealtimeTranscriptDelta(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadRealtimeTranscriptDelta(p),
            }
        }
        ServerNotification::ThreadRealtimeTranscriptDone(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadRealtimeTranscriptDone(p),
            }
        }
        ServerNotification::ThreadRealtimeOutputAudioDelta(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadRealtimeOutputAudioDelta(p),
            }
        }
        ServerNotification::ThreadRealtimeSdp(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadRealtimeSdp(p),
            }
        }
        ServerNotification::ThreadRealtimeError(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadRealtimeError(p),
            }
        }
        ServerNotification::ThreadRealtimeClosed(p) => {
            let thread_id = p.thread_id.clone();
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadRealtimeClosed(p),
            }
        }
        ServerNotification::WindowsWorldWritableWarning(p) => {
            NotificationScope::Global(GlobalNotification::WindowsWorldWritableWarning(p))
        }
        ServerNotification::WindowsSandboxSetupCompleted(p) => {
            NotificationScope::Global(GlobalNotification::WindowsSandboxSetupCompleted(p))
        }
        ServerNotification::AccountLoginCompleted(p) => {
            NotificationScope::Global(GlobalNotification::AccountLoginCompleted(p))
        }
    }
}

/// Server requests unrelated to any thread: MCP elicitation, ChatGPT token refresh, and
/// attestation generation.
#[derive(Clone, Debug)]
pub enum GlobalRequest {
    McpServerElicitationRequest(McpServerElicitationRequestParams),
    AccountChatgptAuthTokensRefresh(ChatgptAuthTokensRefreshParams),
    AttestationGenerate(AttestationGenerateParams),
}

/// Server requests owned by a thread: five approval requests, `item/tool/requestUserInput`,
/// and `item/tool/call`. The two legacy approvals (`ApplyPatchApproval` and
/// `ExecCommandApproval`) use `conversationId` rather than `threadId`; `split_request`
/// reads their typed fields and is unaffected by field renames.
#[derive(Clone, Debug)]
pub enum ThreadRequest {
    ItemCommandExecutionRequestApproval(CommandExecutionRequestApprovalParams),
    ItemFileChangeRequestApproval(FileChangeRequestApprovalParams),
    ItemToolRequestUserInput(ToolRequestUserInputParams),
    ItemPermissionsRequestApproval(PermissionsRequestApprovalParams),
    ItemToolCall(DynamicToolCallParams),
    ApplyPatchApproval(ApplyPatchApprovalParams),
    ExecCommandApproval(ExecCommandApprovalParams),
}

/// The result of [`split_request`].
pub(crate) enum RequestScope {
    Global(GlobalRequest),
    Thread {
        thread_id: String,
        request: ThreadRequest,
    },
}

/// Split a server request by thread ownership. This is exhaustive for the same reason as
/// [`split_notification`].
pub(crate) fn split_request(request: ServerRequest) -> RequestScope {
    match request {
        ServerRequest::ItemCommandExecutionRequestApproval(p) => {
            let thread_id = p.thread_id.clone();
            RequestScope::Thread {
                thread_id,
                request: ThreadRequest::ItemCommandExecutionRequestApproval(p),
            }
        }
        ServerRequest::ItemFileChangeRequestApproval(p) => {
            let thread_id = p.thread_id.clone();
            RequestScope::Thread {
                thread_id,
                request: ThreadRequest::ItemFileChangeRequestApproval(p),
            }
        }
        ServerRequest::ItemToolRequestUserInput(p) => {
            let thread_id = p.thread_id.clone();
            RequestScope::Thread {
                thread_id,
                request: ThreadRequest::ItemToolRequestUserInput(p),
            }
        }
        ServerRequest::McpServerElicitationRequest(p) => {
            RequestScope::Global(GlobalRequest::McpServerElicitationRequest(p))
        }
        ServerRequest::ItemPermissionsRequestApproval(p) => {
            let thread_id = p.thread_id.clone();
            RequestScope::Thread {
                thread_id,
                request: ThreadRequest::ItemPermissionsRequestApproval(p),
            }
        }
        ServerRequest::ItemToolCall(p) => {
            let thread_id = p.thread_id.clone();
            RequestScope::Thread {
                thread_id,
                request: ThreadRequest::ItemToolCall(p),
            }
        }
        ServerRequest::AccountChatgptAuthTokensRefresh(p) => {
            RequestScope::Global(GlobalRequest::AccountChatgptAuthTokensRefresh(p))
        }
        ServerRequest::AttestationGenerate(p) => {
            RequestScope::Global(GlobalRequest::AttestationGenerate(p))
        }
        ServerRequest::ApplyPatchApproval(p) => {
            let thread_id = p.conversation_id.clone();
            RequestScope::Thread {
                thread_id,
                request: ThreadRequest::ApplyPatchApproval(p),
            }
        }
        ServerRequest::ExecCommandApproval(p) => {
            let thread_id = p.conversation_id.clone();
            RequestScope::Thread {
                thread_id,
                request: ThreadRequest::ExecCommandApproval(p),
            }
        }
    }
}

/// The protocol method name of a server request.
///
/// Uses an exhaustive match so adding a server request **fails compilation** here. Add a
/// corresponding entry to [`decline_by_method`] as well; that string-dispatched table
/// would otherwise silently fall back to a JSON-RPC error instead of a useful refusal.
pub(crate) const fn method_of(request: &ServerRequest) -> &'static str {
    match request {
        ServerRequest::ItemCommandExecutionRequestApproval(_) => {
            "item/commandExecution/requestApproval"
        }
        ServerRequest::ItemFileChangeRequestApproval(_) => "item/fileChange/requestApproval",
        ServerRequest::ItemPermissionsRequestApproval(_) => "item/permissions/requestApproval",
        ServerRequest::ApplyPatchApproval(_) => "applyPatchApproval",
        ServerRequest::ExecCommandApproval(_) => "execCommandApproval",
        ServerRequest::ItemToolRequestUserInput(_) => "item/tool/requestUserInput",
        ServerRequest::ItemToolCall(_) => "item/tool/call",
        ServerRequest::McpServerElicitationRequest(_) => "mcpServer/elicitation/request",
        ServerRequest::AccountChatgptAuthTokensRefresh(_) => "account/chatgptAuthTokens/refresh",
        ServerRequest::AttestationGenerate(_) => "attestation/generate",
    }
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
/// Timeouts, requests with no claiming turn, unrecognized methods, and dropped one-shot
/// reply senders all use this path and share the same table. Two separate handwritten
/// `json!` tables once omitted the required `rejection` field from legacy approval
/// refusals. The server could not parse those responses, leaving turns blocked.
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

/// Reconnect backoff: exponential growth capped at the limit, plus jitter over half the
/// cap's width.
///
/// The lower bound is `cap/2`, not zero. Full jitter has a growing expected delay but can
/// still return 1 ms, causing rapid retries while the server is down. Jitter should spread
/// clients out without eliminating the backoff itself.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::ErrorObject;
    use serde::de::DeserializeOwned;

    #[test]
    fn unknown_notifications_are_reported_as_unparsed_not_as_errors() {
        // The server may add notifications at any time. Return None for unknown messages
        // so the caller can preserve the original JSON; never let one unrecognized
        // notification take down the connection.
        assert_eq!(
            parse_notification(&tagged(
                "some/notification/from/the/future",
                json!({"whatever": true})
            )),
            None
        );
    }

    #[test]
    fn typed_notifications_carry_their_payload() {
        let parsed = parse_notification(&tagged(
            "turn/completed",
            json!({"threadId":"t","turn":{"id":"u","items":[],"status":"completed"}}),
        ))
        .expect("known notification must parse successfully");
        match parsed {
            ServerNotification::TurnCompleted(payload) => {
                assert_eq!(payload.thread_id, "t");
                assert_eq!(payload.turn.id, "u");
            }
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    /// Every reply must deserialize as the generated response type for its request.
    ///
    /// This is the only glue between handwritten JSON and the schema, and mismatches can
    /// occur here: a legacy approval fallback once omitted the required `rejection` field.
    /// The server could not parse it, leaving the turn blocked.
    #[test]
    fn every_reply_matches_its_generated_response_type() {
        fn accepts<R: DeserializeOwned>(what: &str, reply: Reply) {
            match reply {
                Reply::Result(value) => {
                    if let Err(error) = serde_json::from_value::<R>(value.clone()) {
                        panic!("{what} reply {value} does not match its response type: {error}");
                    }
                }
                Reply::Error(error) => {
                    panic!("{what} should have a successful reply, got {error:?}")
                }
            }
        }

        // Unclaimed requests, unparseable requests, and caller-initiated refusals all use
        // the same table.
        accepts::<CommandExecutionRequestApprovalResponse>(
            "fallback",
            decline_by_method("item/commandExecution/requestApproval"),
        );
        accepts::<FileChangeRequestApprovalResponse>(
            "fallback",
            decline_by_method("item/fileChange/requestApproval"),
        );
        accepts::<PermissionsRequestApprovalResponse>(
            "fallback",
            decline_by_method("item/permissions/requestApproval"),
        );
        accepts::<ApplyPatchApprovalResponse>("fallback", decline_by_method("applyPatchApproval"));
        accepts::<ExecCommandApprovalResponse>(
            "fallback",
            decline_by_method("execCommandApproval"),
        );
        accepts::<ToolRequestUserInputResponse>(
            "fallback",
            decline_by_method("item/tool/requestUserInput"),
        );
        accepts::<DynamicToolCallResponse>("fallback", decline_by_method("item/tool/call"));
        accepts::<McpServerElicitationRequestResponse>(
            "fallback",
            decline_by_method("mcpServer/elicitation/request"),
        );

        // The two requests requiring real credentials have no refusal response; return an
        // explicit error instead.
        for method in ["attestation/generate", "account/chatgptAuthTokens/refresh"] {
            assert!(
                matches!(
                    decline_by_method(method),
                    Reply::Error(ErrorObject { code, .. })
                        if code == crate::envelope::METHOD_NOT_SUPPORTED
                ),
                "{method} must return an error instead of fabricating a result"
            );
        }
        // Never fabricate a result for a completely unknown method either.
        assert!(matches!(
            decline_by_method("some/method/from/the/future"),
            Reply::Error(_)
        ));
    }

    /// `decline_by_method` must handle every method recognized by `method_of`.
    ///
    /// One is an exhaustive match and the other a string table. The match fails to compile
    /// when the protocol adds a request; the table does not. This test catches cases where
    /// only one of them was updated.
    #[test]
    fn the_decline_table_covers_every_method_the_client_can_name() {
        const ALL: [&str; 10] = [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
            "item/permissions/requestApproval",
            "applyPatchApproval",
            "execCommandApproval",
            "item/tool/requestUserInput",
            "item/tool/call",
            "mcpServer/elicitation/request",
            "account/chatgptAuthTokens/refresh",
            "attestation/generate",
        ];
        for method in ALL {
            let generic = format!("client cannot answer {method}");
            assert!(
                !matches!(
                    decline_by_method(method),
                    Reply::Error(ErrorObject { ref message, .. }) if *message == generic
                ),
                "{method} reached the generic fallback; the refusal table is missing an entry"
            );
        }
    }

    #[test]
    fn backoff_grows_and_keeps_a_floor() {
        for (attempt, low, high) in [(0, 250, 500), (3, 2000, 4000), (20, 15000, 30000)] {
            let delay = reconnect_delay(attempt, 30_000).as_millis();
            assert!(
                (low..=high).contains(&delay),
                "reconnect backoff #{attempt} of {delay}ms is outside {low}–{high}ms"
            );
        }
    }

    /// Legacy approvals use `conversationId`, not `threadId`. The old implementation
    /// inferred ownership with `params.get("threadId")` and always classified these
    /// requests as global. Exhaustive matches read typed fields and are unaffected by
    /// field-name changes.
    #[test]
    fn legacy_approvals_route_by_conversation_id_not_thread_id() {
        let request = parse_server_request(&tagged(
            "applyPatchApproval",
            json!({
                "callId": "c", "conversationId": "thread-legacy",
                "fileChanges": {}
            }),
        ))
        .expect("legacy applyPatchApproval must parse successfully");
        match split_request(request) {
            RequestScope::Thread { thread_id, request } => {
                assert_eq!(thread_id, "thread-legacy");
                assert!(matches!(request, ThreadRequest::ApplyPatchApproval(_)));
            }
            RequestScope::Global(_) => {
                panic!("a request with conversationId must not be classified as global")
            }
        }

        let request = parse_server_request(&tagged(
            "execCommandApproval",
            json!({
                "callId": "c", "command": ["echo"], "conversationId": "thread-legacy",
                "cwd": "/tmp", "parsedCmd": []
            }),
        ))
        .expect("legacy execCommandApproval must parse successfully");
        match split_request(request) {
            RequestScope::Thread { thread_id, request } => {
                assert_eq!(thread_id, "thread-legacy");
                assert!(matches!(request, ThreadRequest::ExecCommandApproval(_)));
            }
            RequestScope::Global(_) => {
                panic!("a request with conversationId must not be classified as global")
            }
        }
    }

    /// `thread/started` has no top-level `threadId`; its ID is nested in `thread.id`.
    #[test]
    fn thread_started_routes_by_the_nested_thread_id() {
        let notification = parse_notification(&tagged(
            "thread/started",
            json!({"thread": {
                "id": "thread-nested", "sessionId": "thread-nested", "cwd": "/tmp",
                "createdAt": 0, "updatedAt": 0, "recencyAt": 0,
                "cliVersion": "0.153.0", "modelProvider": "openai",
                "preview": "", "ephemeral": false, "historyMode": "paginated",
                "source": "vscode", "status": {"type": "idle"},
                "path": "/tmp/rollout.jsonl", "canAcceptDirectInput": true,
                "turns": []
            }}),
        ))
        .expect("thread/started must parse successfully");
        match split_notification(notification) {
            NotificationScope::Thread {
                thread_id,
                notification,
            } => {
                assert_eq!(thread_id, "thread-nested");
                assert!(matches!(notification, ThreadNotification::ThreadStarted(_)));
            }
            NotificationScope::Global(_) => {
                panic!("a notification with thread.id must not be classified as global")
            }
        }
    }

    #[test]
    fn new_attachment_and_gateway_notifications_keep_their_scope() {
        let attachment = parse_notification(&tagged(
            "thread/attachment/updated",
            json!({
                "attachmentId": "attachment-1",
                "attachmentType": "file",
                "identityKey": "key-1",
                "operation": "created",
                "threadId": "thread-attachment"
            }),
        ))
        .expect("thread attachment notification must parse successfully");
        assert!(matches!(
            split_notification(attachment),
            NotificationScope::Thread {
                thread_id,
                notification: ThreadNotification::ThreadAttachmentUpdated(_)
            } if thread_id == "thread-attachment"
        ));

        let gateway = parse_notification(&tagged(
            "account/gatewayOAuth/changed",
            json!({ "providerId": "provider-1", "status": "started" }),
        ))
        .expect("gateway OAuth notification must parse successfully");
        assert!(matches!(
            split_notification(gateway),
            NotificationScope::Global(GlobalNotification::AccountGatewayOAuthChanged(_))
        ));
    }

    /// Three notifications use `Option<threadId>`: a value assigns them to that thread;
    /// no value makes them global. The same notification type can be routed either way,
    /// based on its runtime value rather than its static type.
    #[test]
    fn notifications_with_optional_thread_id_split_by_value() {
        let with_thread = parse_notification(&tagged(
            "warning",
            json!({"message": "m", "threadId": "thread-x"}),
        ))
        .unwrap();
        assert!(matches!(
            split_notification(with_thread),
            NotificationScope::Thread { thread_id, .. } if thread_id == "thread-x"
        ));

        let without_thread =
            parse_notification(&tagged("warning", json!({"message": "m"}))).unwrap();
        assert!(matches!(
            split_notification(without_thread),
            NotificationScope::Global(GlobalNotification::Warning(_))
        ));
    }

    /// For an unknown notification, ownership inference must recognize `conversationId`
    /// and nested `thread.id`, not only `threadId`.
    #[test]
    fn sniffing_an_unknown_notification_tries_all_three_known_shapes() {
        let flat = tagged("future/notification", json!({"threadId": "t1"}));
        assert_eq!(sniff_thread_id(&flat), Some("t1"));

        let legacy = tagged("future/legacyApproval", json!({"conversationId": "t2"}));
        assert_eq!(sniff_thread_id(&legacy), Some("t2"));

        let nested = tagged("future/threadEvent", json!({"thread": {"id": "t3"}}));
        assert_eq!(sniff_thread_id(&nested), Some("t3"));

        let unrelated = tagged("future/globalEvent", json!({"data": 1}));
        assert_eq!(sniff_thread_id(&unrelated), None);
    }
}
