//! GitHub integration.
//!
//! sluss publishes every gate outcome as a check run bound to the head
//! commit — summary, rationale and line annotations live there, and branch
//! protection turns the check into an actual merge gate. An
//! approve/request-changes/comment review is submitted alongside, but the
//! check run is the record.

mod publish;
mod snapshot;

pub use publish::PublishReceipt;

use anyhow::{Context, Result, bail};
use sluss_core::ChangeRef;

/// The check-run name sluss reports under (and excludes from its own view
/// of CI, so a previous sluss verdict never counts as "CI").
pub const DEFAULT_CHECK_NAME: &str = "sluss";

pub struct GitHubForge {
    client: octocrab::Octocrab,
    check_name: String,
}

impl GitHubForge {
    pub fn new(client: octocrab::Octocrab) -> Self {
        Self {
            client,
            check_name: DEFAULT_CHECK_NAME.into(),
        }
    }

    /// Simplest possible auth for early testing. App-installation auth can
    /// come later; note that check-run creation requires App credentials on
    /// GitHub's side, so a personal token only covers snapshot + reviews.
    pub fn from_token(token: impl Into<String>) -> Result<Self> {
        let client = octocrab::Octocrab::builder()
            .personal_token(token.into())
            .build()
            .context("building octocrab client")?;
        Ok(Self::new(client))
    }

    pub fn with_check_name(mut self, name: impl Into<String>) -> Self {
        self.check_name = name.into();
        self
    }

    pub(crate) fn client(&self) -> &octocrab::Octocrab {
        &self.client
    }

    pub(crate) fn check_name(&self) -> &str {
        &self.check_name
    }

    /// Split `owner/repo` out of a change ref, refusing GitLab refs.
    pub(crate) fn owner_repo<'a>(&self, change: &'a ChangeRef) -> Result<(&'a str, &'a str)> {
        if change.forge != sluss_core::Forge::GitHub {
            bail!("GitHubForge got a {:?} change", change.forge);
        }
        change
            .repo
            .split_once('/')
            .with_context(|| format!("repo '{}' is not owner/repo", change.repo))
    }
}
