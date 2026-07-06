//! The sluss daemon: an axum server that receives forge webhooks, verifies
//! them, and appends every single one to the audit store before anything
//! else happens. The review pipeline (snapshot → LLM decision → gate →
//! publish) hangs off of this — but the first invariant is already live:
//! nothing enters sluss without leaving a trace.

mod verify;

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use sluss_audit::{AuditStore, EventRecord};
use tracing::{info, warn};

struct App {
    audit: AuditStore,
    github_webhook_secret: Option<String>,
    gitlab_webhook_token: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sluss=info,tower_http=info".into()),
        )
        .init();

    let db_path = std::env::var("SLUSS_DB").unwrap_or_else(|_| "sluss.db".into());
    let addr = std::env::var("SLUSS_ADDR").unwrap_or_else(|_| "127.0.0.1:8907".into());

    let app = Arc::new(App {
        audit: AuditStore::open(&db_path).context("opening audit store")?,
        github_webhook_secret: std::env::var("SLUSS_GITHUB_WEBHOOK_SECRET").ok(),
        gitlab_webhook_token: std::env::var("SLUSS_GITLAB_WEBHOOK_TOKEN").ok(),
    });

    if app.github_webhook_secret.is_none() {
        warn!("SLUSS_GITHUB_WEBHOOK_SECRET not set — github webhooks will be rejected");
    }
    if app.gitlab_webhook_token.is_none() {
        warn!("SLUSS_GITLAB_WEBHOOK_TOKEN not set — gitlab webhooks will be rejected");
    }

    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/webhook/github", post(github_webhook))
        .route("/webhook/gitlab", post(gitlab_webhook))
        .with_state(app);

    info!(%addr, db = %db_path, "sluss listening");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    axum::serve(listener, router).await.context("serving")?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
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

    let repo = payload
        .pointer("/repository/full_name")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let number = payload.pointer("/pull_request/number").and_then(|v| v.as_u64());
    let head_sha = payload
        .pointer("/pull_request/head/sha")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    if let Err(err) = app.audit.append(&EventRecord {
        kind: &format!("webhook.received.{event_type}"),
        forge: Some("github"),
        repo: repo.as_deref(),
        number,
        head_sha: head_sha.as_deref(),
        payload: &payload,
    }) {
        warn!(%err, "failed to append audit event");
        return (StatusCode::INTERNAL_SERVER_ERROR, "audit append failed");
    }

    info!(event_type, repo = repo.as_deref().unwrap_or("-"), "github webhook recorded");
    // TODO: enqueue review pipeline for pull_request opened/synchronize
    (StatusCode::ACCEPTED, "recorded")
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
        .unwrap_or("unknown");
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "payload is not json"),
    };

    // Best-effort typed parse for MR hooks; raw payload is audited either way.
    let change = serde_json::from_value::<sluss_gitlab::MergeRequestHook>(payload.clone())
        .ok()
        .and_then(|hook| hook.change_ref());

    if let Err(err) = app.audit.append(&EventRecord {
        kind: &format!("webhook.received.{}", event_type.to_lowercase().replace(' ', "_")),
        forge: Some("gitlab"),
        repo: change.as_ref().map(|c| c.repo.as_str()),
        number: change.as_ref().map(|c| c.number),
        head_sha: change.as_ref().map(|c| c.head_sha.as_str()),
        payload: &payload,
    }) {
        warn!(%err, "failed to append audit event");
        return (StatusCode::INTERNAL_SERVER_ERROR, "audit append failed");
    }

    info!(event_type, "gitlab webhook recorded");
    // TODO: enqueue review pipeline for merge_request open/update
    (StatusCode::ACCEPTED, "recorded")
}
