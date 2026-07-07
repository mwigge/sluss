//! Domain types shared by every sluss crate, plus the deterministic gate.
//!
//! The one rule that shapes everything here: the model proposes, the gate
//! disposes. An LLM produces a [`Decision`]; only [`GatePolicy::evaluate`]
//! (plain, testable, deterministic code) decides what actually happens on
//! the forge.

use serde::{Deserialize, Serialize};

/// Which forge a change lives on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Forge {
    GitHub,
    GitLab,
}

/// A pull request or merge request, pinned to an exact commit.
///
/// Every decision is anchored to `head_sha` — if the branch moves, the old
/// decision stays valid for the old commit and a new run is required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRef {
    pub forge: Forge,
    /// `owner/repo` on GitHub, `group/project` on GitLab.
    pub repo: String,
    /// PR number / MR iid.
    pub number: u64,
    pub head_sha: String,
}

/// Everything sluss observed about a change at one point in time — the
/// input to a review pass, and itself an audit artifact (stored before any
/// model sees it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub change: ChangeRef,
    pub title: String,
    pub description: String,
    pub diff: String,
    /// True only when every relevant CI check has concluded green.
    pub ci_green: bool,
    /// Human-readable CI state ("4/4 checks green", "pending: build").
    pub ci_summary: String,
}

/// What the reviewer wants to happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Approve,
    RequestChanges,
    /// Neither approve nor block — just leave observations.
    Comment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Notice,
    Warning,
    Failure,
}

/// A line-anchored remark, mapped 1:1 onto check-run annotations (GitHub)
/// or diff discussions (GitLab).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    pub path: String,
    pub start_line: u64,
    pub end_line: u64,
    pub severity: Severity,
    pub message: String,
}

/// The structured output of one LLM review pass.
///
/// This is a *proposal*. It is stored verbatim in the audit log before the
/// gate ever looks at it, so the record always shows what the model said,
/// not just what was enacted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub verdict: Verdict,
    /// One-paragraph summary, shown as the check-run / review headline.
    pub summary: String,
    /// The full reasoning behind the verdict.
    pub rationale: String,
    pub annotations: Vec<Annotation>,
    /// Model's own confidence in the verdict, 0.0..=1.0.
    pub confidence: f32,
    /// Which model produced this (e.g. `claude-sonnet-5`).
    pub model: String,
}

/// Deterministic policy applied to every [`Decision`] before anything is
/// posted to a forge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatePolicy {
    /// Approvals below this confidence are downgraded to comments.
    pub min_confidence_to_approve: f32,
    /// Never approve while CI is red or pending.
    pub require_ci_green: bool,
}

impl Default for GatePolicy {
    fn default() -> Self {
        Self {
            min_confidence_to_approve: 0.8,
            require_ci_green: true,
        }
    }
}

/// What the gate decided to actually do with a [`Decision`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GateOutcome {
    /// The proposal passes policy as-is.
    Enact { verdict: Verdict },
    /// The proposal was weakened by policy; `reason` says why.
    Downgrade {
        from: Verdict,
        to: Verdict,
        reason: String,
    },
}

impl GateOutcome {
    pub fn verdict(&self) -> Verdict {
        match self {
            GateOutcome::Enact { verdict } => *verdict,
            GateOutcome::Downgrade { to, .. } => *to,
        }
    }
}

impl GatePolicy {
    /// The gate. Pure function of (policy, decision, ci state) — no I/O, no
    /// model in the loop, trivially unit-testable.
    pub fn evaluate(&self, decision: &Decision, ci_green: bool) -> GateOutcome {
        self.evaluate_traced(decision, ci_green).0
    }

    /// The gate, with its full rule trace: every rule checked, in fixed
    /// order, with its outcome — pass the trace into the audit log so the
    /// record shows not just what was decided but everything that was
    /// checked. (A lesson carried back from tumult's autopilot gate: the
    /// rule trace IS the audit record.)
    pub fn evaluate_traced(
        &self,
        decision: &Decision,
        ci_green: bool,
    ) -> (GateOutcome, Vec<(&'static str, bool)>) {
        let approving = decision.verdict == Verdict::Approve;
        let ci_ok = !self.require_ci_green || ci_green;
        let confidence_ok = decision.confidence >= self.min_confidence_to_approve;
        // Rules are always all evaluated, in this order, even after one
        // fails — the trace must be complete to be worth keeping.
        let rules = vec![
            ("ci.green_when_required", ci_ok),
            ("confidence.at_threshold", confidence_ok),
        ];

        let outcome = if approving && !ci_ok {
            GateOutcome::Downgrade {
                from: Verdict::Approve,
                to: Verdict::Comment,
                reason: "CI is not green".into(),
            }
        } else if approving && !confidence_ok {
            GateOutcome::Downgrade {
                from: Verdict::Approve,
                to: Verdict::Comment,
                reason: format!(
                    "confidence {:.2} below approval threshold {:.2}",
                    decision.confidence, self.min_confidence_to_approve
                ),
            }
        } else {
            GateOutcome::Enact {
                verdict: decision.verdict,
            }
        };
        (outcome, rules)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(verdict: Verdict, confidence: f32) -> Decision {
        Decision {
            verdict,
            summary: "looks fine".into(),
            rationale: "tests pass, change is small".into(),
            annotations: vec![],
            confidence,
            model: "test-model".into(),
        }
    }

    #[test]
    fn approve_passes_when_ci_green_and_confident() {
        let out = GatePolicy::default().evaluate(&decision(Verdict::Approve, 0.95), true);
        assert_eq!(out.verdict(), Verdict::Approve);
    }

    #[test]
    fn approve_downgrades_on_red_ci() {
        let out = GatePolicy::default().evaluate(&decision(Verdict::Approve, 0.95), false);
        assert_eq!(out.verdict(), Verdict::Comment);
    }

    #[test]
    fn approve_downgrades_on_low_confidence() {
        let out = GatePolicy::default().evaluate(&decision(Verdict::Approve, 0.5), true);
        assert_eq!(out.verdict(), Verdict::Comment);
    }

    #[test]
    fn request_changes_never_downgraded() {
        let out = GatePolicy::default().evaluate(&decision(Verdict::RequestChanges, 0.1), false);
        assert_eq!(out.verdict(), Verdict::RequestChanges);
    }
}
