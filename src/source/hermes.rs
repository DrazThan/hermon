//! Hermes state source (`~/.hermes/state.db`).
//!
//! Port of `hermon.py:530 HermesSource`. Hermes exposes a real turn-completion
//! signal (`finish_reason`/`tool_calls` on the last message), so sessions read
//! here feed [`crate::source::classify`] via `turn_liveness` directly rather
//! than guessing liveness from file mtimes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::render::hermes::render_hermes_row;
use crate::render::{Seg, Sem, StyledLine, clip};
use crate::source::{Replay, SessionMeta, Source, Tailer};

/// Opens `db_path` read-only, safe to use alongside the running Hermes
/// process under WAL (`hermon.py:460 hermes_connect`). Never opens writable.
fn hermes_connect(db_path: &Path) -> rusqlite::Result<Connection> {
    let uri = format!("file:{}?mode=ro", db_path.display());
    let conn = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(Duration::from_secs(2))?;
    Ok(conn)
}

fn expand_tilde(path: &str) -> PathBuf {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

/// One-line summary of the most recent message, for [`SessionMeta::last_line`].
/// A condensed, single-line cousin of `hermon.py:722 render_hermes_row` — that
/// renderer wraps and colors a whole transcript for the tailer (out of scope
/// here); this just needs enough of the last row to eyeball at a glance.
fn summarize_last_message(
    role: Option<&str>,
    content: Option<&str>,
    tool_calls: Option<&str>,
    tool_name: Option<&str>,
) -> String {
    let Some(role) = role else {
        return String::new();
    };
    match role {
        "assistant" => {
            if let Some(raw) = tool_calls
                && let Ok(serde_json::Value::Array(calls)) = serde_json::from_str(raw)
                && let Some(serde_json::Value::Object(call)) = calls.first()
            {
                let func = call.get("function").and_then(|f| f.as_object());
                let name = func
                    .and_then(|f| f.get("name"))
                    .or_else(|| call.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let args = func
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                return format!("▶ {name} {}", clip(args, 120));
            }
            content.map(|c| clip(c, 200)).unwrap_or_default()
        }
        "tool" => {
            let raw = content.unwrap_or("");
            let mut text = raw.to_string();
            let mut is_error = false;
            if let Ok(serde_json::Value::Object(obj)) = serde_json::from_str(raw) {
                let err = obj.get("error").filter(|v| !v.is_null());
                let code = obj.get("exit_code").and_then(|v| v.as_i64());
                is_error = err.is_some_and(|v| !matches!(v, serde_json::Value::Bool(false)))
                    || !matches!(code, None | Some(0));
                text = if let Some(err) = err {
                    match err {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    }
                } else {
                    obj.get("output")
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or(raw.to_string())
                };
            }
            if is_error {
                format!("◀ ERROR {}", clip(&text, 200))
            } else {
                format!("◀ {} {}", tool_name.unwrap_or("result"), clip(&text, 200))
            }
        }
        "user" => format!("» {}", clip(content.unwrap_or(""), 200)),
        "system" => "· system".to_string(),
        other => format!("· {other}"),
    }
}

/// Hermes state source. Never propagates errors: a missing or unreadable
/// database yields an empty session list plus a one-time warning.
pub struct HermesSource {
    db_path: PathBuf,
    warned: bool,
}

impl HermesSource {
    pub fn new(db_path: impl AsRef<str>) -> Self {
        HermesSource {
            db_path: expand_tilde(db_path.as_ref()),
            warned: false,
        }
    }

    fn warn(&mut self) {
        if !self.warned {
            self.warned = true;
            eprintln!("· hermes db unavailable: {}", self.db_path.display());
        }
    }
}

impl Source for HermesSource {
    fn sessions(&mut self) -> Vec<SessionMeta> {
        let conn = match hermes_connect(&self.db_path) {
            Ok(conn) => conn,
            Err(_) => {
                self.warn();
                return Vec::new();
            }
        };

        let mut stmt = match conn.prepare(
            "SELECT s.id, s.started_at, s.ended_at, s.model, s.title,
                    s.input_tokens, s.output_tokens,
                    s.cache_read_tokens, s.cache_write_tokens,
                    COALESCE(s.actual_cost_usd, s.estimated_cost_usd),
                    COALESCE((SELECT MAX(m.timestamp) FROM messages m
                              WHERE m.session_id = s.id), s.started_at),
                    (SELECT role FROM messages m WHERE m.session_id = s.id
                     ORDER BY m.id DESC LIMIT 1),
                    (SELECT finish_reason FROM messages m WHERE m.session_id = s.id
                     ORDER BY m.id DESC LIMIT 1),
                    (SELECT tool_calls FROM messages m WHERE m.session_id = s.id
                     ORDER BY m.id DESC LIMIT 1),
                    (SELECT tool_name FROM messages m WHERE m.session_id = s.id
                     ORDER BY m.id DESC LIMIT 1),
                    (SELECT content FROM messages m WHERE m.session_id = s.id
                     ORDER BY m.id DESC LIMIT 1)
             FROM sessions s
             ORDER BY s.started_at DESC",
        ) {
            Ok(stmt) => stmt,
            Err(_) => {
                self.warn();
                return Vec::new();
            }
        };

        let rows = stmt.query_map([], |row| {
            let last_role: Option<String> = row.get(11)?;
            let last_finish: Option<String> = row.get(12)?;
            let last_tool_calls: Option<String> = row.get(13)?;
            let last_tool_name: Option<String> = row.get(14)?;
            let last_content: Option<String> = row.get(15)?;
            let last_has_tc = last_tool_calls.is_some();
            let turn_done = last_role.as_deref() == Some("assistant")
                && last_finish.as_deref() == Some("stop")
                && !last_has_tc;
            let tool_pending =
                last_role.as_deref() == Some("assistant") && last_has_tc && !turn_done;
            let started_at: f64 = row.get(1)?;
            let ended_at: Option<f64> = row.get(2)?;
            let last_line = summarize_last_message(
                last_role.as_deref(),
                last_content.as_deref(),
                last_tool_calls.as_deref(),
                last_tool_name.as_deref(),
            );
            Ok(SessionMeta {
                id: row.get(0)?,
                started_at,
                ended: ended_at.is_some(),
                model: row
                    .get::<_, Option<String>>(3)?
                    .unwrap_or_else(|| "?".to_string()),
                title: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                in_tok: (row.get::<_, Option<i64>>(5)?.unwrap_or(0)
                    + row.get::<_, Option<i64>>(7)?.unwrap_or(0)
                    + row.get::<_, Option<i64>>(8)?.unwrap_or(0)) as u64,
                out_tok: row.get::<_, Option<i64>>(6)?.unwrap_or(0) as u64,
                cost: row.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
                last_ts: row.get(10)?,
                turn_done,
                tool_pending,
                force_live: false,
                last_tool: "-".to_string(),
                last_line,
                last_event: None,
            })
        });

        let rows = match rows {
            Ok(rows) => rows,
            Err(_) => {
                self.warn();
                return Vec::new();
            }
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn last_tool(&mut self, session_id: &str) -> String {
        let conn = match hermes_connect(&self.db_path) {
            Ok(conn) => conn,
            Err(_) => return "-".to_string(),
        };
        conn.query_row(
            "SELECT tool_name FROM messages WHERE session_id = ?1
             AND tool_name IS NOT NULL ORDER BY id DESC LIMIT 1",
            [session_id],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "-".to_string())
    }

    fn open_tailer(&self, session_id: &str, replay: Replay) -> Option<Box<dyn Tailer>> {
        Some(Box::new(HermesTailer::new(
            self.db_path.clone(),
            session_id.to_string(),
            replay.rows,
        )))
    }
}

/// Snapshot of the `sessions` stats columns a poll last emitted, so
/// unchanged stats don't reprint every ~2s tick (`hermon.py:821 last_stats`).
#[derive(Debug, Clone, PartialEq)]
struct StatsSnapshot {
    model: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    cost: Option<f64>,
}

/// Live tail of one Hermes session's `messages` table
/// (`hermon.py:777 cmd_render_hermes`). The table is append-only, so tailing
/// is just "rows with a bigger id than last time" — no rotation/truncation
/// handling needed, unlike the Claude transcript files.
pub struct HermesTailer {
    db_path: PathBuf,
    session_id: String,
    replay_rows: u32,
    /// `None` until the first poll seeds it from the replay window.
    last_id: Option<i64>,
    /// Counts polls; the stats line is checked every 7th one (`hermon.py`'s
    /// `tick % 4 == 1` at a 0.5s loop period, rescaled to this crate's 300ms
    /// [`crate::engine::PANE_TICK`] so it still fires roughly every 2s).
    tick: u32,
    last_stats: Option<StatsSnapshot>,
    /// Set once `ended_at` is observed; from then on `poll` returns nothing
    /// forever (`hermon.py:832` returns from the tail loop entirely).
    ended: bool,
    warned_unavailable: bool,
    warned_read_error: bool,
}

fn dim_line(text: &str) -> StyledLine {
    StyledLine(vec![Seg::new(Sem::Dim, text)])
}

impl HermesTailer {
    pub fn new(db_path: PathBuf, session_id: String, replay_rows: u32) -> Self {
        HermesTailer {
            db_path,
            session_id,
            replay_rows,
            last_id: None,
            tick: 0,
            last_stats: None,
            ended: false,
            warned_unavailable: false,
            warned_read_error: false,
        }
    }

    /// The actual read-and-render work, run against a freshly opened
    /// connection; `?` on any sqlite error hands control back to
    /// [`Tailer::poll`] to fold into the "read error — retrying" line.
    fn poll_inner(&mut self, conn: &Connection) -> rusqlite::Result<Vec<StyledLine>> {
        let mut lines = Vec::new();

        if self.last_id.is_none() {
            let replay = self.replay_rows.max(1);
            let seed: Option<i64> = conn.query_row(
                "SELECT MIN(id) FROM (SELECT id FROM messages WHERE session_id = ?1
                 ORDER BY id DESC LIMIT ?2)",
                rusqlite::params![self.session_id, replay],
                |row| row.get(0),
            )?;
            self.last_id = Some(seed.map(|id| id - 1).unwrap_or(0));
        }
        let last_id = self.last_id.unwrap_or(0);

        let mut new_last_id = last_id;
        {
            let mut stmt = conn.prepare(
                "SELECT id, role, content, tool_calls, tool_name FROM messages
                 WHERE session_id = ?1 AND id > ?2 ORDER BY id LIMIT 500",
            )?;
            let rows = stmt.query_map(rusqlite::params![self.session_id, last_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?;
            for row in rows {
                let (id, role, content, tool_calls, tool_name) = row?;
                new_last_id = id;
                lines.extend(render_hermes_row(
                    role.as_deref(),
                    content.as_deref(),
                    tool_calls.as_deref(),
                    tool_name.as_deref(),
                ));
            }
        }
        self.last_id = Some(new_last_id);

        self.tick = self.tick.wrapping_add(1);
        if self.tick % 7 == 1 {
            let srow = conn
                .query_row(
                    "SELECT model, input_tokens, output_tokens, cache_read_tokens,
                            cache_write_tokens,
                            COALESCE(actual_cost_usd, estimated_cost_usd), ended_at
                     FROM sessions WHERE id = ?1",
                    [&self.session_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                            row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                            row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                            row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                            row.get::<_, Option<f64>>(5)?,
                            row.get::<_, Option<f64>>(6)?,
                        ))
                    },
                )
                .optional()?;

            if let Some((model, in_tok, out_tok, cache_read, cache_write, cost, ended_at)) = srow {
                let any_truthy = in_tok != 0
                    || out_tok != 0
                    || cache_read != 0
                    || cache_write != 0
                    || cost.is_some_and(|c| c != 0.0);
                let snapshot = StatsSnapshot {
                    model: model.clone(),
                    input_tokens: in_tok,
                    output_tokens: out_tok,
                    cache_read_tokens: cache_read,
                    cache_write_tokens: cache_write,
                    cost,
                };
                if any_truthy && self.last_stats.as_ref() != Some(&snapshot) {
                    self.last_stats = Some(snapshot);
                    let total_in = in_tok + cache_read + cache_write;
                    let mut text = format!(
                        "Σ in:{} out:{}",
                        fmt_thousands(total_in),
                        fmt_thousands(out_tok)
                    );
                    if let Some(cost) = cost {
                        text.push_str(&format!(" ${cost:.4}"));
                    }
                    text.push_str(&format!("  [{}]", model.as_deref().unwrap_or("?")));
                    lines.push(StyledLine(vec![Seg::new(Sem::Stat, text)]));
                }
                if ended_at.is_some() {
                    lines.push(StyledLine(vec![Seg::new(Sem::Ok, "Σ session ended")]));
                    self.ended = true;
                }
            }
        }

        Ok(lines)
    }
}

/// `1234567` -> `"1,234,567"` (`hermon.py:825`'s `f"{n:,}"`).
fn fmt_thousands(n: i64) -> String {
    let sign = if n < 0 { "-" } else { "" };
    let digits = n.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, ch) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{sign}{}", grouped.chars().rev().collect::<String>())
}

impl Tailer for HermesTailer {
    fn poll(&mut self) -> Vec<StyledLine> {
        if self.ended {
            return Vec::new();
        }

        let conn = match hermes_connect(&self.db_path) {
            Ok(conn) => conn,
            Err(_) => {
                return if self.warned_unavailable {
                    Vec::new()
                } else {
                    self.warned_unavailable = true;
                    vec![dim_line("· hermes db unavailable — waiting")]
                };
            }
        };
        self.warned_unavailable = false;

        match self.poll_inner(&conn) {
            Ok(lines) => {
                self.warned_read_error = false;
                lines
            }
            Err(_) => {
                if self.warned_read_error {
                    Vec::new()
                } else {
                    self.warned_read_error = true;
                    vec![dim_line("· hermes db read error — retrying")]
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_joins_home() {
        let home = dirs::home_dir().unwrap_or_default();
        assert_eq!(
            expand_tilde("~/.hermes/state.db"),
            home.join(".hermes/state.db")
        );
        assert_eq!(expand_tilde("/abs/path.db"), PathBuf::from("/abs/path.db"));
    }

    #[test]
    fn summarize_assistant_tool_call() {
        let calls = r#"[{"id":"toolu_x","type":"function","function":{"name":"terminal","arguments":"ls -la"}}]"#;
        let line = summarize_last_message(Some("assistant"), None, Some(calls), None);
        assert_eq!(line, "▶ terminal ls -la");
    }

    #[test]
    fn summarize_tool_error_via_exit_code() {
        let content = r#"{"output":"boom","exit_code":1,"error":null}"#;
        let line = summarize_last_message(Some("tool"), Some(content), None, Some("terminal"));
        assert!(line.starts_with("◀ ERROR"), "{line}");
        assert!(line.contains("boom"));
    }

    #[test]
    fn summarize_tool_success() {
        let content = r#"{"output":"OK","exit_code":0,"error":null}"#;
        let line = summarize_last_message(Some("tool"), Some(content), None, Some("terminal"));
        assert_eq!(line, "◀ terminal OK");
    }

    #[test]
    fn summarize_user_and_missing() {
        assert_eq!(
            summarize_last_message(Some("user"), Some("hi"), None, None),
            "» hi"
        );
        assert_eq!(summarize_last_message(None, None, None, None), "");
    }
}
