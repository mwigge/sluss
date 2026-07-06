//! Publish a gate outcome to GitLab: a commit status pinned to the head sha
//! (the merge gate, when the project requires the `sluss` status), an MR
//! note carrying the full rationale, and an approve/unapprove call.

use anyhow::{Context, Result, bail};
use serde_json::json;
use sluss_core::{Annotation, ChangeRef, Decision, GateOutcome, Severity, Verdict};

use crate::forge::GitLabForge;

/// What was actually posted — the caller appends this to the audit log.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GitLabReceipt {
    /// `success` / `failed`, or None for a comment-only verdict (a comment
    /// must not move the gate either way).
    pub commit_status: Option<&'static str>,
    pub note_id: Option<u64>,
    pub approval: &'static str,
}

impl GitLabForge {
    async fn post_json(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
        let url = self.api(path);
        let response = self
            .http
            .post(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("POST {url} -> {status}: {text:.300}");
        }
        Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
    }

    /// Publish one gate outcome. Order matters for the same reason as on
    /// GitHub: the gating commit status goes first, then the note, then the
    /// approval state.
    pub async fn publish(
        &self,
        change: &ChangeRef,
        decision: &Decision,
        outcome: &GateOutcome,
    ) -> Result<GitLabReceipt> {
        let project = Self::project_id(&change.repo);
        let verdict = outcome.verdict();

        let commit_status = status_for(verdict);
        if let Some(state) = commit_status {
            self.post_json(
                &format!("/projects/{project}/statuses/{}", change.head_sha),
                &json!({
                    "state": state,
                    "name": "sluss",
                    "description": decision.summary,
                }),
            )
            .await
            .context("posting commit status")?;
        }

        let note = self
            .post_json(
                &format!("/projects/{project}/merge_requests/{}/notes", change.number),
                &json!({ "body": render_note(decision, outcome) }),
            )
            .await
            .context("posting MR note")?;
        let note_id = note.pointer("/id").and_then(|v| v.as_u64());

        let approval = match verdict {
            Verdict::Approve => {
                // `sha` makes GitLab reject the approval if the MR moved
                // between our snapshot and now.
                self.post_json(
                    &format!("/projects/{project}/merge_requests/{}/approve", change.number),
                    &json!({ "sha": change.head_sha }),
                )
                .await
                .context("approving MR")?;
                "approved"
            }
            Verdict::RequestChanges => {
                // Withdraw any earlier sluss approval; 404 here just means
                // there was none.
                let path = format!(
                    "/projects/{project}/merge_requests/{}/unapprove",
                    change.number
                );
                match self.post_json(&path, &json!({})).await {
                    Ok(_) => "unapproved",
                    Err(err) if err.to_string().contains("404") => "no approval to withdraw",
                    Err(err) => return Err(err.context("unapproving MR")),
                }
            }
            Verdict::Comment => "unchanged",
        };

        Ok(GitLabReceipt {
            commit_status,
            note_id,
            approval,
        })
    }
}

/// GitLab commit statuses have no `neutral`: a comment-only verdict posts
/// no status at all rather than pretending success or failure.
fn status_for(verdict: Verdict) -> Option<&'static str> {
    match verdict {
        Verdict::Approve => Some("success"),
        Verdict::RequestChanges => Some("failed"),
        Verdict::Comment => None,
    }
}

/// The MR note: same content as the GitHub check run, with annotations
/// rendered inline since line-anchored discussions need diff position refs
/// we don't track yet.
fn render_note(decision: &Decision, outcome: &GateOutcome) -> String {
    let verdict = outcome.verdict();
    let mut out = format!("**sluss: {}**\n\n{}\n", label(verdict), decision.summary);
    if let GateOutcome::Downgrade { from, reason, .. } = outcome {
        out.push_str(&format!(
            "\n> gate: model proposed *{}* but this was downgraded — {reason}\n",
            label(*from)
        ));
    }
    if !decision.annotations.is_empty() {
        out.push_str("\n");
        for a in &decision.annotations {
            out.push_str(&render_annotation(a));
        }
    }
    out.push_str(&format!(
        "\n<details><summary>rationale</summary>\n\n{}\n</details>\n\n---\nmodel: `{}` · confidence: {:.2}",
        decision.rationale, decision.model, decision.confidence
    ));
    out
}

fn render_annotation(a: &Annotation) -> String {
    let icon = match a.severity {
        Severity::Failure => "🔴",
        Severity::Warning => "🟡",
        Severity::Notice => "🔵",
    };
    format!("- {icon} `{}:{}-{}` {}\n", a.path, a.start_line, a.end_line, a.message)
}

fn label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Approve => "approved",
        Verdict::RequestChanges => "changes requested",
        Verdict::Comment => "comments only",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(verdict: Verdict) -> Decision {
        Decision {
            verdict,
            summary: "sum".into(),
            rationale: "why".into(),
            annotations: vec![Annotation {
                path: "x.rs".into(),
                start_line: 5,
                end_line: 6,
                severity: Severity::Warning,
                message: "careful".into(),
            }],
            confidence: 0.7,
            model: "m".into(),
        }
    }

    #[test]
    fn comment_posts_no_commit_status() {
        assert_eq!(status_for(Verdict::Comment), None);
        assert_eq!(status_for(Verdict::Approve), Some("success"));
        assert_eq!(status_for(Verdict::RequestChanges), Some("failed"));
    }

    #[test]
    fn note_carries_annotations_and_downgrade() {
        let outcome = GateOutcome::Downgrade {
            from: Verdict::Approve,
            to: Verdict::Comment,
            reason: "CI is not green".into(),
        };
        let note = render_note(&decision(Verdict::Approve), &outcome);
        assert!(note.contains("**sluss: comments only**"));
        assert!(note.contains("`x.rs:5-6` careful"));
        assert!(note.contains("model proposed *approved*"));
        assert!(note.contains("CI is not green"));
    }
}
