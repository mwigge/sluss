//! GitHub integration.
//!
//! The plan (per the Checks API model): sluss runs as a GitHub App, and every
//! gate outcome is published as a check run bound to the head commit — that
//! check carries the summary, rationale and line annotations, and branch
//! protection turns it into an actual merge gate. Approve/request-changes
//! reviews are posted alongside, but the check run is the record.

use anyhow::Result;
use sluss_core::{ChangeRef, Decision, GateOutcome};

pub struct GitHubForge {
    #[allow(dead_code)] // read once publish() is implemented
    client: octocrab::Octocrab,
}

impl GitHubForge {
    pub fn new(client: octocrab::Octocrab) -> Self {
        Self { client }
    }

    /// Fetch diff, description and CI status for a PR.
    pub async fn snapshot(&self, _change: &ChangeRef) -> Result<String> {
        todo!("fetch PR title/body/diff/check-suite status via octocrab")
    }

    /// Publish a gate outcome: create the check run (with annotations from
    /// the decision) and, when the outcome enacts a verdict, submit the
    /// matching review.
    pub async fn publish(
        &self,
        _change: &ChangeRef,
        _decision: &Decision,
        _outcome: &GateOutcome,
    ) -> Result<()> {
        todo!("create check run + submit review via octocrab")
    }
}
