//! Fetch everything sluss needs to review a PR: title, description, diff,
//! and the state of CI at the pinned head commit.

use anyhow::{Context, Result, bail};
use octocrab::params::repos::Commitish;
use sluss_core::{ChangeRef, Snapshot};

use crate::GitHubForge;

impl GitHubForge {
    /// Take a [`Snapshot`] of a PR at `change.head_sha`.
    ///
    /// Fails if the branch has moved past the pinned sha — the caller
    /// should re-enter the pipeline with a fresh webhook-supplied ref
    /// rather than review a commit nobody is proposing anymore.
    pub async fn snapshot(&self, change: &ChangeRef) -> Result<Snapshot> {
        let (owner, repo) = self.owner_repo(change)?;
        let client = self.client_for(owner, repo).await?;
        let pulls = client.pulls(owner, repo);

        let pr = pulls
            .get(change.number)
            .await
            .with_context(|| format!("fetching PR {}/{}#{}", owner, repo, change.number))?;

        let live_sha = pr.head.as_ref().map(|h| h.sha.as_str()).unwrap_or_default();
        if live_sha != change.head_sha {
            bail!(
                "head moved: reviewing {} but branch is now at {live_sha}",
                change.head_sha
            );
        }

        let diff = pulls
            .get_diff(change.number)
            .await
            .context("fetching PR diff")?;

        let runs = client
            .checks(owner, repo)
            .list_check_runs_for_git_ref(Commitish(change.head_sha.clone()))
            .send()
            .await
            .context("listing check runs for head sha")?;

        let ci: Vec<(String, Option<String>)> = runs
            .check_runs
            .into_iter()
            .map(|run| (run.name, run.conclusion))
            .collect();
        let (ci_green, ci_summary) = summarize_ci(&ci, self.check_name());

        Ok(Snapshot {
            change: change.clone(),
            title: pr.title.unwrap_or_default(),
            description: pr.body.unwrap_or_default(),
            diff,
            ci_green,
            ci_summary,
        })
    }
}

/// Fold check runs into (green, summary), ignoring sluss's own check.
///
/// Green means: at least one relevant check exists and every one of them
/// concluded `success`, `neutral` or `skipped` (the conclusions branch
/// protection treats as passing). No checks at all is *not* green — a repo
/// without CI shouldn't get auto-approvals on the strength of silence.
fn summarize_ci(runs: &[(String, Option<String>)], own_check: &str) -> (bool, String) {
    let relevant: Vec<_> = runs.iter().filter(|(name, _)| name != own_check).collect();
    if relevant.is_empty() {
        return (false, "no CI checks on this commit".into());
    }

    let mut pending = Vec::new();
    let mut failed = Vec::new();
    let mut green = 0usize;
    for (name, conclusion) in &relevant {
        match conclusion.as_deref() {
            Some("success") | Some("neutral") | Some("skipped") => green += 1,
            None => pending.push(name.as_str()),
            Some(_) => failed.push(name.as_str()),
        }
    }

    let all_green = failed.is_empty() && pending.is_empty();
    let mut summary = format!("{green}/{} checks green", relevant.len());
    if !failed.is_empty() {
        summary.push_str(&format!(", failed: {}", failed.join(", ")));
    }
    if !pending.is_empty() {
        summary.push_str(&format!(", pending: {}", pending.join(", ")));
    }
    (all_green, summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, conclusion: Option<&str>) -> (String, Option<String>) {
        (name.into(), conclusion.map(str::to_owned))
    }

    #[test]
    fn all_green() {
        let runs = vec![run("build", Some("success")), run("test", Some("skipped"))];
        let (green, summary) = summarize_ci(&runs, "sluss");
        assert!(green);
        assert_eq!(summary, "2/2 checks green");
    }

    #[test]
    fn failure_is_not_green_and_named() {
        let runs = vec![run("build", Some("failure")), run("test", Some("success"))];
        let (green, summary) = summarize_ci(&runs, "sluss");
        assert!(!green);
        assert_eq!(summary, "1/2 checks green, failed: build");
    }

    #[test]
    fn pending_is_not_green() {
        let runs = vec![run("build", None)];
        let (green, summary) = summarize_ci(&runs, "sluss");
        assert!(!green);
        assert_eq!(summary, "0/1 checks green, pending: build");
    }

    #[test]
    fn own_check_is_ignored() {
        let runs = vec![run("sluss", Some("failure")), run("build", Some("success"))];
        let (green, summary) = summarize_ci(&runs, "sluss");
        assert!(green);
        assert_eq!(summary, "1/1 checks green");
    }

    #[test]
    fn no_ci_at_all_is_not_green() {
        let (green, _) = summarize_ci(&[], "sluss");
        assert!(!green);
        let only_own = vec![run("sluss", Some("success"))];
        assert!(!summarize_ci(&only_own, "sluss").0);
    }
}
