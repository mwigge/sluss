//! The daemon: axum server receiving forge webhooks. Every verified
//! webhook is appended to the audit store first; review-worthy ones then
//! kick off the pipeline in a background task.

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use sluss_audit::{AuditStore, EventRecord};
use sluss_core::{ChangeRef, Forge, GatePolicy};
use sluss_github::GitHubForge;
use sluss_gitlab::GitLabForge;
use sluss_llm::LlmReviewer;
use tracing::{info, warn};

use crate::{pipeline, verify};

pub struct App {
    pub audit: AuditStore,
    pub github: Option<GitHubForge>,
    pub gitlab: Option<GitLabForge>,
    pub reviewer: Option<LlmReviewer>,
    pub policy: GatePolicy,
    github_webhook_secret: Option<String>,
    gitlab_webhook_token: Option<String>,
}

pub fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sluss=info".into()),
        )
        .init();
    tokio::runtime::Runtime::new()
        .context("building tokio runtime")?
        .block_on(serve())
}

fn app_from_env() -> Result<App> {
    let db_path = std::env::var("SLUSS_DB").unwrap_or_else(|_| "sluss.db".into());
    let mut policy = GatePolicy::default();
    if let Ok(v) = std::env::var("SLUSS_MIN_CONFIDENCE") {
        policy.min_confidence_to_approve = v.parse().context("SLUSS_MIN_CONFIDENCE")?;
    }
    if let Ok(v) = std::env::var("SLUSS_REQUIRE_CI_GREEN") {
        policy.require_ci_green = v.parse().context("SLUSS_REQUIRE_CI_GREEN")?;
    }

    let github = std::env::var("SLUSS_GITHUB_TOKEN")
        .ok()
        .map(GitHubForge::from_token)
        .transpose()?;
    let gitlab = std::env::var("SLUSS_GITLAB_TOKEN").ok().map(|token| {
        let url = std::env::var("SLUSS_GITLAB_URL")
            .unwrap_or_else(|_| "https://gitlab.com".into());
        GitLabForge::new(url, token)
    });
    let reviewer = std::env::var("SLUSS_MODEL")
        .ok()
        .or(Some("claude-sonnet-5".into()))
        .filter(|m| !m.is_empty() && m != "off")
        .map(LlmReviewer::new);

    Ok(App {
        audit: AuditStore::open(&db_path).context("opening audit store")?,
        github,
        gitlab,
        reviewer,
        policy,
        github_webhook_secret: std::env::var("SLUSS_GITHUB_WEBHOOK_SECRET").ok(),
        gitlab_webhook_token: std::env::var("SLUSS_GITLAB_WEBHOOK_TOKEN").ok(),
    })
}

async fn serve() -> Result<()> {
    let app = Arc::new(app_from_env()?);
    for (missing, what) in [
        (app.github_webhook_secret.is_none(), "SLUSS_GITHUB_WEBHOOK_SECRET (github webhooks rejected)"),
        (app.gitlab_webhook_token.is_none(), "SLUSS_GITLAB_WEBHOOK_TOKEN (gitlab webhooks rejected)"),
        (app.github.is_none(), "SLUSS_GITHUB_TOKEN (github pipeline disabled)"),
        (app.gitlab.is_none(), "SLUSS_GITLAB_TOKEN (gitlab pipeline disabled)"),
        (app.reviewer.is_none(), "SLUSS_MODEL=off (review pipeline disabled)"),
    ] {
        if missing {
            warn!("not set: {what}");
        }
    }

    let addr = std::env::var("SLUSS_ADDR").unwrap_or_else(|_| "127.0.0.1:8907".into());
    let router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/webhook/github", post(github_webhook))
        .route("/webhook/gitlab", post(gitlab_webhook))
        .with_state(app);

    info!(%addr, "sluss listening");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    axum::serve(listener, router).await.context("serving")
}

async fn github_webhook(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(secret) = app.github_webhook_secret.as_deref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "webhook secret not configured");
    };
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !verify::github_signature(secret, &body, signature) {
        warn!("github webhook rejected: bad signature");
        return (StatusCode::UNAUTHORIZED, "bad signature");
    }

    let event_type = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "payload is not json"),
    };
    let change = github_change(&payload);

    if let Err(err) = app.audit.append(&EventRecord {
        kind: &format!("webhook.received.{event_type}"),
        forge: Some("github"),
        repo: change.as_ref().map(|c| c.repo.as_str()),
        number: change.as_ref().map(|c| c.number),
        head_sha: change.as_ref().map(|c| c.head_sha.as_str()),
        payload: &payload,
    }) {
        warn!(%err, "failed to append audit event");
        return (StatusCode::INTERNAL_SERVER_ERROR, "audit append failed");
    }

    if event_type == "pull_request"
        && let Some(change) = change
        && github_action_reviews(&payload)
    {
        info!(repo = %change.repo, number = change.number, "starting pipeline");
        tokio::spawn(pipeline::run(app.clone(), change));
    }
    (StatusCode::ACCEPTED, "recorded")
}

/// PR actions that warrant a (re-)review.
fn github_action_reviews(payload: &serde_json::Value) -> bool {
    matches!(
        payload.pointer("/action").and_then(|v| v.as_str()),
        Some("opened" | "synchronize" | "reopened" | "ready_for_review")
    )
}

fn github_change(payload: &serde_json::Value) -> Option<ChangeRef> {
    Some(ChangeRef {
        forge: Forge::GitHub,
        repo: payload.pointer("/repository/full_name")?.as_str()?.to_string(),
        number: payload.pointer("/pull_request/number")?.as_u64()?,
        head_sha: payload.pointer("/pull_request/head/sha")?.as_str()?.to_string(),
    })
}

async fn gitlab_webhook(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(expected) = app.gitlab_webhook_token.as_deref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "webhook token not configured");
    };
    let token = headers
        .get("x-gitlab-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !verify::gitlab_token(expected, token) {
        warn!("gitlab webhook rejected: bad token");
        return (StatusCode::UNAUTHORIZED, "bad token");
    }

    let event_type = headers
        .get("x-gitlab-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_lowercase()
        .replace(' ', "_");
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "payload is not json"),
    };

    let hook = serde_json::from_value::<sluss_gitlab::MergeRequestHook>(payload.clone()).ok();
    let change = hook.as_ref().and_then(|h| h.change_ref());

    if let Err(err) = app.audit.append(&EventRecord {
        kind: &format!("webhook.received.{event_type}"),
        forge: Some("gitlab"),
        repo: change.as_ref().map(|c| c.repo.as_str()),
        number: change.as_ref().map(|c| c.number),
        head_sha: change.as_ref().map(|c| c.head_sha.as_str()),
        payload: &payload,
    }) {
        warn!(%err, "failed to append audit event");
        return (StatusCode::INTERNAL_SERVER_ERROR, "audit append failed");
    }

    let action = hook
        .as_ref()
        .and_then(|h| h.object_attributes.action.as_deref())
        .unwrap_or_default();
    if let Some(change) = change
        && matches!(action, "open" | "reopen" | "update")
    {
        info!(repo = %change.repo, number = change.number, "starting pipeline");
        tokio::spawn(pipeline::run(app.clone(), change));
    }
    (StatusCode::ACCEPTED, "recorded")
}
