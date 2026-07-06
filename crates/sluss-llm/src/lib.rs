//! The reviewer. Takes a snapshot of a change, asks a model for a
//! structured [`Decision`], and hands it back — nothing here talks to a
//! forge, and nothing here gets to enact anything. That's the gate's job.

mod parse;
mod prompt;

use anyhow::{Context, Result};
use genai::chat::{ChatMessage, ChatOptions, ChatRequest, JsonSpec};
use sluss_core::{Decision, Snapshot};

pub struct LlmReviewer {
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
    pub async fn review(&self, snapshot: &Snapshot) -> Result<Decision> {
        let request = ChatRequest::from_system(prompt::SYSTEM)
            .append_message(ChatMessage::user(prompt::render_user(snapshot)));
        let options = ChatOptions::default()
            .with_response_format(JsonSpec::new("review_decision", prompt::decision_schema()));

        let response = self
            .client
            .exec_chat(self.model.as_str(), request, Some(&options))
            .await
            .with_context(|| format!("chat request to {}", self.model))?;
        let text = response
            .first_text()
            .context("model returned no text content")?;

        parse::parse_decision(text, &self.model)
    }
}
