//! Publish a gate outcome to GitHub: a check run pinned to the head commit
//! (the durable, merge-gating record) plus a matching PR review.

use anyhow::{Context, Result};
use octocrab::params::checks::{
    CheckRunConclusion, CheckRunOutput, CheckRunOutputAnnotation, CheckRunOutputAnnotationLevel,
    CheckRunStatus,
};
use serde_json::json;
use sluss_core::{Annotation, ChangeRef, Decision, GateOutcome, Severity, Verdict};

use crate::GitHubForge;

/// GitHub accepts at most 50 annotations per check-run request; more needs
/// paged updates, which we don't do yet — we keep the worst 50 instead.
const MAX_ANNOTATIONS: usize = 50;

/// What was actually posted — the caller appends this to the audit log.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublishReceipt {
    pub check_run_id: u64,
    pub check_run_url: Option<String>,
    pub review_event: &'static str,
    pub annotations_posted: usize,
    pub annotations_dropped: usize,
}

impl GitHubForge {
    /// Create the check run and submit the review for one gate outcome.
    ///
    /// The check run goes first: if review submission then fails, the
    /// durable record already exists and the caller can retry the review.
    pub async fn publish(
        &self,
        change: &ChangeRef,
        decision: &Decision,
        outcome: &GateOutcome,
    ) -> Result<PublishReceipt> {
        let (owner, repo) = self.owner_repo(change)?;
        let verdict = outcome.verdict();

        let (annotations, dropped) = to_annotations(&decision.annotations);
        let check = self
            .client()
            .checks(owner, repo)
            .create_check_run(self.check_name(), change.head_sha.clone())
            .status(CheckRunStatus::Completed)
            .conclusion(conclusion_for(verdict))
            .output(CheckRunOutput {
                title: title_for(verdict).into(),
                summary: render_summary(decision, outcome, dropped),
                text: Some(decision.rationale.clone()),
                annotations,
                images: vec![],
            })
            .send()
            .await
            .context("creating check run")?;

        let review_event = review_event_for(verdict);
        let route = format!("/repos/{owner}/{repo}/pulls/{}/reviews", change.number);
        let _: serde_json::Value = self
            .client()
            .post(
                route,
                Some(&json!({
                    "commit_id": change.head_sha,
                    "event": review_event,
                    "body": render_summary(decision, outcome, dropped),
                })),
            )
            .await
            .context("submitting PR review")?;

        Ok(PublishReceipt {
            check_run_id: check.id.into_inner(),
            check_run_url: check.html_url,
            review_event,
            annotations_posted: decision.annotations.len().min(MAX_ANNOTATIONS),
            annotations_dropped: dropped,
        })
    }
}

fn conclusion_for(verdict: Verdict) -> CheckRunConclusion {
    match verdict {
        Verdict::Approve => CheckRunConclusion::Success,
        Verdict::RequestChanges => CheckRunConclusion::Failure,
        Verdict::Comment => CheckRunConclusion::Neutral,
    }
}

fn review_event_for(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
        Verdict::Comment => "COMMENT",
    }
}

fn title_for(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Approve => "sluss: approved",
        Verdict::RequestChanges => "sluss: changes requested",
        Verdict::Comment => "sluss: comments only",
    }
}

/// Sort annotations worst-first and convert, keeping at most
/// [`MAX_ANNOTATIONS`]. Returns (converted, dropped-count).
fn to_annotations(annotations: &[Annotation]) -> (Vec<CheckRunOutputAnnotation>, usize) {
    let mut sorted: Vec<&Annotation> = annotations.iter().collect();
    sorted.sort_by_key(|a| match a.severity {
        Severity::Failure => 0,
        Severity::Warning => 1,
        Severity::Notice => 2,
    });
    let dropped = sorted.len().saturating_sub(MAX_ANNOTATIONS);
    let converted = sorted
        .into_iter()
        .take(MAX_ANNOTATIONS)
        .map(|a| CheckRunOutputAnnotation {
            path: a.path.clone(),
            start_line: a.start_line as u32,
            end_line: a.end_line as u32,
            start_column: None,
            end_column: None,
            annotation_level: match a.severity {
                Severity::Notice => CheckRunOutputAnnotationLevel::Notice,
                Severity::Warning => CheckRunOutputAnnotationLevel::Warning,
                Severity::Failure => CheckRunOutputAnnotationLevel::Failure,
            },
            message: a.message.clone(),
            title: None,
            raw_details: None,
        })
        .collect();
    (converted, dropped)
}

/// The markdown block used as both check-run summary and review body. Shows
/// what was enacted *and* what the model originally proposed, so a
/// downgrade is always visible on the PR itself, not only in the audit log.
fn render_summary(decision: &Decision, outcome: &GateOutcome, dropped: usize) -> String {
    let mut out = format!("**{}**\n\n{}\n", verdict_label(outcome.verdict()), decision.summary);
    if let GateOutcome::Downgrade { from, reason, .. } = outcome {
        out.push_str(&format!(
            "\n> gate: model proposed *{}* but this was downgraded — {reason}\n",
            verdict_label(*from),
        ));
    }
    out.push_str(&format!(
        "\n---\nmodel: `{}` · confidence: {:.2}",
        decision.model, decision.confidence
    ));
    if dropped > 0 {
        out.push_str(&format!(" · {dropped} annotation(s) over the 50 limit not shown"));
    }
    out
}

fn verdict_label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Approve => "approve",
        Verdict::RequestChanges => "request changes",
        Verdict::Comment => "comment",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annotation(severity: Severity) -> Annotation {
        Annotation {
            path: "src/lib.rs".into(),
            start_line: 1,
            end_line: 1,
            severity,
            message: "hm".into(),
        }
    }

    fn decision() -> Decision {
        Decision {
            verdict: Verdict::Approve,
            summary: "small, well-tested change".into(),
            rationale: "because reasons".into(),
            annotations: vec![],
            confidence: 0.91,
            model: "test-model".into(),
        }
    }

    #[test]
    fn annotations_capped_worst_first() {
        let mut many: Vec<Annotation> = (0..60).map(|_| annotation(Severity::Notice)).collect();
        many.push(annotation(Severity::Failure));
        let (converted, dropped) = to_annotations(&many);
        assert_eq!(converted.len(), 50);
        assert_eq!(dropped, 11);
        assert!(matches!(
            converted[0].annotation_level,
            CheckRunOutputAnnotationLevel::Failure
        ));
    }

    #[test]
    fn downgrade_is_visible_in_summary() {
        let outcome = GateOutcome::Downgrade {
            from: Verdict::Approve,
            to: Verdict::Comment,
            reason: "CI is not green".into(),
        };
        let text = render_summary(&decision(), &outcome, 0);
        assert!(text.contains("**comment**"));
        assert!(text.contains("model proposed *approve*"));
        assert!(text.contains("CI is not green"));
    }

    #[test]
    fn enact_summary_has_no_gate_note() {
        let outcome = GateOutcome::Enact {
            verdict: Verdict::Approve,
        };
        let text = render_summary(&decision(), &outcome, 0);
        assert!(text.contains("**approve**"));
        assert!(!text.contains("downgraded"));
    }
}
