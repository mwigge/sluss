//! The reviewer. Takes a snapshot of a change, asks a model for a
//! structured [`Decision`], and hands it back — nothing here talks to a
//! forge, and nothing here gets to enact anything. That's the gate's job.

use anyhow::Result;
use sluss_core::Decision;

/// Everything the model gets to see for one review pass.
#[derive(Debug, Clone)]
pub struct ReviewInput {
    pub title: String,
    pub description: String,
    pub diff: String,
    /// Human-readable CI summary ("all 14 checks green", "build failed: ...").
    pub ci_summary: String,
}

pub struct LlmReviewer {
    #[allow(dead_code)] // read once review() is implemented
    client: genai::Client,
    model: String,
}

impl LlmReviewer {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            client: genai::Client::default(),
            model: model.into(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// One review pass: prompt with the snapshot, demand structured output
    /// matching [`Decision`], return it verbatim (the caller audits it
    /// before gating).
    pub async fn review(&self, _input: &ReviewInput) -> Result<Decision> {
        todo!("chat request via genai with JSON-schema structured output for Decision")
    }
}
