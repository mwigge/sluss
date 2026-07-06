//! Turn model output into a [`Decision`] — defensively. Providers in JSON
//! mode still occasionally wrap output in markdown fences.

use anyhow::{Context, Result};
use serde::Deserialize;
use sluss_core::{Annotation, Decision, Verdict};

/// What the model actually fills in — everything in [`Decision`] except
/// `model`, which the caller stamps.
#[derive(Deserialize)]
struct Draft {
    verdict: Verdict,
    summary: String,
    rationale: String,
    #[serde(default)]
    annotations: Vec<Annotation>,
    confidence: f32,
}

pub fn parse_decision(text: &str, model: &str) -> Result<Decision> {
    let json = strip_fences(text);
    let draft: Draft = serde_json::from_str(json)
        .with_context(|| format!("model output is not a valid decision: {json:.200}"))?;
    Ok(Decision {
        verdict: draft.verdict,
        summary: draft.summary,
        rationale: draft.rationale,
        annotations: draft.annotations,
        confidence: draft.confidence.clamp(0.0, 1.0),
        model: model.to_string(),
    })
}

fn strip_fences(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(inner) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Drop an optional language tag on the fence line, then the closing fence.
    let inner = inner.strip_prefix("json").unwrap_or(inner);
    inner.trim_start_matches(['\r', '\n']).trim_end().trim_end_matches("```").trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = r#"{
        "verdict": "approve",
        "summary": "fine",
        "rationale": "checked it",
        "annotations": [
            {"path": "a.rs", "start_line": 3, "end_line": 4, "severity": "warning", "message": "hm"}
        ],
        "confidence": 0.9
    }"#;

    #[test]
    fn parses_plain_json() {
        let d = parse_decision(RAW, "m1").unwrap();
        assert_eq!(d.verdict, Verdict::Approve);
        assert_eq!(d.model, "m1");
        assert_eq!(d.annotations.len(), 1);
    }

    #[test]
    fn parses_fenced_json() {
        let fenced = format!("```json\n{RAW}\n```");
        assert_eq!(parse_decision(&fenced, "m").unwrap().verdict, Verdict::Approve);
        let bare_fence = format!("```\n{RAW}\n```");
        assert!(parse_decision(&bare_fence, "m").is_ok());
    }

    #[test]
    fn confidence_is_clamped() {
        let hot = RAW.replace("0.9", "3.5");
        assert_eq!(parse_decision(&hot, "m").unwrap().confidence, 1.0);
    }

    #[test]
    fn missing_annotations_defaults_empty() {
        let no_ann = r#"{"verdict":"comment","summary":"s","rationale":"r","confidence":0.5}"#;
        assert!(parse_decision(no_ann, "m").unwrap().annotations.is_empty());
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse_decision("the PR looks nice!", "m").is_err());
    }
}
