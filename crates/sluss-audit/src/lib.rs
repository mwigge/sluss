//! The audit store. SQLite, append-only, enforced by triggers.
//!
//! Everything sluss does or observes becomes a row in `events`: webhook
//! received, snapshot taken, LLM decision proposed, gate outcome, action
//! posted to the forge. Rows can never be updated or deleted — the schema
//! itself raises on UPDATE/DELETE, so even a buggy caller can't rewrite
//! history. Answering "why did the bot approve PR #42?" is a SELECT.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    at_unix_ms  INTEGER NOT NULL,
    kind        TEXT    NOT NULL,
    forge       TEXT,
    repo        TEXT,
    number      INTEGER,
    head_sha    TEXT,
    payload     TEXT    NOT NULL
);

CREATE TRIGGER IF NOT EXISTS events_no_update
BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events is append-only'); END;

CREATE TRIGGER IF NOT EXISTS events_no_delete
BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events is append-only'); END;

CREATE INDEX IF NOT EXISTS events_by_change ON events (repo, number, at_unix_ms);
";

/// A single audit event, about to be appended.
#[derive(Debug)]
pub struct EventRecord<'a> {
    /// e.g. `webhook.received`, `review.decision`, `gate.outcome`,
    /// `forge.check_run_posted`.
    pub kind: &'a str,
    pub forge: Option<&'a str>,
    pub repo: Option<&'a str>,
    pub number: Option<u64>,
    pub head_sha: Option<&'a str>,
    /// Arbitrary structured detail — stored as JSON text, verbatim.
    pub payload: &'a serde_json::Value,
}

pub struct AuditStore {
    conn: Mutex<Connection>,
}

impl AuditStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref())
            .with_context(|| format!("opening audit db at {}", path.as_ref().display()))?;
        conn.execute_batch(SCHEMA).context("applying audit schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// In-memory store, for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Append one event, returning its rowid.
    pub fn append(&self, event: &EventRecord) -> Result<i64> {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before 1970")
            .as_millis() as i64;
        let conn = self.conn.lock().expect("audit store mutex poisoned");
        conn.execute(
            "INSERT INTO events (at_unix_ms, kind, forge, repo, number, head_sha, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                at,
                event.kind,
                event.forge,
                event.repo,
                event.number,
                event.head_sha,
                event.payload.to_string(),
            ],
        )
        .context("appending audit event")?;
        Ok(conn.last_insert_rowid())
    }

    pub fn event_count(&self) -> Result<u64> {
        let conn = self.conn.lock().expect("audit store mutex poisoned");
        let n: u64 = conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        Ok(n)
    }

    /// Read events back, newest last, optionally narrowed to a repo and a
    /// PR/MR number. This is the whole query story for `sluss log`.
    pub fn events(
        &self,
        repo: Option<&str>,
        number: Option<u64>,
        limit: u64,
    ) -> Result<Vec<StoredEvent>> {
        let conn = self.conn.lock().expect("audit store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, at_unix_ms, kind, forge, repo, number, head_sha, payload FROM events
             WHERE (?1 IS NULL OR repo = ?1) AND (?2 IS NULL OR number = ?2)
             ORDER BY id DESC LIMIT ?3",
        )?;
        let mut rows: Vec<StoredEvent> = stmt
            .query_map(params![repo, number, limit], |row| {
                Ok(StoredEvent {
                    id: row.get(0)?,
                    at_unix_ms: row.get(1)?,
                    kind: row.get(2)?,
                    forge: row.get(3)?,
                    repo: row.get(4)?,
                    number: row.get(5)?,
                    head_sha: row.get(6)?,
                    payload: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        rows.reverse();
        Ok(rows)
    }
}

/// One event read back out of the log.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub id: i64,
    pub at_unix_ms: i64,
    pub kind: String,
    pub forge: Option<String>,
    pub repo: Option<String>,
    pub number: Option<u64>,
    pub head_sha: Option<String>,
    pub payload: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_count() {
        let store = AuditStore::open_in_memory().unwrap();
        let payload = serde_json::json!({"hello": "world"});
        let id = store
            .append(&EventRecord {
                kind: "webhook.received",
                forge: Some("github"),
                repo: Some("mwigge/sluss"),
                number: Some(1),
                head_sha: None,
                payload: &payload,
            })
            .unwrap();
        assert_eq!(id, 1);
        assert_eq!(store.event_count().unwrap(), 1);
    }

    #[test]
    fn updates_and_deletes_are_rejected() {
        let store = AuditStore::open_in_memory().unwrap();
        let payload = serde_json::json!({});
        store
            .append(&EventRecord {
                kind: "test",
                forge: None,
                repo: None,
                number: None,
                head_sha: None,
                payload: &payload,
            })
            .unwrap();
        let conn = store.conn.lock().unwrap();
        assert!(conn.execute("UPDATE events SET kind = 'oops'", []).is_err());
        assert!(conn.execute("DELETE FROM events", []).is_err());
    }
}
