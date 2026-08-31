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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;

use crate::render::opencode::{PartStatus, render_opencode_part};
use crate::render::{Seg, Sem, StyledLine};
use crate::roster::commas;

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
        connect(&self.db_path)
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
                cost: row.get::<_, Option<f64>>(3)?,
                last_ts: updated.or(created).unwrap_or(0.0) / 1000.0,
                turn_done: role == Some("assistant") && finish == Some("stop"),
                tool_pending: role == Some("assistant") && finish == Some("tool-calls"),
                force_live: false,
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
    /// never applies to it.
    ///
    /// Always `Some`: the tailer opens its own connection on every poll and
    /// waits out a database that is missing or locked, so refusing here
    /// would turn a transient hiccup into a permanently dead pane.
    pub fn open_tailer(&self, session_id: &str, replay: Replay) -> Option<Box<dyn Tailer>> {
        Some(Box::new(OpenCodeTailer::new(
            &self.db_path,
            session_id,
            replay.rows,
        )))
    }

    fn warn(&mut self) {
        if !self.warned {
            self.warned = true;
            eprintln!("· opencode db unavailable: {}", self.db_path.display());
        }
    }
}

/// Read-only connection to an opencode.db, shared by the source and its
/// tailer (port of `opencode_connect`, hermon.py:612).
fn connect(db_path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(Duration::from_secs(2))?;
    Ok(conn)
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

/// A dim status line the tailer has already shown; re-emitted only when the
/// condition changes, so a store that stays missing does not repaint the
/// pane every 300ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Notice {
    Unavailable,
    ReadError,
}

/// The session counters last printed, so an unchanged `Σ` line is not
/// repeated (`hermon.py:975`).
#[derive(Debug, Clone, PartialEq)]
struct Stats {
    model: Option<String>,
    cost: Option<f64>,
    in_tok: i64,
    out_tok: i64,
    cache_read: i64,
    cache_write: i64,
    archived: bool,
}

/// Live tail of one OpenCode session's `part` rows (port of
/// `hermon.py:917 cmd_render_opencode`, as a polled value instead of a
/// `while True` loop).
///
/// The cursor is `part.time_updated`, not rowid: OpenCode rewrites a tool
/// part in place as it finishes, so a rowid cursor would sail past the very
/// transition the pane exists to show. The cost of that choice is that an
/// old row resurfaces whenever it is touched, which is why the tailer also
/// owns a `part.id → `[`PartStatus`] map — the renderer uses it to tell a
/// real status change from a re-read and stays silent for the latter.
///
/// All times here stay in the epoch **milliseconds** the schema stores;
/// unlike [`OpenCodeSource::sessions`] the watermark never leaves the
/// tailer, so there is nothing to scale into seconds.
pub struct OpenCodeTailer {
    db_path: PathBuf,
    session_id: String,
    replay_rows: u32,
    /// Highest `time_updated` consumed, in ms. `None` until the first poll
    /// seeds it from the replay window.
    watermark: Option<i64>,
    statuses: HashMap<String, PartStatus>,
    tick: u64,
    last_stats: Option<Stats>,
    last_notice: Option<Notice>,
    /// The session was archived and the closing line has been printed;
    /// every later poll is silent.
    closed: bool,
}

impl OpenCodeTailer {
    fn new(db_path: &Path, session_id: &str, replay_rows: u32) -> Self {
        OpenCodeTailer {
            db_path: db_path.to_path_buf(),
            session_id: session_id.to_string(),
            replay_rows,
            watermark: None,
            statuses: HashMap::new(),
            tick: 0,
            last_stats: None,
            last_notice: None,
            closed: false,
        }
    }

    /// Replays the newest `replay_rows` parts by starting just below the
    /// oldest of them (`hermon.py:938`). No parts at all means start from
    /// zero, so the first row ever written still shows.
    fn seed_watermark(&self, conn: &Connection) -> rusqlite::Result<i64> {
        let oldest: Option<i64> = conn.query_row(
            "SELECT MIN(time_updated) FROM (SELECT time_updated FROM part
             WHERE session_id = ?1 ORDER BY rowid DESC LIMIT ?2)",
            params![self.session_id, self.replay_rows.max(1)],
            |row| row.get(0),
        )?;
        Ok(oldest.map_or(0, |t| t - 1))
    }

    fn read_parts(&mut self, conn: &Connection) -> rusqlite::Result<Vec<StyledLine>> {
        let watermark = match self.watermark {
            Some(w) => w,
            None => {
                let seeded = self.seed_watermark(conn)?;
                self.watermark = Some(seeded);
                seeded
            }
        };

        let mut stmt = conn.prepare(
            "SELECT part.id, part.time_updated, part.data, message.data
             FROM part JOIN message ON message.id = part.message_id
             WHERE part.session_id = ?1 AND part.time_updated > ?2
             ORDER BY part.time_updated, part.rowid LIMIT 1000",
        )?;
        let rows = stmt
            .query_map(params![self.session_id, watermark], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut out = Vec::new();
        for (part_id, updated, part_data, msg_data) in rows {
            self.watermark = Some(self.watermark.unwrap_or(updated).max(updated));
            let (role, _) = role_finish(msg_data.as_deref());
            let (lines, status) =
                render_opencode_part(role.as_deref(), &part_data, self.statuses.get(&part_id));
            match status {
                Some(status) => {
                    self.statuses.insert(part_id, status);
                }
                None => {
                    self.statuses.remove(&part_id);
                }
            }
            out.extend(lines);
        }
        Ok(out)
    }

    /// The periodic `Σ` counters, plus the one-shot archived line. Runs on
    /// roughly every seventh poll — at [`crate::engine::PANE_TICK`] that is
    /// the ~2s cadence of the Python loop (`hermon.py:972`).
    fn read_stats(&mut self, conn: &Connection) -> rusqlite::Result<Vec<StyledLine>> {
        self.tick += 1;
        if self.tick % 7 != 1 {
            return Ok(Vec::new());
        }
        let stats = conn
            .query_row(
                "SELECT model, cost, tokens_input, tokens_output,
                        tokens_cache_read, tokens_cache_write, time_archived
                 FROM session WHERE id = ?1",
                params![self.session_id],
                |row| {
                    Ok(Stats {
                        model: row.get(0)?,
                        cost: row.get(1)?,
                        in_tok: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        out_tok: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        cache_read: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        cache_write: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                        archived: row.get::<_, Option<i64>>(6)?.is_some(),
                    })
                },
            )
            .optional()?;
        let Some(stats) = stats else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        let counted = stats.cost.unwrap_or(0.0) != 0.0
            || [
                stats.in_tok,
                stats.out_tok,
                stats.cache_read,
                stats.cache_write,
            ]
            .iter()
            .any(|&v| v != 0);
        if counted && self.last_stats.as_ref() != Some(&stats) {
            out.push(stats_line(&stats));
            self.last_stats = Some(stats.clone());
        }
        if stats.archived {
            out.push(StyledLine(vec![Seg::new(Sem::Ok, "Σ session archived")]));
            self.closed = true;
        }
        Ok(out)
    }

    /// New part lines, then the periodic counters — the order the Python
    /// prints them in.
    fn read(&mut self, conn: &Connection) -> rusqlite::Result<Vec<StyledLine>> {
        let mut lines = self.read_parts(conn)?;
        lines.extend(self.read_stats(conn)?);
        Ok(lines)
    }

    fn notice(&mut self, kind: Notice, text: &str) -> Vec<StyledLine> {
        if self.last_notice == Some(kind) {
            return Vec::new();
        }
        self.last_notice = Some(kind);
        vec![StyledLine(vec![Seg::new(Sem::Dim, text)])]
    }
}

impl Tailer for OpenCodeTailer {
    fn poll(&mut self) -> Vec<StyledLine> {
        if self.closed {
            return Vec::new();
        }
        let conn = match connect(&self.db_path) {
            Ok(conn) => conn,
            Err(_) => {
                return self.notice(Notice::Unavailable, "· opencode db unavailable — waiting");
            }
        };
        match self.read(&conn) {
            Ok(lines) => {
                self.last_notice = None;
                lines
            }
            Err(_) => self.notice(Notice::ReadError, "· opencode db read error — retrying"),
        }
    }
}

/// `Σ in:1,234 out:56 $0.0500  [claude-sonnet-5]` (`hermon.py:981`).
fn stats_line(stats: &Stats) -> StyledLine {
    let in_tok = (stats.in_tok + stats.cache_read + stats.cache_write).max(0) as u64;
    let mut line = format!(
        "Σ in:{} out:{}",
        commas(in_tok),
        commas(stats.out_tok.max(0) as u64)
    );
    if let Some(cost) = stats.cost {
        line.push_str(&format!(" ${cost:.4}"));
    }
    line.push_str(&format!("  [{}]", model_id(stats.model.as_deref())));
    StyledLine(vec![Seg::new(Sem::Stat, line)])
}
