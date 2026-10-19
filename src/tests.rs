//! 纯协议序列化与真实 daemon 集成测试。
//!
//! 真实 daemon 测试标记为 `#[ignore]`，要求管理员预先运行 daemon；测试自身绝不启动或停止它。
//! 运行时设置 `CODEX_APP_SERVER_ENDPOINT=unix:///…/app-server-control.sock`，
//! 可选 `CODEX_APP_SERVER_TEST_CWD` 指定工作目录。

use crate::client::AppServerClient;
use crate::client::inbound::{GlobalMessage, ThreadMessage};
use crate::generated::methods::{self as requests, Request};
use crate::generated::types::{
    ApprovalsReviewer, AskForApproval, GetAccountRateLimitsParams, ModelListParams, SandboxPolicy,
    ThreadResumeParams, ThreadStartParams, ThreadTokenUsage, TurnInterruptParams, TurnStartParams,
    TurnStatus, UserInput,
};
use crate::protocol::ThreadNotification;
use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

fn text_input(text: &str) -> UserInput {
    serde_json::from_value(serde_json::json!({"type": "text", "text": text})).expect("文本输入")
}

fn start_params(cwd: &str) -> ThreadStartParams {
    ThreadStartParams {
        cwd: Some(cwd.to_owned()),
        ..Default::default()
    }
}

fn turn_params(
    thread_id: &str,
    text: &str,
    cwd: &str,
    reviewer: ApprovalsReviewer,
) -> TurnStartParams {
    let approval_policy = match reviewer {
        ApprovalsReviewer::User => AskForApproval::Untrusted,
        _ => AskForApproval::OnRequest,
    };
    TurnStartParams {
        thread_id: thread_id.to_owned(),
        input: vec![text_input(text)],
        cwd: Some(cwd.to_owned()),
        sandbox_policy: Some(SandboxPolicy::WorkspaceWrite {
            writable_roots: Some(vec![cwd.to_owned()]),
            network_access: Some(false),
            exclude_slash_tmp: None,
            exclude_tmpdir_env_var: None,
        }),
        approval_policy: Some(approval_policy),
        approvals_reviewer: Some(reviewer),
        ..Default::default()
    }
}

fn completed_status(message: &ThreadMessage) -> Option<TurnStatus> {
    match message {
        ThreadMessage::Notification(notification) => match &**notification {
            ThreadNotification::TurnCompleted(payload) => Some(payload.turn.status.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn usage_of(message: &ThreadMessage) -> Option<&ThreadTokenUsage> {
    match message {
        ThreadMessage::Notification(notification) => match &**notification {
            ThreadNotification::ThreadTokenUsageUpdated(payload) => Some(&payload.token_usage),
            _ => None,
        },
        _ => None,
    }
}

async fn wait_for_real_turn(
    mut handle: tokio::sync::mpsc::UnboundedReceiver<ThreadMessage>,
) -> Result<(TurnStatus, Option<ThreadTokenUsage>)> {
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let mut last_usage = None;
        while let Some(message) = handle.recv().await {
            if let Some(usage) = usage_of(&message) {
                last_usage = Some(usage.clone());
            }
            match message {
                ThreadMessage::Request { reply, .. } => drop(reply),
                other => {
                    if let Some(status) = completed_status(&other) {
                        return Ok((status, last_usage));
                    }
                }
            }
        }
        anyhow::bail!("event stream closed before turn completion")
    })
    .await
    .context("real turn test timed out")?
}

fn daemon_endpoint() -> Option<String> {
    std::env::var("CODEX_APP_SERVER_ENDPOINT").ok()
}

fn daemon_test_cwd() -> String {
    std::env::var("CODEX_APP_SERVER_TEST_CWD").unwrap_or_else(|_| {
        std::env::current_dir()
            .expect("current working directory")
            .to_string_lossy()
            .to_string()
    })
}

/// 无参方法的参数类型是我们凭空造的 unit struct；可空 params 仍必须能序列化成 `null`。
///
/// 它们此前是 `type Params = ()`；换成 unit struct 是为了让 `Request` 能实现在
/// 参数类型上（7 个方法没法共用 `()`），线上字节不该跟着变。
#[test]
fn nullary_params_serialize_to_null() {
    assert_eq!(
        serde_json::to_value(requests::AccountLogoutParams).unwrap(),
        Value::Null
    );
    assert_eq!(
        serde_json::to_value(requests::ConfigRequirementsReadParams).unwrap(),
        serde_json::to_value(()).unwrap()
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

/// 可空 params 在真实 daemon 上仍然可以用 `null` 调用。
///
/// 单测只能证明 unit struct 序列化成 `null`，证明不了服务端吃这一口。挑
/// `account/rateLimits/read` 是因为它只读、无副作用、不消耗额度。
#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon"]
async fn real_daemon_accepts_nullary_params() {
    let Some(endpoint) = daemon_endpoint() else {
        return;
    };
    let client = AppServerClient::builder(endpoint).build();
    client
        .request(None::<GetAccountRateLimitsParams>)
        .await
        .expect("daemon account/rateLimits/read");
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon"]
async fn real_daemon_emits_initialize_response_as_connected() {
    let Some(endpoint) = daemon_endpoint() else {
        return;
    };
    let client = AppServerClient::builder(endpoint).build();
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), client.next_event())
        .await
        .expect("daemon initialize timeout")
        .expect("global event stream closed");
    assert!(matches!(event, GlobalMessage::Connected(_)));
    let models = client
        .request(ModelListParams::default())
        .await
        .expect("daemon model/list");
    assert!(
        !models.data.is_empty(),
        "daemon should publish at least one model"
    );
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon and uses a no-tool test turn"]
async fn real_daemon_turn_start_completes() -> Result<()> {
    let Some(endpoint) = daemon_endpoint() else {
        return Ok(());
    };
    let cwd = daemon_test_cwd();
    let client = AppServerClient::builder(endpoint.clone()).build();
    let (thread, handle) = client.start_thread(start_params(&cwd)).await?;
    client
        .start_turn(turn_params(
            &thread.thread.id,
            "Reply with exactly OK. Do not call tools or make any file changes.",
            &cwd,
            ApprovalsReviewer::User,
        ))
        .await?;
    let (status, usage) = wait_for_real_turn(handle).await?;
    assert_eq!(status, TurnStatus::Completed);
    // 真实 daemon 一定会推送用量；拿不到说明解析又回到了错误的字段上。
    let usage = usage.context("真实 turn 必须报告用量")?;
    assert!(usage.total.total_tokens > 0);
    assert!(
        usage.model_context_window.is_some(),
        "真实 turn 必须报告 modelContextWindow"
    );

    // 新连接必须先 resume（订阅）已持久化的 thread，再启动下一次 turn，
    // 才能确保收到该 thread 的通知。
    let resumed = AppServerClient::builder(endpoint).build();
    let (_, resumed_handle) = resumed
        .resume_thread(ThreadResumeParams {
            thread_id: thread.thread.id.clone(),
            approvals_reviewer: Some(ApprovalsReviewer::User),
            approval_policy: Some(AskForApproval::Untrusted),
            ..Default::default()
        })
        .await?;
    resumed
        .start_turn(turn_params(
            &thread.thread.id,
            "Reply with exactly AGAIN. Do not call tools or make any file changes.",
            &cwd,
            ApprovalsReviewer::User,
        ))
        .await?;
    assert_eq!(
        wait_for_real_turn(resumed_handle).await?.0,
        TurnStatus::Completed
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon and uses a declined approval probe"]
async fn real_daemon_approval_is_pushed_to_turn_connection() -> Result<()> {
    let Some(endpoint) = daemon_endpoint() else {
        return Ok(());
    };
    let cwd = daemon_test_cwd();
    let probe = std::path::Path::new(&cwd)
        .parent()
        .context("approval probe cwd must have a parent")?
        .join(format!(
            "codex-client-cw-approval-probe-{}",
            Uuid::new_v4().simple()
        ));
    let client = AppServerClient::builder(endpoint).build();
    let (thread, mut handle) = client.start_thread(start_params(&cwd)).await?;
    let prompt = format!(
        "Use the shell once to run `touch {}`. Do not use another tool. If permission is denied, reply DENIED.",
        probe.display()
    );
    client
        .start_turn(turn_params(
            &thread.thread.id,
            &prompt,
            &cwd,
            ApprovalsReviewer::User,
        ))
        .await?;
    let (status, saw_approval) = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let mut saw_approval = false;
        while let Some(message) = handle.recv().await {
            if let Some(status) = completed_status(&message) {
                return Ok((status, saw_approval));
            }
            if let ThreadMessage::Request { reply, .. } = message {
                saw_approval = true;
                drop(reply);
            }
        }
        // actor 会跨临时断线保留 route；提前关闭说明无法恢复。
        anyhow::bail!("message stream closed before approval probe completed")
    })
    .await
    .context("approval push test timed out")??;
    assert!(
        saw_approval,
        "turn completed without pushing an approval request"
    );
    assert_eq!(status, TurnStatus::Completed);
    assert!(
        !probe.exists(),
        "declined approval probe unexpectedly created {}",
        probe.display()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon and starts then interrupts a sleep turn"]
async fn real_daemon_turn_can_be_interrupted() -> Result<()> {
    let Some(endpoint) = daemon_endpoint() else {
        return Ok(());
    };
    let cwd = daemon_test_cwd();
    let stream_client = AppServerClient::builder(endpoint.clone()).build();
    let (thread, _handle) = stream_client.start_thread(start_params(&cwd)).await?;
    let turn = stream_client
        .start_turn(turn_params(
            &thread.thread.id,
            "Run `sleep 30`, then reply DONE.",
            &cwd,
            ApprovalsReviewer::User,
        ))
        .await?;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // 另一条连接、手上没有句柄：先加入这条 thread，再发中断。
    let control = AppServerClient::builder(endpoint.clone()).build();
    control
        .subscribe(ThreadResumeParams {
            thread_id: thread.thread.id.clone(),
            ..Default::default()
        })
        .await?;
    control
        .request(TurnInterruptParams {
            thread_id: thread.thread.id.clone(),
            turn_id: turn.turn.id.clone(),
        })
        .await?;

    let status = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let poll = AppServerClient::builder(endpoint.clone()).build();
            if let Some(polled) = poll.poll_turn(&thread.thread.id, &turn.turn.id).await?
                && polled.status != TurnStatus::InProgress
            {
                return Ok::<_, anyhow::Error>(polled.status);
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    })
    .await
    .context("interrupted turn did not reach a terminal state")??;
    assert_eq!(status, TurnStatus::Interrupted);
    Ok(())
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon and probes auto-review on a resumed thread"]
async fn real_daemon_resumed_thread_switches_to_auto_review() -> Result<()> {
    let Some(endpoint) = daemon_endpoint() else {
        return Ok(());
    };
    let cwd = daemon_test_cwd();
    let client = AppServerClient::builder(endpoint.clone()).build();
    let (thread, first) = client.start_thread(start_params(&cwd)).await?;
    client
        .start_turn(turn_params(
            &thread.thread.id,
            "Reply with exactly ASK. Do not call tools or make file changes.",
            &cwd,
            ApprovalsReviewer::User,
        ))
        .await?;
    assert_eq!(wait_for_real_turn(first).await?.0, TurnStatus::Completed);

    let probe = std::path::Path::new(&cwd)
        .parent()
        .context("auto-review probe cwd must have a parent")?
        .join(format!(
            "codex-client-cw-auto-review-probe-{}",
            Uuid::new_v4().simple()
        ));
    let prompt = format!(
        "Use the shell once to run `touch {}`. Do not use another tool. If permission is denied, reply DENIED.",
        probe.display()
    );
    let resumed = AppServerClient::builder(endpoint).build();
    // 新连接、新 reviewer：resume 必须带上 auto_review 与它的 approvalPolicy，
    // 否则 thread 会沿用创建时的那一套。
    let (_, mut handle) = resumed
        .resume_thread(ThreadResumeParams {
            thread_id: thread.thread.id.clone(),
            approvals_reviewer: Some(ApprovalsReviewer::AutoReview),
            approval_policy: Some(AskForApproval::OnRequest),
            ..Default::default()
        })
        .await?;
    resumed
        .start_turn(turn_params(
            &thread.thread.id,
            &prompt,
            &cwd,
            ApprovalsReviewer::AutoReview,
        ))
        .await?;
    let (status, saw_manual_approval) =
        tokio::time::timeout(std::time::Duration::from_secs(180), async {
            let mut saw_manual_approval = false;
            while let Some(message) = handle.recv().await {
                if let Some(status) = completed_status(&message) {
                    return Ok((status, saw_manual_approval));
                }
                if let ThreadMessage::Request { reply, .. } = message {
                    saw_manual_approval = true;
                    drop(reply);
                }
            }
            // actor 会跨临时断线保留 route；提前关闭说明无法恢复。
            anyhow::bail!("message stream closed before auto-review probe completed")
        })
        .await
        .context("auto-review probe timed out")??;
    if probe.exists() {
        tokio::fs::remove_file(&probe).await?;
    }
    assert_eq!(status, TurnStatus::Completed);
    assert!(
        !saw_manual_approval,
        "resumed auto-review thread routed an approval back to the client"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires an administrator-managed Codex daemon; starts an ephemeral thread"]
async fn real_daemon_thread_start_accepts_config_overrides() -> Result<()> {
    let Some(endpoint) = daemon_endpoint() else {
        return Ok(());
    };
    let client = AppServerClient::builder(endpoint).build();
    let (response, _handle) = client
        .start_thread(ThreadStartParams {
            cwd: Some("/tmp".to_owned()),
            ephemeral: Some(true),
            model: Some("gpt-5.6-luna".to_owned()),
            ..Default::default()
        })
        .await?;
    assert_eq!(response.model, "gpt-5.6-luna");
    assert!(response.thread.ephemeral);
    Ok(())
}
