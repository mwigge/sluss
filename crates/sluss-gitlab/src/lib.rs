//! GitLab integration.
//!
//! The `gitlab` crate ships no webhook payload types, so the merge request
//! hook is deserialized with our own structs — only the fields sluss needs,
//! everything else ignored. On the GitLab side the merge gate will be an
//! external status check plus the MR approvals API.

use serde::Deserialize;
use sluss_core::{ChangeRef, Forge};

/// A GitLab merge request webhook (`object_kind: "merge_request"`).
/// Field reference: GitLab docs, "Webhook events" → "Merge request events".
#[derive(Debug, Deserialize)]
pub struct MergeRequestHook {
    pub object_kind: String,
    pub project: Project,
    pub object_attributes: MergeRequestAttrs,
}

#[derive(Debug, Deserialize)]
pub struct Project {
    pub path_with_namespace: String,
}

#[derive(Debug, Deserialize)]
pub struct MergeRequestAttrs {
    /// The per-project MR number (what shows up in `!42`).
    pub iid: u64,
    pub title: String,
    /// `open`, `update`, `merge`, `close`, `approved`, ...
    pub action: Option<String>,
    pub state: Option<String>,
    pub last_commit: Option<LastCommit>,
}

#[derive(Debug, Deserialize)]
pub struct LastCommit {
    /// Head commit SHA.
    pub id: String,
}

impl MergeRequestHook {
    /// The commit-pinned change this hook refers to, if the payload carries
    /// a head commit.
    pub fn change_ref(&self) -> Option<ChangeRef> {
        Some(ChangeRef {
            forge: Forge::GitLab,
            repo: self.project.path_with_namespace.clone(),
            number: self.object_attributes.iid,
            head_sha: self.object_attributes.last_commit.as_ref()?.id.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_mr_hook() {
        let json = r#"{
            "object_kind": "merge_request",
            "project": { "path_with_namespace": "morgan/smedja" },
            "object_attributes": {
                "iid": 7,
                "title": "fix input wrap",
                "action": "open",
                "state": "opened",
                "last_commit": { "id": "abc123" }
            }
        }"#;
        let hook: MergeRequestHook = serde_json::from_str(json).unwrap();
        let change = hook.change_ref().unwrap();
        assert_eq!(change.repo, "morgan/smedja");
        assert_eq!(change.number, 7);
        assert_eq!(change.head_sha, "abc123");
    }
}
