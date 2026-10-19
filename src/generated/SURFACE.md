# 协议表面

由 protocol-codegen 生成，请勿手工修改。**这个文件是用来 diff 的**：
codex 升级后重新生成，`git diff` 它就能看出协议的方法级变化。

- 客户端请求：104 个
- 服务端通知：83 个
- 服务端请求：10 个
- 数据类型：709 个

## 客户端请求

| 方法 | 参数 | 响应 |
| --- | --- | --- |
| `initialize` | `InitializeParams` | `InitializeResponse` |
| `thread/start` | `ThreadStartParams` | `ThreadStartResponse` |
| `thread/resume` | `ThreadResumeParams` | `ThreadResumeResponse` |
| `thread/fork` | `ThreadForkParams` | `ThreadForkResponse` |
| `thread/archive` | `ThreadArchiveParams` | `ThreadArchiveResponse` |
| `thread/delete` | `ThreadDeleteParams` | `ThreadDeleteResponse` |
| `thread/unsubscribe` | `ThreadUnsubscribeParams` | `ThreadUnsubscribeResponse` |
| `thread/name/set` | `ThreadSetNameParams` | `ThreadSetNameResponse` |
| `thread/goal/set` | `ThreadGoalSetParams` | `ThreadGoalSetResponse` |
| `thread/goal/get` | `ThreadGoalGetParams` | `ThreadGoalGetResponse` |
| `thread/goal/clear` | `ThreadGoalClearParams` | `ThreadGoalClearResponse` |
| `thread/metadata/update` | `ThreadMetadataUpdateParams` | `ThreadMetadataUpdateResponse` |
| `thread/attachment/add` | `ThreadAttachmentAddParams` | `ThreadAttachmentAddResponse` |
| `thread/attachment/list` | `ThreadAttachmentListParams` | `ThreadAttachmentListResponse` |
| `thread/attachment/remove` | `ThreadAttachmentRemoveParams` | `ThreadAttachmentRemoveResponse` |
| `thread/section/move` | `ThreadSectionMoveParams` | `ThreadSectionMoveResponse` |
| `thread/unarchive` | `ThreadUnarchiveParams` | `ThreadUnarchiveResponse` |
| `thread/compact/start` | `ThreadCompactStartParams` | `ThreadCompactStartResponse` |
| `thread/shellCommand` | `ThreadShellCommandParams` | `ThreadShellCommandResponse` |
| `thread/approveGuardianDeniedAction` | `ThreadApproveGuardianDeniedActionParams` | `ThreadApproveGuardianDeniedActionResponse` |
| `thread/revert` | `ThreadRevertParams` | `ThreadRevertResponse` |
| `thread/list` | `ThreadListParams` | `ThreadListResponse` |
| `threadSection/list` | `ThreadSectionListParams` | `ThreadSectionListResponse` |
| `threadSection/create` | `ThreadSectionCreateParams` | `ThreadSectionCreateResponse` |
| `threadSection/update` | `ThreadSectionUpdateParams` | `ThreadSectionUpdateResponse` |
| `threadSection/delete` | `ThreadSectionDeleteParams` | `ThreadSectionDeleteResponse` |
| `thread/loaded/list` | `ThreadLoadedListParams` | `ThreadLoadedListResponse` |
| `thread/read` | `ThreadReadParams` | `ThreadReadResponse` |
| `thread/turns/list` | `ThreadTurnsListParams` | `ThreadTurnsListResponse` |
| `thread/items/list` | `ThreadItemsListParams` | `ThreadItemsListResponse` |
| `thread/inject_items` | `ThreadInjectItemsParams` | `ThreadInjectItemsResponse` |
| `skills/list` | `SkillsListParams` | `SkillsListResponse` |
| `skills/extraRoots/set` | `SkillsExtraRootsSetParams` | `SkillsExtraRootsSetResponse` |
| `hooks/list` | `HooksListParams` | `HooksListResponse` |
| `marketplace/add` | `MarketplaceAddParams` | `MarketplaceAddResponse` |
| `marketplace/remove` | `MarketplaceRemoveParams` | `MarketplaceRemoveResponse` |
| `marketplace/upgrade` | `MarketplaceUpgradeParams` | `MarketplaceUpgradeResponse` |
| `plugin/list` | `PluginListParams` | `PluginListResponse` |
| `plugin/installed` | `PluginInstalledParams` | `PluginInstalledResponse` |
| `plugin/reconcile` | `PluginReconcileParams` | `PluginReconcileResponse` |
| `plugin/read` | `PluginReadParams` | `PluginReadResponse` |
| `plugin/skill/read` | `PluginSkillReadParams` | `PluginSkillReadResponse` |
| `plugin/share/save` | `PluginShareSaveParams` | `PluginShareSaveResponse` |
| `plugin/share/updateTargets` | `PluginShareUpdateTargetsParams` | `PluginShareUpdateTargetsResponse` |
| `plugin/share/list` | `PluginShareListParams` | `PluginShareListResponse` |
| `plugin/share/checkout` | `PluginShareCheckoutParams` | `PluginShareCheckoutResponse` |
| `plugin/share/delete` | `PluginShareDeleteParams` | `PluginShareDeleteResponse` |
| `app/read` | `AppsReadParams` | `AppsReadResponse` |
| `app/list` | `AppsListParams` | `AppsListResponse` |
| `app/installed` | `AppsInstalledParams` | `AppsInstalledResponse` |
| `fs/readFile` | `FsReadFileParams` | `FsReadFileResponse` |
| `fs/writeFile` | `FsWriteFileParams` | `FsWriteFileResponse` |
| `fs/createDirectory` | `FsCreateDirectoryParams` | `FsCreateDirectoryResponse` |
| `fs/getMetadata` | `FsGetMetadataParams` | `FsGetMetadataResponse` |
| `fs/readDirectory` | `FsReadDirectoryParams` | `FsReadDirectoryResponse` |
| `fs/remove` | `FsRemoveParams` | `FsRemoveResponse` |
| `fs/copy` | `FsCopyParams` | `FsCopyResponse` |
| `fs/watch` | `FsWatchParams` | `FsWatchResponse` |
| `fs/unwatch` | `FsUnwatchParams` | `FsUnwatchResponse` |
| `skills/config/write` | `SkillsConfigWriteParams` | `SkillsConfigWriteResponse` |
| `plugin/install` | `PluginInstallParams` | `PluginInstallResponse` |
| `plugin/uninstall` | `PluginUninstallParams` | `PluginUninstallResponse` |
| `turn/start` | `TurnStartParams` | `TurnStartResponse` |
| `turn/steer` | `TurnSteerParams` | `TurnSteerResponse` |
| `turn/interrupt` | `TurnInterruptParams` | `TurnInterruptResponse` |
| `review/start` | `ReviewStartParams` | `ReviewStartResponse` |
| `model/list` | `ModelListParams` | `ModelListResponse` |
| `account/gatewayOAuth/read` | `AccountGatewayOAuthReadParams` | `GatewayOAuthReadResponse` |
| `account/gatewayOAuth/login` | `AccountGatewayOAuthLoginParams` | `GatewayOAuthLoginResponse` |
| `account/gatewayOAuth/cancel` | `AccountGatewayOAuthCancelParams` | `GatewayOAuthCancelResponse` |
| `modelProvider/capabilities/read` | `ModelProviderCapabilitiesReadParams` | `ModelProviderCapabilitiesReadResponse` |
| `experimentalFeature/list` | `ExperimentalFeatureListParams` | `ExperimentalFeatureListResponse` |
| `permissionProfile/list` | `PermissionProfileListParams` | `PermissionProfileListResponse` |
| `experimentalFeature/enablement/set` | `ExperimentalFeatureEnablementSetParams` | `ExperimentalFeatureEnablementSetResponse` |
| `mcpServer/oauth/login` | `McpServerOauthLoginParams` | `McpServerOauthLoginResponse` |
| `config/mcpServer/reload` | `ConfigMcpServerReloadParams` | `McpServerRefreshResponse` |
| `mcpServerStatus/list` | `ListMcpServerStatusParams` | `ListMcpServerStatusResponse` |
| `mcpServer/resource/read` | `McpResourceReadParams` | `McpResourceReadResponse` |
| `mcpServer/tool/call` | `McpServerToolCallParams` | `McpServerToolCallResponse` |
| `windowsSandbox/setupStart` | `WindowsSandboxSetupStartParams` | `WindowsSandboxSetupStartResponse` |
| `windowsSandbox/readiness` | `WindowsSandboxReadinessParams` | `WindowsSandboxReadinessResponse` |
| `account/login/start` | `LoginAccountParams` | `LoginAccountResponse` |
| `account/login/cancel` | `CancelLoginAccountParams` | `CancelLoginAccountResponse` |
| `account/logout` | `AccountLogoutParams` | `LogoutAccountResponse` |
| `account/rateLimits/read` | `Option<GetAccountRateLimitsParams>` | `GetAccountRateLimitsResponse` |
| `account/rateLimitResetCredit/consume` | `ConsumeAccountRateLimitResetCreditParams` | `ConsumeAccountRateLimitResetCreditResponse` |
| `account/usage/read` | `Option<GetAccountTokenUsageParams>` | `GetAccountTokenUsageResponse` |
| `account/workspaceMessages/read` | `AccountWorkspaceMessagesReadParams` | `GetWorkspaceMessagesResponse` |
| `account/sendAddCreditsNudgeEmail` | `SendAddCreditsNudgeEmailParams` | `SendAddCreditsNudgeEmailResponse` |
| `feedback/upload` | `FeedbackUploadParams` | `FeedbackUploadResponse` |
| `command/exec` | `CommandExecParams` | `CommandExecResponse` |
| `command/exec/write` | `CommandExecWriteParams` | `CommandExecWriteResponse` |
| `command/exec/terminate` | `CommandExecTerminateParams` | `CommandExecTerminateResponse` |
| `command/exec/resize` | `CommandExecResizeParams` | `CommandExecResizeResponse` |
| `config/read` | `ConfigReadParams` | `ConfigReadResponse` |
| `externalAgentConfig/detect` | `ExternalAgentConfigDetectParams` | `ExternalAgentConfigDetectResponse` |
| `externalAgentConfig/import` | `ExternalAgentConfigImportParams` | `ExternalAgentConfigImportResponse` |
| `externalAgentConfig/import/recordHistory` | `ExternalAgentConfigImportHistoryRecordParams` | `ExternalAgentConfigImportHistoryRecordResponse` |
| `externalAgentConfig/import/readHistories` | `ExternalAgentConfigImportReadHistoriesParams` | `ExternalAgentConfigImportHistoriesReadResponse` |
| `config/value/write` | `ConfigValueWriteParams` | `ConfigWriteResponse` |
| `config/batchWrite` | `ConfigBatchWriteParams` | `ConfigWriteResponse` |
| `configRequirements/read` | `ConfigRequirementsReadParams` | `ConfigRequirementsReadResponse` |
| `account/read` | `GetAccountParams` | `GetAccountResponse` |
| `fuzzyFileSearch` | `FuzzyFileSearchParams` | `FuzzyFileSearchResponse` |

## 服务端通知

| 方法 | payload |
| --- | --- |
| `error` | `ErrorNotification` |
| `thread/started` | `ThreadStartedNotification` |
| `thread/status/changed` | `ThreadStatusChangedNotification` |
| `thread/archived` | `ThreadArchivedNotification` |
| `thread/deleted` | `ThreadDeletedNotification` |
| `thread/unarchived` | `ThreadUnarchivedNotification` |
| `thread/closed` | `ThreadClosedNotification` |
| `thread/reverted` | `ThreadRevertedNotification` |
| `skills/changed` | `SkillsChangedNotification` |
| `thread/name/updated` | `ThreadNameUpdatedNotification` |
| `thread/attachment/updated` | `ThreadAttachmentUpdatedNotification` |
| `thread/goal/updated` | `ThreadGoalUpdatedNotification` |
| `thread/goal/cleared` | `ThreadGoalClearedNotification` |
| `thread/queue/changed` | `ThreadQueueChangedNotification` |
| `project/changed` | `ProjectChangedNotification` |
| `thread/project/updated` | `ThreadProjectUpdatedNotification` |
| `thread/environment/connected` | `EnvironmentConnectionNotification` |
| `thread/environment/disconnected` | `EnvironmentConnectionNotification` |
| `thread/settings/updated` | `ThreadSettingsUpdatedNotification` |
| `thread/tokenUsage/updated` | `ThreadTokenUsageUpdatedNotification` |
| `turn/started` | `TurnStartedNotification` |
| `hook/started` | `HookStartedNotification` |
| `turn/completed` | `TurnCompletedNotification` |
| `hook/completed` | `HookCompletedNotification` |
| `turn/diff/updated` | `TurnDiffUpdatedNotification` |
| `turn/plan/updated` | `TurnPlanUpdatedNotification` |
| `item/started` | `ItemStartedNotification` |
| `item/autoApprovalReview/started` | `ItemGuardianApprovalReviewStartedNotification` |
| `item/autoApprovalReview/completed` | `ItemGuardianApprovalReviewCompletedNotification` |
| `autoApprovalReview/strictReviewRequired` | `StrictReviewRequiredNotification` |
| `item/completed` | `ItemCompletedNotification` |
| `item/agentMessage/delta` | `AgentMessageDeltaNotification` |
| `item/plan/delta` | `PlanDeltaNotification` |
| `command/exec/outputDelta` | `CommandExecOutputDeltaNotification` |
| `process/outputDelta` | `ProcessOutputDeltaNotification` |
| `process/exited` | `ProcessExitedNotification` |
| `item/commandExecution/outputDelta` | `CommandExecutionOutputDeltaNotification` |
| `item/commandExecution/terminalInteraction` | `TerminalInteractionNotification` |
| `item/fileChange/outputDelta` | `FileChangeOutputDeltaNotification` |
| `item/fileChange/patchUpdated` | `FileChangePatchUpdatedNotification` |
| `serverRequest/resolved` | `ServerRequestResolvedNotification` |
| `item/mcpToolCall/progress` | `McpToolCallProgressNotification` |
| `mcpServer/oauthLogin/completed` | `McpServerOauthLoginCompletedNotification` |
| `mcpServer/startupStatus/updated` | `McpServerStatusUpdatedNotification` |
| `mcpServer/event/stream/notification` | `McpServerEventStreamNotification` |
| `account/updated` | `AccountUpdatedNotification` |
| `account/gatewayOAuth/changed` | `GatewayOAuthChangedNotification` |
| `account/rateLimits/updated` | `AccountRateLimitsUpdatedNotification` |
| `app/list/updated` | `AppListUpdatedNotification` |
| `remoteControl/status/changed` | `RemoteControlStatusChangedNotification` |
| `externalAgentConfig/import/progress` | `ExternalAgentConfigImportProgressNotification` |
| `externalAgentConfig/import/completed` | `ExternalAgentConfigImportCompletedNotification` |
| `fs/changed` | `FsChangedNotification` |
| `item/reasoning/summaryTextDelta` | `ReasoningSummaryTextDeltaNotification` |
| `item/reasoning/summaryPartAdded` | `ReasoningSummaryPartAddedNotification` |
| `item/reasoning/textDelta` | `ReasoningTextDeltaNotification` |
| `thread/compacted` | `ContextCompactedNotification` |
| `model/rerouted` | `ModelReroutedNotification` |
| `model/verification` | `ModelVerificationNotification` |
| `modelProvider/authRecoveryStarted` | `AuthRecoveryNotification` |
| `modelProvider/authRecoveryCompleted` | `AuthRecoveryNotification` |
| `turn/moderationMetadata` | `TurnModerationMetadataNotification` |
| `model/safetyBuffering/updated` | `ModelSafetyBufferingUpdatedNotification` |
| `warning` | `WarningNotification` |
| `guardianWarning` | `GuardianWarningNotification` |
| `deprecationNotice` | `DeprecationNoticeNotification` |
| `configWarning` | `ConfigWarningNotification` |
| `fuzzyFileSearch/sessionUpdated` | `FuzzyFileSearchSessionUpdatedNotification` |
| `fuzzyFileSearch/sessionCompleted` | `FuzzyFileSearchSessionCompletedNotification` |
| `thread/realtime/started` | `ThreadRealtimeStartedNotification` |
| `thread/realtime/itemAdded` | `ThreadRealtimeItemAddedNotification` |
| `thread/realtime/item/started` | `ThreadRealtimeItemStartedNotification` |
| `thread/realtime/item/transcript/delta` | `ThreadRealtimeItemTranscriptDeltaNotification` |
| `thread/realtime/item/completed` | `ThreadRealtimeItemCompletedNotification` |
| `thread/realtime/transcript/delta` | `ThreadRealtimeTranscriptDeltaNotification` |
| `thread/realtime/transcript/done` | `ThreadRealtimeTranscriptDoneNotification` |
| `thread/realtime/outputAudio/delta` | `ThreadRealtimeOutputAudioDeltaNotification` |
| `thread/realtime/sdp` | `ThreadRealtimeSdpNotification` |
| `thread/realtime/error` | `ThreadRealtimeErrorNotification` |
| `thread/realtime/closed` | `ThreadRealtimeClosedNotification` |
| `windows/worldWritableWarning` | `WindowsWorldWritableWarningNotification` |
| `windowsSandbox/setupCompleted` | `WindowsSandboxSetupCompletedNotification` |
| `account/login/completed` | `AccountLoginCompletedNotification` |

## 服务端请求

这些**必须回复**，否则服务端一直阻塞。

| 方法 | payload |
| --- | --- |
| `item/commandExecution/requestApproval` | `CommandExecutionRequestApprovalParams` |
| `item/fileChange/requestApproval` | `FileChangeRequestApprovalParams` |
| `item/tool/requestUserInput` | `ToolRequestUserInputParams` |
| `mcpServer/elicitation/request` | `McpServerElicitationRequestParams` |
| `item/permissions/requestApproval` | `PermissionsRequestApprovalParams` |
| `item/tool/call` | `DynamicToolCallParams` |
| `account/chatgptAuthTokens/refresh` | `ChatgptAuthTokensRefreshParams` |
| `attestation/generate` | `AttestationGenerateParams` |
| `applyPatchApproval` | `ApplyPatchApprovalParams` |
| `execCommandApproval` | `ExecCommandApprovalParams` |

## 数据类型

- `AbsolutePathBuf`
- `Account`
- `AccountLoginCompletedNotification`
- `AccountRateLimitsUpdatedNotification`
- `AccountRoutingOverride`
- `AccountTokenUsageDailyBucket`
- `AccountTokenUsageSummary`
- `AccountUpdatedNotification`
- `ActivePermissionProfile`
- `AddCreditsNudgeCreditType`
- `AddCreditsNudgeEmailStatus`
- `AdditionalContextEntry`
- `AdditionalContextKind`
- `AdditionalFileSystemPermissions`
- `AdditionalNetworkPermissions`
- `AdditionalPermissionProfile`
- `AgentMessageDelivery`
- `AgentMessageDeltaNotification`
- `AgentMessageInputContent`
- `AgentPath`
- `AllowDenyRequirement`
- `AnalyticsConfig`
- `AppBranding`
- `AppConfig`
- `AppInfo`
- `AppLinkConfig`
- `AppLinksConfig`
- `AppListUpdatedNotification`
- `AppMetadata`
- `AppReview`
- `AppScreenshot`
- `AppSummary`
- `AppTemplateSummary`
- `AppTemplateUnavailableReason`
- `AppToolApproval`
- `AppToolConfig`
- `AppToolSummary`
- `AppToolsConfig`
- `ApplicationNetworkRequirements`
- `ApplicationRequirements`
- `ApplyPatchApprovalParams`
- `ApplyPatchApprovalResponse`
- `ApprovalsReviewer`
- `AppsConfig`
- `AppsDefaultConfig`
- `AppsInstalledParams`
- `AppsInstalledResponse`
- `AppsListParams`
- `AppsListResponse`
- `AppsReadParams`
- `AppsReadResponse`
- `AskForApproval`
- `AsyncUserInputQuestion`
- `AttestationGenerateParams`
- `AttestationGenerateResponse`
- `AuthMode`
- `AuthRecoveryNotification`
- `AutoCompactTokenLimitScope`
- `AutoReviewDecisionSource`
- `AutoReviewRequirements`
- `BrowserUseAccessApprovalLifetime`
- `BrowserUseConfig`
- `BrowserUseOriginPolicy`
- `BrowserUseOriginPolicyConfig`
- `BrowserUseRequirements`
- `ByteRange`
- `CancelLoginAccountParams`
- `CancelLoginAccountResponse`
- `CancelLoginAccountStatus`
- `CapabilityRootLocation`
- `ChatgptAuthTokensRefreshParams`
- `ChatgptAuthTokensRefreshReason`
- `ChatgptAuthTokensRefreshResponse`
- `CliAuthCredentialsStoreMode`
- `ClientInfo`
- `ClientNotification`
- `ClientRequest`
- `CodexErrorInfo`
- `CodexResponseHandoffMode`
- `CollabAgentState`
- `CollabAgentStatus`
- `CollabAgentTool`
- `CollabAgentToolCallStatus`
- `CollaborationMode`
- `CommandAction`
- `CommandExecOutputDeltaNotification`
- `CommandExecOutputStream`
- `CommandExecParams`
- `CommandExecResizeParams`
- `CommandExecResizeResponse`
- `CommandExecResponse`
- `CommandExecTerminalSize`
- `CommandExecTerminateParams`
- `CommandExecTerminateResponse`
- `CommandExecWriteParams`
- `CommandExecWriteResponse`
- `CommandExecutionApprovalDecision`
- `CommandExecutionApprovalKind`
- `CommandExecutionOutputDeltaNotification`
- `CommandExecutionRequestApprovalParams`
- `CommandExecutionRequestApprovalResponse`
- `CommandExecutionSource`
- `CommandExecutionStatus`
- `CommandMigration`
- `ComputerUseConfig`
- `ComputerUseMacosConfig`
- `ComputerUseMacosRequirements`
- `ComputerUseRequirements`
- `ComputerUseWindowsConfig`
- `ComputerUseWindowsExeConfig`
- `ComputerUseWindowsExeRequirement`
- `ComputerUseWindowsRequirements`
- `Config`
- `ConfigBatchWriteParams`
- `ConfigEdit`
- `ConfigLayer`
- `ConfigLayerMetadata`
- `ConfigLayerSource`
- `ConfigReadParams`
- `ConfigReadResponse`
- `ConfigRequirements`
- `ConfigRequirementsReadResponse`
- `ConfigValueWriteParams`
- `ConfigWarningNotification`
- `ConfigWriteResponse`
- `ConfigurationReasoning`
- `ConfiguredHookHandler`
- `ConfiguredHookMatcherGroup`
- `ConnectorMetadata`
- `ConsumeAccountRateLimitResetCreditOutcome`
- `ConsumeAccountRateLimitResetCreditParams`
- `ConsumeAccountRateLimitResetCreditResponse`
- `ContentItem`
- `ContextCompactedNotification`
- `ConversationTextRole`
- `CreditsSnapshot`
- `CyberAccessProgram`
- `DeprecationNoticeNotification`
- `DesktopOnboardingEntrypoint`
- `DynamicToolCallOutputContentItem`
- `DynamicToolCallParams`
- `DynamicToolCallResponse`
- `DynamicToolCallStatus`
- `DynamicToolNamespaceTool`
- `DynamicToolSpec`
- `EnvironmentConnectionNotification`
- `ErrorNotification`
- `ExecCommandApprovalParams`
- `ExecCommandApprovalResponse`
- `ExperimentalFeature`
- `ExperimentalFeatureEnablementSetParams`
- `ExperimentalFeatureEnablementSetResponse`
- `ExperimentalFeatureListParams`
- `ExperimentalFeatureListResponse`
- `ExperimentalFeatureStage`
- `ExternalAgentConfigDetectParams`
- `ExternalAgentConfigDetectResponse`
- `ExternalAgentConfigImportCompletedNotification`
- `ExternalAgentConfigImportHistoriesReadResponse`
- `ExternalAgentConfigImportHistory`
- `ExternalAgentConfigImportHistoryRecordParams`
- `ExternalAgentConfigImportHistoryRecordResponse`
- `ExternalAgentConfigImportHistoryRecordSuccessParams`
- `ExternalAgentConfigImportHistoryRecordTypeResultParams`
- `ExternalAgentConfigImportItemTypeFailure`
- `ExternalAgentConfigImportItemTypeSuccess`
- `ExternalAgentConfigImportParams`
- `ExternalAgentConfigImportProgressNotification`
- `ExternalAgentConfigImportResponse`
- `ExternalAgentConfigImportTypeResult`
- `ExternalAgentConfigMigrationItem`
- `ExternalAgentConfigMigrationItemType`
- `ExternalAgentDetectedConnectorCandidate`
- `ExternalAgentDetectedConnectorSource`
- `ExternalAgentImportedConnectorCandidate`
- `ExternalAgentImportedConnectorSource`
- `FeedbackRequirements`
- `FeedbackUploadParams`
- `FeedbackUploadResponse`
- `FileChange`
- `FileChangeApprovalDecision`
- `FileChangeOutputDeltaNotification`
- `FileChangePatchUpdatedNotification`
- `FileChangeRequestApprovalParams`
- `FileChangeRequestApprovalResponse`
- `FileSystemAccessMode`
- `FileSystemPath`
- `FileSystemSandboxEntry`
- `FileSystemSpecialPath`
- `FileUpdateChange`
- `ForcedChatgptWorkspaceIds`
- `ForcedLoginMethod`
- `FsChangedNotification`
- `FsCopyParams`
- `FsCopyResponse`
- `FsCreateDirectoryParams`
- `FsCreateDirectoryResponse`
- `FsGetMetadataParams`
- `FsGetMetadataResponse`
- `FsReadDirectoryEntry`
- `FsReadDirectoryParams`
- `FsReadDirectoryResponse`
- `FsReadFileParams`
- `FsReadFileResponse`
- `FsRemoveParams`
- `FsRemoveResponse`
- `FsUnwatchParams`
- `FsUnwatchResponse`
- `FsWatchParams`
- `FsWatchResponse`
- `FsWriteFileParams`
- `FsWriteFileResponse`
- `FunctionCallOutputBody`
- `FunctionCallOutputContentItem`
- `FuzzyFileSearchMatchType`
- `FuzzyFileSearchParams`
- `FuzzyFileSearchResponse`
- `FuzzyFileSearchResult`
- `FuzzyFileSearchSessionCompletedNotification`
- `FuzzyFileSearchSessionUpdatedNotification`
- `GatewayOAuthCancelResponse`
- `GatewayOAuthChangedNotification`
- `GatewayOAuthLoginResponse`
- `GatewayOAuthReadResponse`
- `GatewayOAuthStatus`
- `GetAccountParams`
- `GetAccountRateLimitsParams`
- `GetAccountRateLimitsResponse`
- `GetAccountResponse`
- `GetAccountTokenUsageParams`
- `GetAccountTokenUsageResponse`
- `GetWorkspaceMessagesResponse`
- `GitInfo`
- `GrantedPermissionProfile`
- `GuardianApprovalReview`
- `GuardianApprovalReviewAction`
- `GuardianApprovalReviewStatus`
- `GuardianCommandSource`
- `GuardianRiskLevel`
- `GuardianUserAuthorization`
- `GuardianWarningNotification`
- `HookCompletedNotification`
- `HookErrorInfo`
- `HookEventName`
- `HookExecutionMode`
- `HookHandlerType`
- `HookMetadata`
- `HookMigration`
- `HookOutputEntry`
- `HookOutputEntryKind`
- `HookPromptFragment`
- `HookRunStatus`
- `HookRunSummary`
- `HookScope`
- `HookSource`
- `HookStartedNotification`
- `HookTrustStatus`
- `HooksListEntry`
- `HooksListParams`
- `HooksListResponse`
- `ImageDetail`
- `ImageGenerationFailure`
- `InAppBrowserRequirements`
- `InitializeCapabilities`
- `InitializeParams`
- `InitializeResponse`
- `InputModality`
- `InstalledApp`
- `InternalChatMessageMetadataPassthrough`
- `ItemCompletedNotification`
- `ItemGuardianApprovalReviewCompletedNotification`
- `ItemGuardianApprovalReviewStartedNotification`
- `ItemStartedNotification`
- `JSONRPCError`
- `JSONRPCErrorError`
- `JSONRPCMessage`
- `JSONRPCNotification`
- `JSONRPCRequest`
- `JSONRPCResponse`
- `LegacyAppPathString`
- `ListMcpServerStatusParams`
- `ListMcpServerStatusResponse`
- `LocalShellAction`
- `LocalShellStatus`
- `LoginAccountParams`
- `LoginAccountResponse`
- `LoginAppBrand`
- `LogoutAccountResponse`
- `ManagedHooksRequirements`
- `MarketplaceAddParams`
- `MarketplaceAddResponse`
- `MarketplaceInterface`
- `MarketplaceLoadErrorInfo`
- `MarketplaceRemoveParams`
- `MarketplaceRemoveResponse`
- `MarketplaceUpgradeErrorInfo`
- `MarketplaceUpgradeParams`
- `MarketplaceUpgradeResponse`
- `McpAppDisplayMode`
- `McpAppUi`
- `McpAuthStatus`
- `McpElicitationArrayType`
- `McpElicitationBooleanSchema`
- `McpElicitationBooleanType`
- `McpElicitationConstOption`
- `McpElicitationEnumSchema`
- `McpElicitationLegacyTitledEnumSchema`
- `McpElicitationMultiSelectEnumSchema`
- `McpElicitationNumberSchema`
- `McpElicitationNumberType`
- `McpElicitationObjectType`
- `McpElicitationPrimitiveSchema`
- `McpElicitationSchema`
- `McpElicitationSingleSelectEnumSchema`
- `McpElicitationStringFormat`
- `McpElicitationStringSchema`
- `McpElicitationStringType`
- `McpElicitationTitledEnumItems`
- `McpElicitationTitledMultiSelectEnumSchema`
- `McpElicitationTitledSingleSelectEnumSchema`
- `McpElicitationUntitledEnumItems`
- `McpElicitationUntitledMultiSelectEnumSchema`
- `McpElicitationUntitledSingleSelectEnumSchema`
- `McpResourceReadParams`
- `McpResourceReadResponse`
- `McpResourceReadTarget`
- `McpServerConnectionStatus`
- `McpServerElicitationAction`
- `McpServerElicitationRequestParams`
- `McpServerElicitationRequestResponse`
- `McpServerEventNotification`
- `McpServerEventStreamNotification`
- `McpServerInfo`
- `McpServerMigration`
- `McpServerOauthClientRegistration`
- `McpServerOauthLoginCompletedNotification`
- `McpServerOauthLoginParams`
- `McpServerOauthLoginResponse`
- `McpServerRefreshResponse`
- `McpServerStartupFailureReason`
- `McpServerStartupState`
- `McpServerStatus`
- `McpServerStatusDetail`
- `McpServerStatusUpdatedNotification`
- `McpServerToolCallParams`
- `McpServerToolCallResponse`
- `McpToolCallAppContext`
- `McpToolCallError`
- `McpToolCallProgressNotification`
- `McpToolCallResult`
- `McpToolCallStatus`
- `MemoryCitation`
- `MemoryCitationEntry`
- `MergeStrategy`
- `MessagePhase`
- `MigrationDetails`
- `MisalignmentErrorDetails`
- `MisalignmentSteer`
- `ModeKind`
- `Model`
- `ModelAccessPrograms`
- `ModelAvailabilityNux`
- `ModelListParams`
- `ModelListResponse`
- `ModelProviderCapabilitiesReadParams`
- `ModelProviderCapabilitiesReadResponse`
- `ModelRerouteReason`
- `ModelReroutedNotification`
- `ModelSafetyBufferingUpdatedNotification`
- `ModelServiceTier`
- `ModelUpgradeInfo`
- `ModelVerification`
- `ModelVerificationNotification`
- `ModelsRequirements`
- `MultiAgentMode`
- `MultiAgentVersion`
- `NetworkAccess`
- `NetworkApprovalContext`
- `NetworkApprovalProtocol`
- `NetworkDomainPermission`
- `NetworkPolicyAmendment`
- `NetworkPolicyRuleAction`
- `NetworkRequirements`
- `NetworkUnixSocketPermission`
- `NewThreadModelDefaults`
- `NonSteerableTurnKind`
- `NullableGetAccountRateLimitsParams`
- `NullableGetAccountTokenUsageParams`
- `OverriddenMetadata`
- `ParsedCommand`
- `PatchApplyStatus`
- `PatchChangeKind`
- `PermissionGrantScope`
- `PermissionProfileListParams`
- `PermissionProfileListResponse`
- `PermissionProfileSummary`
- `PermissionsRequestApprovalParams`
- `PermissionsRequestApprovalResponse`
- `Personality`
- `PlanDeltaNotification`
- `PlanType`
- `PluginAuthPolicy`
- `PluginAvailability`
- `PluginDetail`
- `PluginDisabledReason`
- `PluginHookSummary`
- `PluginInstallParams`
- `PluginInstallPolicy`
- `PluginInstallPolicySource`
- `PluginInstallResponse`
- `PluginInstalledParams`
- `PluginInstalledResponse`
- `PluginInterface`
- `PluginListMarketplaceKind`
- `PluginListParams`
- `PluginListResponse`
- `PluginMarketplaceEntry`
- `PluginReadParams`
- `PluginReadResponse`
- `PluginReconcileChangedPlugin`
- `PluginReconcileParams`
- `PluginReconcileResponse`
- `PluginSearchScope`
- `PluginShareCheckoutParams`
- `PluginShareCheckoutResponse`
- `PluginShareContext`
- `PluginShareDeleteParams`
- `PluginShareDeleteResponse`
- `PluginShareDiscoverability`
- `PluginShareListItem`
- `PluginShareListParams`
- `PluginShareListResponse`
- `PluginSharePrincipal`
- `PluginSharePrincipalRole`
- `PluginSharePrincipalType`
- `PluginShareSaveParams`
- `PluginShareSaveResponse`
- `PluginShareTarget`
- `PluginShareTargetRole`
- `PluginShareUpdateDiscoverability`
- `PluginShareUpdateTargetsParams`
- `PluginShareUpdateTargetsResponse`
- `PluginSkillReadParams`
- `PluginSkillReadResponse`
- `PluginSource`
- `PluginSummary`
- `PluginUninstallParams`
- `PluginUninstallResponse`
- `PluginsMigration`
- `ProcessExitedNotification`
- `ProcessOutputDeltaNotification`
- `ProcessOutputStream`
- `ProcessTerminalSize`
- `ProjectChangeType`
- `ProjectChangedNotification`
- `ProjectRoot`
- `ProjectSortKey`
- `RateLimitReachedType`
- `RateLimitResetCredit`
- `RateLimitResetCreditStatus`
- `RateLimitResetCreditsSummary`
- `RateLimitResetType`
- `RateLimitSnapshot`
- `RateLimitWindow`
- `RawResponseCompletedNotification`
- `RawResponseItemCompletedNotification`
- `RealtimeConversationVersion`
- `RealtimeOutputModality`
- `RealtimeVoice`
- `ReasoningEffort`
- `ReasoningEffortOption`
- `ReasoningItemContent`
- `ReasoningItemReasoningSummary`
- `ReasoningSummary`
- `ReasoningSummaryPartAddedNotification`
- `ReasoningSummaryTextDeltaNotification`
- `ReasoningTextDeltaNotification`
- `RemoteControlConnectionStatus`
- `RemoteControlDisableParams`
- `RemoteControlEnableParams`
- `RemoteControlStatusChangedNotification`
- `RequestId`
- `RequestPermissionProfile`
- `ResidencyRequirement`
- `Resource`
- `ResourceContent`
- `ResourceTemplate`
- `ResponseItem`
- `ResponseUsageMetadata`
- `ResponsesApiWebSearchAction`
- `ReviewDecision`
- `ReviewDelivery`
- `ReviewStartParams`
- `ReviewStartResponse`
- `ReviewTarget`
- `SandboxMode`
- `SandboxPolicy`
- `SandboxWorkspaceWrite`
- `ScheduledTaskSchedule`
- `ScheduledTaskSummary`
- `ScheduledTaskWeekday`
- `SelectedCapabilityRoot`
- `SendAddCreditsNudgeEmailParams`
- `SendAddCreditsNudgeEmailResponse`
- `ServerNotification`
- `ServerRequest`
- `ServerRequestResolvedNotification`
- `SessionMigration`
- `SessionSource`
- `Settings`
- `SkillDependencies`
- `SkillErrorInfo`
- `SkillInterface`
- `SkillMetadata`
- `SkillMigration`
- `SkillScope`
- `SkillSummary`
- `SkillToolDependency`
- `SkillsChangedNotification`
- `SkillsConfigWriteParams`
- `SkillsConfigWriteResponse`
- `SkillsExtraRootsSetParams`
- `SkillsExtraRootsSetResponse`
- `SkillsListEntry`
- `SkillsListParams`
- `SkillsListResponse`
- `SortDirection`
- `SpendControlLimitSnapshot`
- `StrictReviewRequiredNotification`
- `SubAgentActivityKind`
- `SubAgentSource`
- `SubagentMigration`
- `TerminalInteractionNotification`
- `TextElement`
- `TextPosition`
- `TextRange`
- `Thread`
- `ThreadActiveFlag`
- `ThreadApproveGuardianDeniedActionParams`
- `ThreadApproveGuardianDeniedActionResponse`
- `ThreadArchiveParams`
- `ThreadArchiveResponse`
- `ThreadArchivedNotification`
- `ThreadAttachment`
- `ThreadAttachmentAddOutcome`
- `ThreadAttachmentAddParams`
- `ThreadAttachmentAddResponse`
- `ThreadAttachmentListParams`
- `ThreadAttachmentListResponse`
- `ThreadAttachmentOperation`
- `ThreadAttachmentRemoveParams`
- `ThreadAttachmentRemoveResponse`
- `ThreadAttachmentUpdatedNotification`
- `ThreadClosedNotification`
- `ThreadCompactStartParams`
- `ThreadCompactStartResponse`
- `ThreadDeleteParams`
- `ThreadDeleteResponse`
- `ThreadDeletedNotification`
- `ThreadEnvironment`
- `ThreadExtra`
- `ThreadForkParams`
- `ThreadForkResponse`
- `ThreadGoal`
- `ThreadGoalClearParams`
- `ThreadGoalClearResponse`
- `ThreadGoalClearedNotification`
- `ThreadGoalGetParams`
- `ThreadGoalGetResponse`
- `ThreadGoalSetParams`
- `ThreadGoalSetResponse`
- `ThreadGoalStatus`
- `ThreadGoalUpdatedNotification`
- `ThreadHistoryMode`
- `ThreadId`
- `ThreadInjectItemsParams`
- `ThreadInjectItemsResponse`
- `ThreadItem`
- `ThreadItemEntry`
- `ThreadItemsListParams`
- `ThreadItemsListResponse`
- `ThreadListCwdFilter`
- `ThreadListParams`
- `ThreadListResponse`
- `ThreadLoadedListParams`
- `ThreadLoadedListResponse`
- `ThreadMemoryMode`
- `ThreadMetadataGitInfoUpdateParams`
- `ThreadMetadataUpdateParams`
- `ThreadMetadataUpdateResponse`
- `ThreadNameUpdatedNotification`
- `ThreadProjectUpdatedNotification`
- `ThreadQueueChangedNotification`
- `ThreadReadParams`
- `ThreadReadResponse`
- `ThreadRealtimeAudioChunk`
- `ThreadRealtimeBemItemPresentation`
- `ThreadRealtimeClosedNotification`
- `ThreadRealtimeErrorNotification`
- `ThreadRealtimeInitialItem`
- `ThreadRealtimeItem`
- `ThreadRealtimeItemAddedNotification`
- `ThreadRealtimeItemCompletedNotification`
- `ThreadRealtimeItemStartedNotification`
- `ThreadRealtimeItemTranscriptDeltaNotification`
- `ThreadRealtimeOutputAudioDeltaNotification`
- `ThreadRealtimeSdpNotification`
- `ThreadRealtimeSessionOutcome`
- `ThreadRealtimeStartTransport`
- `ThreadRealtimeStartedNotification`
- `ThreadRealtimeTranscriptDeltaNotification`
- `ThreadRealtimeTranscriptDoneNotification`
- `ThreadRealtimeTranscriptRole`
- `ThreadResumeInitialTurnsPageParams`
- `ThreadResumeParams`
- `ThreadResumeResponse`
- `ThreadRevertParams`
- `ThreadRevertResponse`
- `ThreadRevertedNotification`
- `ThreadSearchSortKey`
- `ThreadSection`
- `ThreadSectionAppearance`
- `ThreadSectionCreateParams`
- `ThreadSectionCreateResponse`
- `ThreadSectionDeleteParams`
- `ThreadSectionDeleteResponse`
- `ThreadSectionListParams`
- `ThreadSectionListResponse`
- `ThreadSectionMoveParams`
- `ThreadSectionMoveResponse`
- `ThreadSectionUpdateParams`
- `ThreadSectionUpdateResponse`
- `ThreadSetNameParams`
- `ThreadSetNameResponse`
- `ThreadSettings`
- `ThreadSettingsUpdatedNotification`
- `ThreadShellCommandParams`
- `ThreadShellCommandResponse`
- `ThreadSortKey`
- `ThreadSource`
- `ThreadSourceKind`
- `ThreadStartParams`
- `ThreadStartResponse`
- `ThreadStartSource`
- `ThreadStartedNotification`
- `ThreadStatus`
- `ThreadStatusChangedNotification`
- `ThreadTokenUsage`
- `ThreadTokenUsageUpdatedNotification`
- `ThreadTurnsListParams`
- `ThreadTurnsListResponse`
- `ThreadUnarchiveParams`
- `ThreadUnarchiveResponse`
- `ThreadUnarchivedNotification`
- `ThreadUnsubscribeParams`
- `ThreadUnsubscribeResponse`
- `ThreadUnsubscribeStatus`
- `ThreadUsage`
- `ThreadUsageBreakdownGroup`
- `TokenUsageBreakdown`
- `Tool`
- `ToolExposureSurface`
- `ToolRequestUserInputAnswer`
- `ToolRequestUserInputOption`
- `ToolRequestUserInputParams`
- `ToolRequestUserInputQuestion`
- `ToolRequestUserInputResponse`
- `ToolsV2`
- `Turn`
- `TurnCompletedNotification`
- `TurnDiffUpdatedNotification`
- `TurnEnvironmentParams`
- `TurnError`
- `TurnInterruptParams`
- `TurnInterruptResponse`
- `TurnItemsView`
- `TurnModerationMetadataNotification`
- `TurnPlanStep`
- `TurnPlanStepStatus`
- `TurnPlanUpdatedNotification`
- `TurnStartParams`
- `TurnStartResponse`
- `TurnStartedNotification`
- `TurnStatus`
- `TurnSteerParams`
- `TurnSteerResponse`
- `TurnToolOutput`
- `TurnsPage`
- `UserInput`
- `Verbosity`
- `W3cTraceContext`
- `WarningNotification`
- `WebSearchAction`
- `WebSearchContextSize`
- `WebSearchLocation`
- `WebSearchMode`
- `WebSearchToolConfig`
- `WindowsSandboxImplementation`
- `WindowsSandboxReadiness`
- `WindowsSandboxReadinessResponse`
- `WindowsSandboxSetupCompletedNotification`
- `WindowsSandboxSetupMode`
- `WindowsSandboxSetupStartParams`
- `WindowsSandboxSetupStartResponse`
- `WindowsWorldWritableWarningNotification`
- `WorkspaceMessage`
- `WorkspaceMessageType`
- `WorkspaceRouting`
- `WriteStatus`
