//! REST client + snapshot for GitLab (gitlab.com or self-hosted).

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sluss_core::{ChangeRef, Snapshot};

pub struct GitLabForge {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) token: String,
}

impl GitLabForge {
    /// `base_url` like `https://gitlab.com` (no trailing slash needed).
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
        }
    }

    /// `/projects/:id` path segment for a `group/project` repo path.
    pub(crate) fn project_id(repo: &str) -> String {
        repo.replace('/', "%2F")
    }

    pub(crate) fn api(&self, path: &str) -> String {
        format!("{}/api/v4{path}", self.base_url)
    }

    pub(crate) async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = self.api(path);
        let response = self
            .http
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("GET {url} -> {status}: {body:.300}");
        }
        response.json().await.with_context(|| format!("decoding GET {url}"))
    }

    /// Take a [`Snapshot`] of an MR at `change.head_sha`. Same contract as
    /// the GitHub side: refuses when the branch has moved past the pin.
    pub async fn snapshot(&self, change: &ChangeRef) -> Result<Snapshot> {
        if change.forge != sluss_core::Forge::GitLab {
            bail!("GitLabForge got a {:?} change", change.forge);
        }
        let project = Self::project_id(&change.repo);
        let mr_path = format!("/projects/{project}/merge_requests/{}", change.number);

        let mr: MergeRequest = self.get_json(&mr_path).await?;
        if mr.sha.as_deref() != Some(change.head_sha.as_str()) {
            bail!(
                "head moved: reviewing {} but MR is now at {}",
                change.head_sha,
                mr.sha.as_deref().unwrap_or("<none>")
            );
        }

        let mut diffs: Vec<FileDiff> = Vec::new();
        for page in 1..=10u8 {
            let batch: Vec<FileDiff> = self
                .get_json(&format!("{mr_path}/diffs?per_page=100&page={page}"))
                .await?;
            let done = batch.len() < 100;
            diffs.extend(batch);
            if done {
                break;
            }
        }

        let pipelines: Vec<Pipeline> = self
            .get_json(&format!(
                "/projects/{project}/pipelines?sha={}&per_page=20",
                change.head_sha
            ))
            .await?;
        let (ci_green, ci_summary) = summarize_pipelines(&pipelines);

        Ok(Snapshot {
            change: change.clone(),
            title: mr.title,
            description: mr.description.unwrap_or_default(),
            diff: assemble_diff(&diffs),
            ci_green,
            ci_summary,
        })
    }
}

#[derive(Deserialize)]
struct MergeRequest {
    title: String,
    description: Option<String>,
    /// Head sha of the MR's source branch.
    sha: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct FileDiff {
    old_path: String,
    new_path: String,
    diff: String,
}

#[derive(Deserialize)]
struct Pipeline {
    status: String,
}

/// Stitch GitLab's per-file diffs into one unified-diff-shaped string.
fn assemble_diff(diffs: &[FileDiff]) -> String {
    let mut out = String::new();
    for d in diffs {
        out.push_str(&format!("--- a/{}\n+++ b/{}\n", d.old_path, d.new_path));
        out.push_str(&d.diff);
        if !d.diff.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// GitLab reports whole pipelines, newest first, so unlike the GitHub side
/// only the latest pipeline for the sha counts. Same stance on silence:
/// no pipeline at all is not green.
fn summarize_pipelines(pipelines: &[Pipeline]) -> (bool, String) {
    match pipelines.first() {
        None => (false, "no pipeline for this commit".into()),
        Some(latest) => (
            latest.status == "success",
            format!("latest pipeline: {}", latest.status),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_assembly_adds_file_headers() {
        let diffs = vec![
            FileDiff {
                old_path: "a.rs".into(),
                new_path: "a.rs".into(),
                diff: "@@ -1 +1 @@\n-x\n+y\n".into(),
            },
            FileDiff {
                old_path: "b.rs".into(),
                new_path: "c.rs".into(),
                diff: "@@ -1 +1 @@\n-1\n+2".into(),
            },
        ];
        let out = assemble_diff(&diffs);
        assert!(out.contains("--- a/a.rs\n+++ b/a.rs\n@@"));
        assert!(out.contains("--- a/b.rs\n+++ b/c.rs\n@@"));
        assert!(out.ends_with("+2\n"));
    }

    #[test]
    fn only_latest_pipeline_counts() {
        let pipelines = vec![
            Pipeline { status: "success".into() },
            Pipeline { status: "failed".into() },
        ];
        assert!(summarize_pipelines(&pipelines).0);
        let red_then_green = vec![
            Pipeline { status: "running".into() },
            Pipeline { status: "success".into() },
        ];
        let (green, summary) = summarize_pipelines(&red_then_green);
        assert!(!green);
        assert_eq!(summary, "latest pipeline: running");
    }

    #[test]
    fn no_pipeline_is_not_green() {
        assert!(!summarize_pipelines(&[]).0);
    }

    #[test]
    fn project_id_is_path_encoded() {
        assert_eq!(GitLabForge::project_id("group/sub/project"), "group%2Fsub%2Fproject");
    }
}
