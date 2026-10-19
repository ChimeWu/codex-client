//! 入站消息的类型化解析、按 thread 归属的拆分，以及服务端请求的应答构造。
//!
//! # 应答只从生成的响应类型来
//!
//! 服务端请求的每一种都有 schema 生成的响应类型（`CommandExecutionRequestApprovalResponse`、
//! `PermissionsRequestApprovalResponse`……）。本模块**只**用那些类型构造应答，
//! 不手写 `json!`：手写的形状没有编译期约束，schema 一变就悄悄发出服务端解析
//! 不了的应答，而服务端解析不了就等于没收到——那个 turn 会永久卡住。
//!
//! # 按 thread 归属拆分
//!
//! [`split_notification`]/[`split_request`] 把生成的 `ServerNotification`/
//! `ServerRequest` 按「属于某个 thread」还是「与任何 thread 都无关」拆成
//! [`ThreadNotification`]/[`GlobalNotification`]、[`ThreadRequest`]/
//! [`GlobalRequest`] 四个更窄的类型，供 [`crate::client::inbound::ThreadMessage`]/
//! [`crate::client::inbound::GlobalMessage`] 使用。两个函数都是穷尽 match，协议
//! 新增一种通知/请求时这里会编译不过——这不是巧合：曾经按
//! `params.get("threadId")` 猜归属，猜漏了两类真实存在的协议形状——legacy 审批
//! （`ApplyPatchApproval`/`ExecCommandApproval`）的归属字段叫 `conversationId`，
//! `ThreadStarted` 的 id 嵌在 `thread.id` 里、顶层根本没有 `threadId`——这两类
//! 消息因此永远进不了它们本该进的 thread 通道，只会静默漏到全局流。穷尽 match
//! 从类型化字段取值，不会再犯同样的错，也不会在协议新增字段形状时悄悄漏第三次。

use crate::envelope::Reply;
use crate::generated::events::{ServerNotification, ServerRequest};
use crate::generated::types::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

/// 把信封拆出来的 `method` / `params` 重新装回带标签的形状。
///
/// 生成的 `ServerNotification` / `ServerRequest` 是 `#[serde(tag = "method",
/// content = "params")]`，解析它们需要这个形状。
pub(crate) fn tagged(method: &str, params: Value) -> Value {
    json!({ "method": method, "params": params })
}

/// 解析一条服务端通知。
///
/// 解析失败**不是**错误：服务端随时可能新增字段或新增通知，一条读不懂的通知
/// 绝不该让整条连接失败。读不懂时返回 `None`，调用方按「未知通知」处理即可。
///
/// 从 `&Value` 反序列化而不是 `from_value(value.clone())`：失败路径要把原样的
/// JSON 交出去，成功路径不该为此先复制一份整棵树。
pub(crate) fn parse_notification(tagged: &Value) -> Option<ServerNotification> {
    ServerNotification::deserialize(tagged).ok()
}

/// 解析一条服务端请求。
///
/// 同样是尽力而为，但后果不同：读不懂的请求**仍然必须回复**，否则服务端会一直
/// 阻塞。返回 `None` 时调用方应当按方法名走 [`decline_by_method`]。
pub(crate) fn parse_server_request(tagged: &Value) -> Option<ServerRequest> {
    ServerRequest::deserialize(tagged).ok()
}

/// 对本客户端解析不出来的通知，尽力猜一个 thread 归属。
///
/// 只有 `parse_notification` 失败——协议新增了本客户端还不认识的通知——时才用
/// 得到：已知的通知一律走 [`split_notification`] 的类型化字段，不靠猜。接的是
/// [`tagged`] 装出来的 `{method, params}` 形状；三种已知归属形状挨个试：多数
/// 通知的顶层 `threadId`，legacy 审批系的 `conversationId`，以及
/// `thread/started` 那种嵌在 `thread.id` 里的。都没有就说明这条通知本来就与
/// 任何 thread 无关，交给全局流。
pub(crate) fn sniff_thread_id(tagged: &Value) -> Option<&str> {
    let params = tagged.get("params")?;
    params
        .get("threadId")
        .or_else(|| params.get("conversationId"))
        .or_else(|| params.get("thread").and_then(|thread| thread.get("id")))
        .and_then(Value::as_str)
}

/// 与任何 thread 都无关的服务端通知：账号、进程、MCP 服务器生命周期、导入
/// 任务等。三种字段里带 `Option<threadId>` 的通知（`McpServerOauthLoginCompleted`、
/// `McpServerStartupStatusUpdated`、`Warning`）两边都有变体——具体某一条落在哪
/// 边由 [`split_notification`] 在运行时按值决定，不是由类型决定。
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

/// 属于某个 thread 的服务端通知：item/turn 级事件流、thread 生命周期、
/// realtime 会话等。归属由 [`split_notification`] 从各自的字段里取——多数是
/// 顶层 `threadId`，`ThreadStarted` 是嵌在 `thread.id` 里的例外。
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

/// [`split_notification`] 的判定结果。
#[allow(clippy::large_enum_variant)]
pub(crate) enum NotificationScope {
    Global(GlobalNotification),
    Thread {
        thread_id: String,
        notification: ThreadNotification,
    },
}

/// 按 thread 归属拆分一条服务端通知。
///
/// **穷尽 match**：协议新增通知时这里会编译不过，逼着调用方显式决定新通知该
/// 进 `GlobalNotification` 还是 `ThreadNotification`——而不是像原来那样按
/// `params.get("threadId")` 猜，猜漏的两个真实教训是 `ApplyPatchApproval`/
/// `ExecCommandApproval` 用的是 `conversationId` 而不是 `threadId`，
/// `ThreadStarted` 的 id 嵌在 `thread.id` 里、顶层根本没有 `threadId` 字段。
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

/// 与任何 thread 都无关的服务端请求：MCP elicitation、ChatGPT token 刷新、
/// attestation 生成。
#[derive(Clone, Debug)]
pub enum GlobalRequest {
    McpServerElicitationRequest(McpServerElicitationRequestParams),
    AccountChatgptAuthTokensRefresh(ChatgptAuthTokensRefreshParams),
    AttestationGenerate(AttestationGenerateParams),
}

/// 属于某个 thread 的服务端请求：五种审批 + `item/tool/requestUserInput` +
/// `item/tool/call`。legacy 的两种（`ApplyPatchApproval`/`ExecCommandApproval`）
/// 归属字段叫 `conversationId`，不叫 `threadId`——[`split_request`] 直接从
/// 各自的类型化字段取值，不受字段改名影响。
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

/// [`split_request`] 的判定结果。
pub(crate) enum RequestScope {
    Global(GlobalRequest),
    Thread {
        thread_id: String,
        request: ThreadRequest,
    },
}

/// 按 thread 归属拆分一条服务端请求。穷尽 match，理由同 [`split_notification`]。
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

/// 一条服务端请求的协议方法名。
///
/// 穷尽 match：协议新增一种服务端请求时这里会**编译不过**。届时
/// [`decline_by_method`] 也必须补一条——那张表按字符串分派，漏了不会编译不过，
/// 只会在运行时给出一个 JSON-RPC error 而不是一次得体的拒绝。
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

/// 兜底拒绝的理由，会随协议一起交给 agent（legacy 审批的 `rejection` 是必填
/// 字段）。这是**客户端自己**给不出决定时说的话，不是用户说的话——用户的措辞
/// 由调用方随它的响应类型一起给出。
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

/// 对一条服务端请求的保守应答：本客户端答不了、或者没人来答时用它。
///
/// 超时、没有 turn 认领、方法名解析不出来，以及 oneshot reply sender 被丢弃时
/// 的拒绝都走这里——这些路径共用一张表，不会错开。
/// 曾经有过两张手写的 `json!` 表，其中 legacy 审批的兜底拒绝少了必填的
/// `rejection` 字段，服务端解析不了等于没收到，turn 永久卡住。
///
/// 按方法名而不是按请求类型分派：解析不出来的请求也必须应答，那时手里只有
/// 名字。认不出来就回 JSON-RPC error——编造一个 result 会让服务端拿到假数据。
///
/// 审批一律 `decline` 而不是 `cancel`：前者让 agent 知道被拒后继续这个 turn
/// 去找替代路径，后者会直接中断整个 turn。
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
        // 下面这些不是二元决定，本客户端拿不出内容，只能给出「什么也没答」。
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
        // 这两个要求客户端拿出真实凭据，不存在「拒绝」这一说。
        "account/chatgptAuthTokens/refresh" => {
            Reply::unsupported("client cannot refresh ChatGPT auth tokens")
        }
        "attestation/generate" => Reply::unsupported("client cannot generate attestations"),
        other => Reply::unsupported(format!("client cannot answer {other}")),
    }
}

/// 重连退避：指数增长到上限，再叠加一半宽度的随机抖动。
///
/// 下界是 `cap/2` 而不是 0——「全抖动」写法期望值虽然也在增长，但单次可以退回
/// 到 1 毫秒，服务端没起来时会变成高频重试。抖动只是错开多个客户端，不该把退避
/// 本身抹掉。
pub fn reconnect_delay(attempt: u32, max_ms: u64) -> Duration {
    let cap = 500u64
        .saturating_mul(2u64.saturating_pow(attempt.min(16)))
        .min(max_ms)
        .max(2);
    let half = cap / 2;
    // 抖动取自时钟低位。它不是随机数发生器，但用途只是让同时重连的多个客户端
    // 错开，而退避的下界已经由 `half` 保证了。
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
        // 服务端随时可能新增通知。读不懂时返回 None，由调用方原样保留原始
        // JSON——绝不能让一条读不懂的通知拖垮整条连接。
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
        .expect("已知通知必须解析成功");
        match parsed {
            ServerNotification::TurnCompleted(payload) => {
                assert_eq!(payload.thread_id, "t");
                assert_eq!(payload.turn.id, "u");
            }
            other => panic!("解析成了 {other:?}"),
        }
    }

    /// 每一条应答都必须能被它那条请求的生成响应类型接住。
    ///
    /// 这是手写 JSON 与 schema 之间唯一的粘合点，也只有这里会悄悄错开：legacy
    /// 审批的兜底拒绝就曾经少了必填的 `rejection` 字段，服务端解析不了等于没
    /// 收到，turn 永久卡住。
    #[test]
    fn every_reply_matches_its_generated_response_type() {
        fn accepts<R: DeserializeOwned>(what: &str, reply: Reply) {
            match reply {
                Reply::Result(value) => {
                    if let Err(error) = serde_json::from_value::<R>(value.clone()) {
                        panic!("{what} 的应答 {value} 不符合它的响应类型：{error}");
                    }
                }
                Reply::Error(error) => panic!("{what} 应当有一个成功应答，却给了 {error:?}"),
            }
        }

        // 无人认领、解析不出请求、调用方主动拒绝——三条路径同一张表。
        accepts::<CommandExecutionRequestApprovalResponse>(
            "兜底",
            decline_by_method("item/commandExecution/requestApproval"),
        );
        accepts::<FileChangeRequestApprovalResponse>(
            "兜底",
            decline_by_method("item/fileChange/requestApproval"),
        );
        accepts::<PermissionsRequestApprovalResponse>(
            "兜底",
            decline_by_method("item/permissions/requestApproval"),
        );
        accepts::<ApplyPatchApprovalResponse>("兜底", decline_by_method("applyPatchApproval"));
        accepts::<ExecCommandApprovalResponse>("兜底", decline_by_method("execCommandApproval"));
        accepts::<ToolRequestUserInputResponse>(
            "兜底",
            decline_by_method("item/tool/requestUserInput"),
        );
        accepts::<DynamicToolCallResponse>("兜底", decline_by_method("item/tool/call"));
        accepts::<McpServerElicitationRequestResponse>(
            "兜底",
            decline_by_method("mcpServer/elicitation/request"),
        );

        // 要求真实凭据的两个请求不存在「拒绝」，只能明确报错。
        for method in ["attestation/generate", "account/chatgptAuthTokens/refresh"] {
            assert!(
                matches!(
                    decline_by_method(method),
                    Reply::Error(ErrorObject { code, .. })
                        if code == crate::envelope::METHOD_NOT_SUPPORTED
                ),
                "{method} 必须报错而不是编造 result"
            );
        }
        // 完全不认识的方法同样不许编造 result。
        assert!(matches!(
            decline_by_method("some/method/from/the/future"),
            Reply::Error(_)
        ));
    }

    /// `method_of` 认得的每一个名字，`decline_by_method` 都必须认得。
    ///
    /// 两者一个是穷尽 match、一个是字符串表：前者会因为协议新增请求而编译不过，
    /// 后者不会。这条测试是把「补了一张、忘了另一张」变成红灯的地方。
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
                "{method} 落到了通用兜底，说明拒绝表漏了它"
            );
        }
    }

    #[test]
    fn backoff_grows_and_keeps_a_floor() {
        for (attempt, low, high) in [(0, 250, 500), (3, 2000, 4000), (20, 15000, 30000)] {
            let delay = reconnect_delay(attempt, 30_000).as_millis();
            assert!(
                (low..=high).contains(&delay),
                "第 {attempt} 次重连退避 {delay}ms 落在 {low}–{high}ms 之外"
            );
        }
    }

    /// legacy 审批的归属字段叫 `conversationId`，不叫 `threadId`。旧实现按
    /// `params.get("threadId")` 猜归属，会把这两种请求永远判成「与任何 thread
    /// 都无关」——穷尽 match 从类型化字段取值，不受字段改名影响。
    #[test]
    fn legacy_approvals_route_by_conversation_id_not_thread_id() {
        let request = parse_server_request(&tagged(
            "applyPatchApproval",
            json!({
                "callId": "c", "conversationId": "thread-legacy",
                "fileChanges": {}
            }),
        ))
        .expect("legacy applyPatchApproval 必须解析成功");
        match split_request(request) {
            RequestScope::Thread { thread_id, request } => {
                assert_eq!(thread_id, "thread-legacy");
                assert!(matches!(request, ThreadRequest::ApplyPatchApproval(_)));
            }
            RequestScope::Global(_) => {
                panic!("conversationId 存在时不该判成全局请求")
            }
        }

        let request = parse_server_request(&tagged(
            "execCommandApproval",
            json!({
                "callId": "c", "command": ["echo"], "conversationId": "thread-legacy",
                "cwd": "/tmp", "parsedCmd": []
            }),
        ))
        .expect("legacy execCommandApproval 必须解析成功");
        match split_request(request) {
            RequestScope::Thread { thread_id, request } => {
                assert_eq!(thread_id, "thread-legacy");
                assert!(matches!(request, ThreadRequest::ExecCommandApproval(_)));
            }
            RequestScope::Global(_) => {
                panic!("conversationId 存在时不该判成全局请求")
            }
        }
    }

    /// `thread/started` 顶层没有 `threadId`，id 嵌在 `thread.id` 里。
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
        .expect("thread/started 必须解析成功");
        match split_notification(notification) {
            NotificationScope::Thread {
                thread_id,
                notification,
            } => {
                assert_eq!(thread_id, "thread-nested");
                assert!(matches!(notification, ThreadNotification::ThreadStarted(_)));
            }
            NotificationScope::Global(_) => {
                panic!("thread.id 存在时不该判成全局通知")
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
        .expect("thread attachment 通知必须解析成功");
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
        .expect("gateway OAuth 通知必须解析成功");
        assert!(matches!(
            split_notification(gateway),
            NotificationScope::Global(GlobalNotification::AccountGatewayOAuthChanged(_))
        ));
    }

    /// 三种通知的 `threadId` 是 `Option`：有值就归属那个 thread，没有就是
    /// 真正的全局通知——同一个通知类型两边都可能落，靠的是运行时的值而不是
    /// 静态类型。
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

    /// 本客户端还不认识的通知：猜归属时同样要认得 `conversationId` 与嵌套的
    /// `thread.id`，不能只认 `threadId`。
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
