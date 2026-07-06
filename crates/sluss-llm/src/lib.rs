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
    ///
    /// Providers vary in how seriously they take the structured-output
    /// spec, so a non-JSON reply gets exactly one corrective retry with the
    /// bad reply included as context.
    pub async fn review(&self, snapshot: &Snapshot) -> Result<Decision> {
        let mut request = ChatRequest::from_system(prompt::SYSTEM)
            .append_message(ChatMessage::user(prompt::render_user(snapshot)));
        let options = ChatOptions::default()
            .with_response_format(JsonSpec::new("review_decision", prompt::decision_schema()));

        let mut last_err = None;
        for attempt in 0..2 {
            let response = self
                .client
                .exec_chat(self.model.as_str(), request.clone(), Some(&options))
                .await
                .with_context(|| format!("chat request to {} (attempt {})", self.model, attempt + 1))?;
            let text = response
                .first_text()
                .context("model returned no text content")?
                .to_string();

            match parse::parse_decision(&text, &self.model) {
                Ok(decision) => return Ok(decision),
                Err(err) => {
                    request = request
                        .append_message(ChatMessage::assistant(text))
                        .append_message(ChatMessage::user(prompt::RETRY));
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.expect("loop ran").context("model never produced a parseable decision"))
    }
}
