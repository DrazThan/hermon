//! OpenCode state source (`~/.local/share/opencode/opencode.db`).
//!
//! OpenCode CLI sessions, from its own `opencode.db` (SQLite, WAL).
//! OpenCode's `message.data.finish` is 'tool-calls' / 'stop' — the exact
//! same signal as Hermes's `finish_reason`, so it plugs into the same
//! turn-liveness logic via the shared [`SessionMeta`] shape. Two schema
//! quirks bite: timestamps are epoch **milliseconds**, and turn state
//! lives inside a JSON blob.
//!
//! Port of `hermon.py` `OpenCodeSource` (hermon.py:615).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::{Replay, SessionMeta, Tailer};

pub struct OpenCodeSource {
    db_path: PathBuf,
    warned: bool,
}

impl OpenCodeSource {
    pub fn new(db_path: impl AsRef<Path>) -> Self {
        Self {
            db_path: expand_user(db_path.as_ref()),
            warned: false,
        }
    }

    /// Read-only connection, never writable — safe alongside the running
    /// tool under WAL (port of `opencode_connect`, hermon.py:612).
    fn connect(&self) -> rusqlite::Result<Connection> {
        let conn = Connection::open_with_flags(
            &self.db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?;
        conn.busy_timeout(Duration::from_secs(2))?;
        Ok(conn)
    }

    /// Recent/unfinished session rows; empty on any error, never an error.
    ///
    /// `since` is epoch seconds; OpenCode stores epoch milliseconds, so
    /// the bound is scaled up on the way in and row times scaled down on
    /// the way out.
    pub fn sessions(&mut self, since: f64) -> Vec<SessionMeta> {
        match self.try_sessions(since) {
            Ok(rows) => rows,
            Err(_) => {
                self.warn();
                Vec::new()
            }
        }
    }

    fn try_sessions(&self, since: f64) -> rusqlite::Result<Vec<SessionMeta>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT s.id, s.title, s.model, s.cost,
                    s.tokens_input, s.tokens_output,
                    s.tokens_cache_read, s.tokens_cache_write,
                    s.time_created, s.time_updated, s.time_archived,
                    (SELECT data FROM message m WHERE m.session_id = s.id
                     ORDER BY m.rowid DESC LIMIT 1)
             FROM session s
             WHERE s.time_created >= ?1 OR s.time_archived IS NULL",
        )?;
        let rows = stmt.query_map([since * 1000.0], |row| {
            let last_msg: Option<String> = row.get(11)?;
            let (role, finish) = role_finish(last_msg.as_deref());
            let role = role.as_deref();
            let finish = finish.as_deref();
            let created = nonzero(row.get(8)?);
            let updated = nonzero(row.get(9)?);
            Ok(SessionMeta {
                id: row.get(0)?,
                started_at: created.unwrap_or(0.0) / 1000.0,
                ended: row.get::<_, Option<i64>>(10)?.is_some(),
                model: model_id(row.get::<_, Option<String>>(2)?.as_deref()),
                title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                in_tok: tok(row.get(4)?) + tok(row.get(6)?) + tok(row.get(7)?),
                out_tok: tok(row.get(5)?),
                cost: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                last_ts: updated.or(created).unwrap_or(0.0) / 1000.0,
                turn_done: role == Some("assistant") && finish == Some("stop"),
                tool_pending: role == Some("assistant") && finish == Some("tool-calls"),
                last_tool: "-".to_string(),
                last_line: last_line(role, finish),
                last_event: None,
            })
        })?;
        rows.collect()
    }

    /// Name of the most recent tool-type part; a bounded scan since parts
    /// interleave text/reasoning/tool rows (no JSON1 dependency). "-" on
    /// any error or when none of the last 20 parts is a tool call.
    pub fn last_tool(&self, session_id: &str) -> String {
        self.try_last_tool(session_id)
            .unwrap_or_else(|_| "-".to_string())
    }

    fn try_last_tool(&self, session_id: &str) -> rusqlite::Result<String> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare("SELECT data FROM part WHERE session_id = ?1 ORDER BY rowid DESC LIMIT 20")?;
        let rows = stmt.query_map([session_id], |row| row.get::<_, String>(0))?;
        for data in rows {
            let Ok(v) = serde_json::from_str::<Value>(&data?) else {
                continue;
            };
            if v.get("type").and_then(Value::as_str) == Some("tool") {
                let tool = v.get("tool").and_then(Value::as_str).unwrap_or("-");
                return Ok(tool.to_string());
            }
        }
        Ok("-".to_string())
    }

    /// Inherent twin of [`Source::open_tailer`](super::Source::open_tailer)
    /// — this source is used by concrete type (its `sessions` takes a
    /// `since` bound the trait has no room for), so the trait's default
    /// never applies to it. `None` until the OpenCode part tailer lands.
    pub fn open_tailer(&self, _session_id: &str, _replay: Replay) -> Option<Box<dyn Tailer>> {
        None
    }

    fn warn(&mut self) {
        if !self.warned {
            self.warned = true;
            eprintln!("· opencode db unavailable: {}", self.db_path.display());
        }
    }
}

fn expand_user(p: &Path) -> PathBuf {
    if let Ok(rest) = p.strip_prefix("~")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    p.to_path_buf()
}

/// Python `or` semantics: a zero timestamp falls through to the fallback.
fn nonzero(v: Option<f64>) -> Option<f64> {
    v.filter(|&x| x != 0.0)
}

fn tok(v: Option<i64>) -> u64 {
    v.unwrap_or(0).max(0) as u64
}

fn model_id(raw: Option<&str>) -> String {
    raw.and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "?".to_string())
}

fn role_finish(data: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(v) = data.and_then(|s| serde_json::from_str::<Value>(s).ok()) else {
        return (None, None);
    };
    (
        v.get("role").and_then(Value::as_str).map(str::to_string),
        v.get("finish").and_then(Value::as_str).map(str::to_string),
    )
}

/// One-line summary of the most recent message. OpenCode's `message.data`
/// blob carries turn state, not text (the text lives in `part` rows), so
/// the summary is the turn state itself, e.g. "assistant · tool-calls".
fn last_line(role: Option<&str>, finish: Option<&str>) -> String {
    match (role, finish) {
        (Some(r), Some(f)) => format!("{r} · {f}"),
        (Some(r), None) => r.to_string(),
        _ => String::new(),
    }
}
