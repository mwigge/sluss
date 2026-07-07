//! The review pipeline: snapshot → LLM decision → gate → publish, with an
//! audit event appended at every step — and appended *before* the next
//! step runs, so a crash mid-pipeline still leaves the trail intact up to
//! the point of failure.

use anyhow::{Context, Result};
use serde_json::json;
use sluss_audit::EventRecord;
use sluss_core::{ChangeRef, Decision, Forge, GateOutcome, Snapshot};
use sluss_github::GitHubForge;
use sluss_gitlab::GitLabForge;
use tracing::{info, warn};

use crate::server::App;

/// Run the full pipeline for one change; every step audited. Called from a
/// spawned task — errors land in the audit log, not just the process log.
pub async fn run(app: std::sync::Arc<App>, change: ChangeRef) {
    if let Err(err) = run_inner(&app, &change).await {
        warn!(%err, repo = %change.repo, number = change.number, "pipeline failed");
        audit(&app, &change, "pipeline.error", &json!({ "error": format!("{err:#}") }));
    }
}

async fn run_inner(app: &App, change: &ChangeRef) -> Result<()> {
    let Some(reviewer) = app.reviewer.as_ref() else {
        // No model configured: the webhook is already audited, stop there.
        info!("no reviewer configured, skipping pipeline");
        return Ok(());
    };

    audit(app, change, "pipeline.started", &json!({ "model": reviewer.model() }));

    let snapshot = snapshot(app, change).await?;
    audit(
        app,
        change,
        "snapshot.taken",
        &serde_json::to_value(&snapshot).expect("snapshot serializes"),
    );

    let decision = reviewer.review(&snapshot).await.context("review pass")?;
    audit(
        app,
        change,
        "review.decision",
        &serde_json::to_value(&decision).expect("decision serializes"),
    );

    let (outcome, rules) = app.policy.evaluate_traced(&decision, snapshot.ci_green);
    // The record carries the outcome, every rule checked, and the policy
    // itself — a verdict must be reproducible from this row alone.
    let mut gate_record = serde_json::to_value(&outcome).expect("outcome serializes");
    gate_record["rules"] = serde_json::json!(rules);
    gate_record["policy"] = serde_json::to_value(&app.policy).expect("policy serializes");
    audit(app, change, "gate.outcome", &gate_record);

    let receipt = publish(app, change, &decision, &outcome).await?;
    audit(app, change, "forge.published", &receipt);

    info!(
        repo = %change.repo,
        number = change.number,
        verdict = ?outcome.verdict(),
        "pipeline complete"
    );
    Ok(())
}

async fn snapshot(app: &App, change: &ChangeRef) -> Result<Snapshot> {
    match change.forge {
        Forge::GitHub => forge_github(app)?.snapshot(change).await,
        Forge::GitLab => forge_gitlab(app)?.snapshot(change).await,
    }
}

async fn publish(
    app: &App,
    change: &ChangeRef,
    decision: &Decision,
    outcome: &GateOutcome,
) -> Result<serde_json::Value> {
    match change.forge {
        Forge::GitHub => {
            let receipt = forge_github(app)?.publish(change, decision, outcome).await?;
            Ok(serde_json::to_value(receipt)?)
        }
        Forge::GitLab => {
            let receipt = forge_gitlab(app)?.publish(change, decision, outcome).await?;
            Ok(serde_json::to_value(receipt)?)
        }
    }
}

fn forge_github(app: &App) -> Result<&GitHubForge> {
    app.github
        .as_ref()
        .context("github change but SLUSS_GITHUB_TOKEN is not configured")
}

fn forge_gitlab(app: &App) -> Result<&GitLabForge> {
    app.gitlab
        .as_ref()
        .context("gitlab change but SLUSS_GITLAB_TOKEN is not configured")
}

fn audit(app: &App, change: &ChangeRef, kind: &str, payload: &serde_json::Value) {
    let forge = match change.forge {
        Forge::GitHub => "github",
        Forge::GitLab => "gitlab",
    };
    if let Err(err) = app.audit.append(&EventRecord {
        kind,
        forge: Some(forge),
        repo: Some(&change.repo),
        number: Some(change.number),
        head_sha: Some(&change.head_sha),
        payload,
    }) {
        // The store never rejects appends in normal operation; if it does,
        // scream but keep the pipeline going — the forge-side records
        // (check run / note) still get written.
        warn!(%err, kind, "failed to append audit event");
    }
}
