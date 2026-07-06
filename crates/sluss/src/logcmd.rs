//! `sluss log [repo [number]]` — read the audit trail. This is the answer
//! to "why did the bot do that?": every pipeline step for a change, in
//! order, from the append-only store.

use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use sluss_audit::{AuditStore, StoredEvent};

pub fn run(args: &[String]) -> Result<()> {
    let repo = args.first().map(String::as_str);
    let number = args
        .get(1)
        .map(|n| n.parse::<u64>().context("number must be an integer"))
        .transpose()?;

    let store = AuditStore::open(crate::db_path()?)?;
    let events = store.events(repo, number, 500)?;

    if events.is_empty() {
        println!("no events{}", match repo {
            Some(r) => format!(" for {r}{}", number.map(|n| format!("#{n}")).unwrap_or_default()),
            None => String::new(),
        });
        return Ok(());
    }
    for event in &events {
        println!("{}", render(event));
    }
    Ok(())
}

fn render(e: &StoredEvent) -> String {
    let when = Utc
        .timestamp_millis_opt(e.at_unix_ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| e.at_unix_ms.to_string());
    let target = match (&e.repo, e.number) {
        (Some(repo), Some(nr)) => format!("{repo}#{nr}"),
        (Some(repo), None) => repo.clone(),
        _ => "-".into(),
    };
    let sha = e
        .head_sha
        .as_deref()
        .map(|s| format!(" @{}", &s[..s.len().min(8)]))
        .unwrap_or_default();
    format!(
        "#{:<5} {when}  {:<28} {target}{sha}  {}",
        e.id,
        e.kind,
        payload_gist(&e.kind, &e.payload)
    )
}

/// One informative line per event kind — the full payload stays in the db.
fn payload_gist(kind: &str, payload: &str) -> String {
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return truncate(payload, 100),
    };
    let gist = if kind.starts_with("review.decision") {
        format!(
            "{} (confidence {}) — {}",
            value.pointer("/verdict").and_then(|v| v.as_str()).unwrap_or("?"),
            value.pointer("/confidence").map(|v| v.to_string()).unwrap_or_default(),
            value.pointer("/summary").and_then(|v| v.as_str()).unwrap_or(""),
        )
    } else if kind.starts_with("gate.outcome") {
        format!(
            "{} -> {}",
            value.pointer("/outcome").and_then(|v| v.as_str()).unwrap_or("?"),
            value
                .pointer("/to")
                .or_else(|| value.pointer("/verdict"))
                .and_then(|v| v.as_str())
                .unwrap_or("?"),
        )
    } else if kind.starts_with("snapshot.taken") {
        format!(
            "ci: {} · diff {} bytes",
            value.pointer("/ci_summary").and_then(|v| v.as_str()).unwrap_or("?"),
            value.pointer("/diff").and_then(|v| v.as_str()).map(str::len).unwrap_or(0),
        )
    } else if kind.starts_with("pipeline.error") {
        truncate(value.pointer("/error").and_then(|v| v.as_str()).unwrap_or(payload), 120)
    } else {
        truncate(&value.to_string(), 100)
    };
    truncate(&gist, 160)
}

fn truncate(s: &str, max: usize) -> String {
    let clean = s.replace('\n', " ");
    if clean.chars().count() <= max {
        return clean;
    }
    let cut: String = clean.chars().take(max).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_gist_shows_verdict() {
        let payload = r#"{"verdict":"approve","confidence":0.9,"summary":"tiny fix"}"#;
        let gist = payload_gist("review.decision", payload);
        assert!(gist.contains("approve"));
        assert!(gist.contains("tiny fix"));
    }

    #[test]
    fn truncate_handles_multibyte() {
        // Must not panic on non-ascii; counts chars, not bytes.
        let s = "åäö".repeat(100);
        assert!(truncate(&s, 10).chars().count() <= 11);
    }
}
