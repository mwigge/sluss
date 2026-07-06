//! Aggregate read-side queries for `sluss dash`. All plain SQL over the
//! append-only events table — small data, no analytics engine needed.

use anyhow::Result;
use rusqlite::params;

use crate::AuditStore;

/// One row of the recent-decisions table.
#[derive(Debug, Clone)]
pub struct DecisionRow {
    pub at_unix_ms: i64,
    pub repo: String,
    pub number: u64,
    pub verdict: String,
    pub confidence: f64,
    pub summary: String,
}

/// Pipeline wall-clock for one published outcome.
#[derive(Debug, Clone)]
pub struct LatencyRow {
    pub repo: String,
    pub number: u64,
    pub millis: i64,
}

/// A published outcome joined with its decision and cost.
#[derive(Debug, Clone)]
pub struct OutcomeRow {
    pub at_unix_ms: i64,
    pub repo: String,
    pub number: u64,
    pub verdict: String,
    pub confidence: f64,
    pub millis: i64,
}

impl AuditStore {
    /// Decisions per repo, busiest first.
    pub fn decisions_per_repo(&self) -> Result<Vec<(String, u64)>> {
        let conn = self.conn.lock().expect("poisoned");
        let mut stmt = conn.prepare(
            "SELECT COALESCE(repo, '?'), COUNT(*) FROM events
             WHERE kind = 'review.decision' GROUP BY repo ORDER BY COUNT(*) DESC",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Model verdict counts plus how many approvals the gate downgraded and
    /// how many human overrides were recorded.
    pub fn verdict_breakdown(&self) -> Result<Vec<(String, u64)>> {
        let conn = self.conn.lock().expect("poisoned");
        let mut out: Vec<(String, u64)> = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT json_extract(payload, '$.verdict'), COUNT(*) FROM events
             WHERE kind = 'review.decision' GROUP BY 1 ORDER BY 2 DESC",
        )?;
        for row in stmt.query_map([], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, u64>(1)?))
        })? {
            let (verdict, n) = row?;
            out.push((verdict.unwrap_or_else(|| "?".into()), n));
        }
        for (label, sql) in [
            (
                "downgraded",
                "SELECT COUNT(*) FROM events WHERE kind = 'gate.outcome'
                 AND json_extract(payload, '$.outcome') = 'downgrade'",
            ),
            (
                "overridden",
                "SELECT COUNT(*) FROM events WHERE kind = 'human.override'",
            ),
        ] {
            let n: u64 = conn.query_row(sql, [], |r| r.get(0))?;
            if n > 0 {
                out.push((label.into(), n));
            }
        }
        Ok(out)
    }

    /// Wall-clock from each pipeline start to its published outcome.
    pub fn pipeline_latencies(&self, limit: u64) -> Result<Vec<LatencyRow>> {
        let conn = self.conn.lock().expect("poisoned");
        let mut stmt = conn.prepare(
            "SELECT p.repo, p.number, p.at_unix_ms - (
                 SELECT MAX(s.at_unix_ms) FROM events s
                 WHERE s.kind = 'pipeline.started' AND s.repo = p.repo
                   AND s.number = p.number AND s.head_sha = p.head_sha
                   AND s.at_unix_ms <= p.at_unix_ms)
             FROM events p WHERE p.kind = 'forge.published'
             ORDER BY p.id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(LatencyRow {
                    repo: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    number: r.get::<_, Option<u64>>(1)?.unwrap_or_default(),
                    millis: r.get::<_, Option<i64>>(2)?.unwrap_or_default(),
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Newest decisions first.
    pub fn recent_decisions(&self, limit: u64) -> Result<Vec<DecisionRow>> {
        let conn = self.conn.lock().expect("poisoned");
        let mut stmt = conn.prepare(
            "SELECT at_unix_ms, COALESCE(repo, '?'), COALESCE(number, 0),
                    COALESCE(json_extract(payload, '$.verdict'), '?'),
                    COALESCE(json_extract(payload, '$.confidence'), 0.0),
                    COALESCE(json_extract(payload, '$.summary'), '')
             FROM events WHERE kind = 'review.decision'
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(DecisionRow {
                    at_unix_ms: r.get(0)?,
                    repo: r.get(1)?,
                    number: r.get(2)?,
                    verdict: r.get(3)?,
                    confidence: r.get(4)?,
                    summary: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Events per hour for the trailing `hours`, oldest bucket first —
    /// sparkline fodder. `now_ms` is passed in so the query is testable.
    pub fn events_per_hour(&self, hours: u64, now_ms: i64) -> Result<Vec<u64>> {
        let conn = self.conn.lock().expect("poisoned");
        let start = now_ms - (hours as i64) * 3_600_000;
        let mut buckets = vec![0u64; hours as usize];
        let mut stmt = conn.prepare(
            "SELECT (at_unix_ms - ?1) / 3600000, COUNT(*) FROM events
             WHERE at_unix_ms >= ?1 GROUP BY 1",
        )?;
        for row in stmt.query_map(params![start], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, u64>(1)?))
        })? {
            let (bucket, n) = row?;
            if let Some(slot) = buckets.get_mut(bucket.max(0) as usize) {
                *slot = n;
            }
        }
        Ok(buckets)
    }

    /// One row per published outcome with the decision that drove it and
    /// the pipeline time it cost — the raw material for velocity/value
    /// metrics (weighting and rates are computed in the dash, not in SQL).
    pub fn decision_outcomes(&self, limit: u64) -> Result<Vec<OutcomeRow>> {
        let conn = self.conn.lock().expect("poisoned");
        let mut stmt = conn.prepare(
            "SELECT p.at_unix_ms, COALESCE(p.repo,'?'), COALESCE(p.number,0),
                (SELECT json_extract(d.payload,'$.verdict') FROM events d
                 WHERE d.kind='review.decision' AND d.repo=p.repo AND d.number=p.number
                   AND d.head_sha=p.head_sha AND d.at_unix_ms<=p.at_unix_ms
                 ORDER BY d.id DESC LIMIT 1),
                (SELECT json_extract(d.payload,'$.confidence') FROM events d
                 WHERE d.kind='review.decision' AND d.repo=p.repo AND d.number=p.number
                   AND d.head_sha=p.head_sha AND d.at_unix_ms<=p.at_unix_ms
                 ORDER BY d.id DESC LIMIT 1),
                p.at_unix_ms - (SELECT MAX(s.at_unix_ms) FROM events s
                 WHERE s.kind='pipeline.started' AND s.repo=p.repo
                   AND s.number=p.number AND s.head_sha=p.head_sha
                   AND s.at_unix_ms<=p.at_unix_ms)
             FROM events p WHERE p.kind='forge.published'
             ORDER BY p.id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(OutcomeRow {
                    at_unix_ms: r.get(0)?,
                    repo: r.get(1)?,
                    number: r.get(2)?,
                    verdict: r.get::<_, Option<String>>(3)?.unwrap_or_else(|| "?".into()),
                    confidence: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    millis: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// The gate's most recent enacted verdict for a change, if any —
    /// used to detect human overrides at merge time.
    pub fn last_enacted_verdict(&self, repo: &str, number: u64) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("poisoned");
        let verdict = conn
            .query_row(
                "SELECT COALESCE(json_extract(payload, '$.to'),
                                 json_extract(payload, '$.verdict'))
                 FROM events WHERE kind = 'gate.outcome' AND repo = ?1 AND number = ?2
                 ORDER BY id DESC LIMIT 1",
                params![repo, number],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap_or(None);
        Ok(verdict)
    }
}

#[cfg(test)]
mod tests {
    use crate::{AuditStore, EventRecord};

    fn seed(store: &AuditStore, kind: &str, repo: &str, number: u64, payload: serde_json::Value) {
        store
            .append(&EventRecord {
                kind,
                forge: Some("github"),
                repo: Some(repo),
                number: Some(number),
                head_sha: Some("abc"),
                payload: &payload,
            })
            .unwrap();
    }

    #[test]
    fn breakdown_and_recent() {
        let store = AuditStore::open_in_memory().unwrap();
        seed(&store, "review.decision", "a/b", 1,
            serde_json::json!({"verdict":"approve","confidence":0.9,"summary":"ok"}));
        seed(&store, "review.decision", "a/b", 2,
            serde_json::json!({"verdict":"comment","confidence":0.4,"summary":"hm"}));
        seed(&store, "gate.outcome", "a/b", 1,
            serde_json::json!({"outcome":"downgrade","from":"approve","to":"comment","reason":"x"}));

        let breakdown = store.verdict_breakdown().unwrap();
        assert!(breakdown.contains(&("approve".into(), 1)));
        assert!(breakdown.contains(&("downgraded".into(), 1)));
        assert_eq!(store.recent_decisions(10).unwrap().len(), 2);
        assert_eq!(store.decisions_per_repo().unwrap()[0], ("a/b".into(), 2));
        assert_eq!(store.last_enacted_verdict("a/b", 1).unwrap().as_deref(), Some("comment"));
        assert_eq!(store.last_enacted_verdict("a/b", 99).unwrap(), None);
    }

    #[test]
    fn latency_pairs_start_with_publish() {
        let store = AuditStore::open_in_memory().unwrap();
        seed(&store, "pipeline.started", "a/b", 1, serde_json::json!({}));
        seed(&store, "forge.published", "a/b", 1, serde_json::json!({}));
        let lat = store.pipeline_latencies(10).unwrap();
        assert_eq!(lat.len(), 1);
        assert!(lat[0].millis >= 0);
    }
}
