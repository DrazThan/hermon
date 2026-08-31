//! Claude Code transcript source (`~/.claude/projects/**/*.jsonl`).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::render::claude::render_claude_line;
use crate::render::{Seg, Sem, StyledLine, clip, parse_ts};
use crate::source::{LastEvent, Replay, SessionMeta, Tailer};

/// Tool arguments are clipped at 120 chars, tool results at 200
/// (`hermon.py:212`, `hermon.py:243`). Shared with [`crate::render::claude`],
/// which clips the same fields for the full-line renderer.
pub(crate) const ARG_CLIP: usize = 120;
pub(crate) const RESULT_CLIP: usize = 200;

/// Incremental per-transcript accumulator for the roster
/// (`hermon.py:327 ClaudeStats`).
///
/// [`update`](ClaudeStats::update) is called once per roster tick and parses
/// only the bytes appended since the last one. Two invariants make that safe:
/// [`offset`](Self::offset) advances only past `\n`-terminated lines, so a
/// half-written line is re-read next tick rather than parsed as truncated
/// JSON; and it advances by the *raw byte* length, so a line carrying
/// multibyte UTF-8 leaves the offset exactly at the next line's first byte.
/// (`hermon.py:366` re-encodes the already-decoded line to guess that length,
/// which drifts whenever decoding replaced an invalid byte.)
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeStats {
    path: PathBuf,
    offset: u64,
    pub model: String,
    pub in_tok: u64,
    pub out_tok: u64,
    /// Summed per-message costs, used by older transcripts that predate the
    /// running `result` total.
    cost_sum: f64,
    /// Running total from `result` events; authoritative when present.
    cost_reported: Option<f64>,
    pub last_tool: String,
    pub first_ts: Option<f64>,
    pub last_ts: Option<f64>,
    /// Shape of the most recent conversational event, which is what tells
    /// [`classify`](crate::source::classify) a silent session is sitting on a
    /// permission prompt rather than working.
    pub last_event: Option<LastEvent>,
    /// One-line summary of that event for the roster's list mode.
    pub last_line: String,
}

impl ClaudeStats {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        ClaudeStats {
            path: path.into(),
            offset: 0,
            model: "?".to_string(),
            in_tok: 0,
            out_tok: 0,
            cost_sum: 0.0,
            cost_reported: None,
            last_tool: "-".to_string(),
            first_ts: None,
            last_ts: None,
            last_event: None,
            last_line: String::new(),
        }
    }

    /// The `result` running total when the transcript reports one, else the
    /// summed per-message costs (`hermon.py:343`). Returns `None` if neither
    /// a result event nor any per-message costs have been seen.
    pub fn cost(&self) -> Option<f64> {
        if let Some(reported) = self.cost_reported {
            Some(reported)
        } else if self.cost_sum > 0.0 {
            Some(self.cost_sum)
        } else {
            None
        }
    }

    pub fn elapsed(&self) -> Option<f64> {
        Some(self.last_ts? - self.first_ts?)
    }

    /// Parse whatever has been appended since the last call.
    ///
    /// I/O errors are swallowed, as in Python: a transcript that is missing,
    /// unreadable or mid-rotation is a normal state for a polling roster, and
    /// the next tick retries from the same offset.
    pub fn update(&mut self) {
        let Ok(meta) = fs::metadata(&self.path) else {
            return;
        };
        let size = meta.len();
        if size < self.offset {
            // Truncated or replaced: reparse from scratch.
            *self = ClaudeStats::new(self.path.clone());
        }
        if size == self.offset {
            return;
        }
        let Ok(file) = File::open(&self.path) else {
            return;
        };
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut raw = Vec::new();
        loop {
            raw.clear();
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    if !raw.ends_with(b"\n") {
                        return; // partial trailing line; re-read next tick
                    }
                    self.offset += raw.len() as u64;
                    self.ingest(&String::from_utf8_lossy(&raw));
                }
            }
        }
    }

    fn ingest(&mut self, raw: &str) {
        let Ok(Value::Object(ev)) = serde_json::from_str::<Value>(raw) else {
            return;
        };
        if let Some(ts) = ev
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| parse_ts(s).ok())
        {
            self.first_ts.get_or_insert(ts);
            self.last_ts = Some(ts);
        }

        let msg = ev.get("message").and_then(Value::as_object);
        if let Some(model) = msg.and_then(|m| m.get("model")).and_then(Value::as_str) {
            self.model = model.to_string();
        }
        if let Some(usage) = msg.and_then(|m| m.get("usage")).and_then(Value::as_object) {
            self.in_tok += usage_in(usage);
            self.out_tok += count(usage.get("output_tokens"));
        }

        let etype = ev.get("type").and_then(Value::as_str);
        let role = msg.and_then(|m| m.get("role")).and_then(Value::as_str);
        let is_user = etype == Some("user") || role == Some("user");
        match msg.and_then(|m| m.get("content")) {
            Some(Value::String(text)) if is_user => self.saw_user_text(text),
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    if let Some(block) = block.as_object() {
                        self.saw_block(block, is_user);
                    }
                }
            }
            _ => {}
        }

        if let Some(cost) = event_cost(&ev) {
            if etype == Some("result") {
                self.cost_reported = Some(cost);
            } else {
                self.cost_sum += cost;
            }
        }
    }

    /// Records one content block. Unrecognised block types (`thinking`,
    /// `image`, …) leave `last_event` and `last_line` untouched, so the last
    /// *meaningful* event survives a trailing block hermon does not render.
    fn saw_block(&mut self, block: &Map<String, Value>, is_user: bool) {
        let text = |key: &str| block.get(key).and_then(Value::as_str).unwrap_or("");
        match block.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("?");
                let input = block
                    .get("input")
                    .map(ToString::to_string)
                    .unwrap_or_default();
                self.last_tool = name.to_string();
                self.last_event = Some(LastEvent::ToolUse(name.to_string()));
                self.last_line = format!("▶ {name} {}", clip(&input, ARG_CLIP));
            }
            Some("tool_result") => {
                let body = clip(&result_text(block.get("content")), RESULT_CLIP);
                let failed = block.get("is_error").and_then(Value::as_bool) == Some(true);
                self.last_event = Some(LastEvent::ToolResult);
                self.last_line = if failed {
                    format!("◀ ERROR {body}")
                } else {
                    format!("◀ result {body}")
                };
            }
            Some("text") if is_user => self.saw_user_text(text("text")),
            Some("text") => {
                let body = clip(text("text"), ARG_CLIP);
                if !body.is_empty() {
                    self.last_event = Some(LastEvent::AssistantText);
                    self.last_line = body;
                }
            }
            _ => {}
        }
    }

    fn saw_user_text(&mut self, text: &str) {
        let body = clip(text, ARG_CLIP);
        if !body.is_empty() {
            self.last_event = Some(LastEvent::User);
            self.last_line = format!("» {body}");
        }
    }
}

/// A JSON number as a token count, truncating floats like Python's `int()`;
/// anything else counts as zero (`hermon.py:166`).
pub(crate) fn count(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_f64).map_or(0, |n| n as u64)
}

/// Input tokens plus both cache legs (`hermon.py:166 _usage_in`).
pub(crate) fn usage_in(usage: &Map<String, Value>) -> u64 {
    [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .iter()
    .map(|k| count(usage.get(*k)))
    .sum()
}

/// The first cost field the event carries, oldest transcripts last
/// (`hermon.py:175 _event_cost`).
pub(crate) fn event_cost(ev: &Map<String, Value>) -> Option<f64> {
    ["total_cost_usd", "cost_usd", "costUSD"]
        .iter()
        .find_map(|k| ev.get(*k).and_then(Value::as_f64))
}

/// `tool_result` content is a plain string or a list of blocks
/// (`hermon.py:155 _result_text`).
pub(crate) fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .map(|b| b.get("text").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>()
            .join(" "),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Transcripts idle longer than this are not worth statting every tick —
/// `~/.claude/projects` accumulates every session ever run, and nothing
/// this old is a live session worth tracking.
const RECENCY_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// A stale transcript this many multiples of `idle_timeout` past its mtime
/// is not worth an `lsof` call — port of the `now - mtime <= idle_timeout *
/// 5` leg of `hermon.py:447`'s cost bound.
const LSOF_STALE_MULT: f64 = 5.0;

/// True if any descriptor line from `lsof -F a` output reports write (`w`)
/// or update (`u`) access. Read-only (`r`) handles are deliberately
/// excluded: hermon's own tailers hold transcripts open read-only, and
/// counting those would pin every session live forever
/// (`hermon.py:127 has_open_handle`).
fn parse_write_access(stdout: &str) -> bool {
    stdout
        .lines()
        .any(|line| line.starts_with('a') && (line.contains('w') || line.contains('u')))
}

/// Detects `lsof` on `PATH` exactly once per process, warning to stderr the
/// first time it's found missing (`hermon.py:1314`).
fn lsof_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let available = Command::new("lsof").arg("-v").output().is_ok();
        if !available {
            eprintln!("hermon: lsof not found — mtime-only liveness for claude transcripts");
        }
        available
    })
}

/// A write-handle check, real or (in tests) a call-counting stub.
type WriteHandleProbe = Rc<dyn Fn(&Path) -> Option<bool>>;

/// True if some process holds `path` open for writing, else false; `None`
/// when `lsof` is unavailable (`hermon.py:127 has_open_handle`).
fn has_open_write_handle(path: &Path) -> Option<bool> {
    if !lsof_available() {
        return None;
    }
    let output = Command::new("lsof")
        .args(["-F", "a", "--"])
        .arg(path)
        .output()
        .ok()?;
    Some(parse_write_access(&String::from_utf8_lossy(&output.stdout)))
}

/// Discovers Claude Code sessions by walking transcript files on disk
/// (`hermon.py:424 scan_claude_root`, `hermon.py:431 ClaudeSource`).
/// Claude has no session database, so each `*.jsonl` file under the root
/// *is* a session, and its [`ClaudeStats`] accumulator is the session's
/// state.
pub struct ClaudeSource {
    root: PathBuf,
    /// Keyed by transcript file stem (the session id), so it doubles as
    /// the `last_tool` lookup index. Kept across calls to `sessions()` so
    /// each transcript is re-parsed only from its last byte offset.
    ///
    /// It also stands in for "already tracked" in the `lsof` cost bound
    /// (`hermon.py:447`): Python's `tracked_keys` is the watcher's live
    /// tmux-pane set, but this port has no pane manager yet, so a session
    /// this map already holds an entry for is the closest available
    /// analogue — one hermon has already started following, as opposed to
    /// one just now noticed deep in `RECENCY_WINDOW`.
    stats: HashMap<String, ClaudeStats>,
    /// Swappable so tests can inject a call counter instead of shelling out
    /// to the real `lsof` (`has_open_write_handle` by default).
    probe: WriteHandleProbe,
}

impl ClaudeSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ClaudeSource {
            root: root.into(),
            stats: HashMap::new(),
            probe: Rc::new(has_open_write_handle),
        }
    }

    /// One [`SessionMeta`] per transcript modified within
    /// [`RECENCY_WINDOW`]. Claude carries no turn-completion signal, so
    /// `turn_done`, `tool_pending` and `ended` are always false — liveness
    /// is keyed off `last_ts`/mtime instead (`hermon.py:431`), with
    /// [`SessionMeta::force_live`] set when a stale transcript still has an
    /// open `lsof` write handle (`hermon.py:447`).
    pub fn sessions(&mut self, now: f64, idle_timeout: f64) -> Vec<SessionMeta> {
        scan_jsonl_files(&self.root, RECENCY_WINDOW)
            .into_iter()
            .filter_map(|path| self.session_for(path, now, idle_timeout))
            .collect()
    }

    fn session_for(&mut self, path: PathBuf, now: f64, idle_timeout: f64) -> Option<SessionMeta> {
        let id = path.file_stem()?.to_str()?.to_string();
        let mtime = mtime_secs(&path);
        let already_tracked = self.stats.contains_key(&id);
        let stats = self
            .stats
            .entry(id.clone())
            .or_insert_with(|| ClaudeStats::new(path.clone()));
        stats.update();
        // File mtime is a floor, not a substitute: a transcript recently
        // appended with an untimestamped tail event (e.g. a tool result
        // with no `timestamp` field) must not read as stale just because
        // the last *timestamped* event is old (`hermon.py:431`, which
        // keys Claude recency off mtime alone). Clock skew can still push
        // the event timestamp ahead of mtime, so take the max rather than
        // preferring either source outright.
        let last_ts = match (stats.last_ts, mtime) {
            (Some(ts), Some(mt)) => ts.max(mt),
            (Some(ts), None) => ts,
            (None, Some(mt)) => mt,
            (None, None) => 0.0,
        };
        // A fresh transcript is already live via the plain `last_ts`
        // ceiling downstream; only pay for an `lsof` call where the answer
        // could change the verdict, and only within a window where the
        // answer could still matter (`hermon.py:447`).
        let stale = now - last_ts > idle_timeout;
        let worth_checking =
            stale && (already_tracked || now - last_ts <= idle_timeout * LSOF_STALE_MULT);
        let force_live = worth_checking && (self.probe)(&path).unwrap_or(false);
        let started_at = stats.first_ts.unwrap_or(last_ts);
        Some(SessionMeta {
            id,
            started_at,
            ended: false,
            model: stats.model.clone(),
            title: String::new(),
            in_tok: stats.in_tok,
            out_tok: stats.out_tok,
            cost: stats.cost(),
            last_ts,
            turn_done: false,
            tool_pending: false,
            force_live,
            last_tool: stats.last_tool.clone(),
            last_line: stats.last_line.clone(),
            last_event: stats.last_event.clone(),
        })
    }

    /// The last tool name seen in `session_id`'s transcript
    /// (`hermon.py:352 ClaudeStats.last_tool`).
    pub fn last_tool(&mut self, session_id: &str) -> String {
        match self.stats.get_mut(session_id) {
            Some(stats) => {
                stats.update();
                stats.last_tool.clone()
            }
            None => "-".to_string(),
        }
    }

    /// Inherent twin of [`Source::open_tailer`](super::Source::open_tailer)
    /// — this source is used by its concrete type, so the trait's default
    /// never applies to it. `session_id` must already be in [`Self::stats`]
    /// (i.e. surfaced by an earlier [`Self::sessions`] call) since this
    /// takes `&self` and cannot scan for it.
    pub fn open_tailer(&self, session_id: &str, replay: Replay) -> Option<Box<dyn Tailer>> {
        let path = self.stats.get(session_id)?.path.clone();
        Some(Box::new(ClaudeTailer::new(path, replay)))
    }
}

impl Default for ClaudeSource {
    fn default() -> Self {
        let root = dirs::home_dir()
            .unwrap_or_default()
            .join(".claude")
            .join("projects");
        ClaudeSource::new(root)
    }
}

fn mtime_secs(path: &Path) -> Option<f64> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
}

/// Recursively collects `*.jsonl` files under `root` modified within
/// `window` of now, skipping `subagents/` directories, sorted for a
/// deterministic scan order. I/O errors (missing root, permission denied)
/// are swallowed and yield an empty scan, matching Python's
/// `except OSError: return []` (`hermon.py:424`).
fn scan_jsonl_files(root: &Path, window: Duration) -> Vec<PathBuf> {
    let cutoff = SystemTime::now().checked_sub(window);
    let mut out = Vec::new();
    walk_jsonl(root, cutoff, &mut out);
    out.sort();
    out
}

fn walk_jsonl(dir: &Path, cutoff: Option<SystemTime>, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            // Subagent transcripts are not top-level sessions: Python's
            // one-level `*/*.jsonl` glob never sees them, and counting them
            // inflates the roster and double-counts tokens.
            if entry.file_name() != "subagents" {
                walk_jsonl(&path, cutoff, out);
            }
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
        let fresh = match (cutoff, modified) {
            (Some(cutoff), Some(modified)) => modified >= cutoff,
            _ => true,
        };
        if fresh {
            out.push(path);
        }
    }
}

/// A live tail of one Claude transcript file (`hermon.py:268
/// cmd_render_claude`, as a value instead of a loop).
///
/// A freshly opened tailer replays up to `replay.bytes` from the end of the
/// file on its first [`poll`](Tailer::poll), discarding the partial line at
/// the seek point, then streams complete lines only. Truncation and
/// deletion are detected at end-of-file, emit one dim status line each, and
/// self-heal on the next successful open — from byte zero, since only the
/// very first open replays history.
pub struct ClaudeTailer {
    path: PathBuf,
    replay_bytes: u64,
    state: TailState,
    /// Whether the replay seek/discard still needs to happen. Cleared after
    /// the very first successful open and never set again, so a later
    /// reopen (after truncation or deletion) reads its file from byte zero.
    first_open: bool,
    /// Set once a "not found" line has been emitted, so a file that stays
    /// missing across many polls gets exactly one status line rather than
    /// one per tick.
    warned_missing: bool,
}

enum TailState {
    Closed,
    Open {
        file: File,
        /// Bytes read from `file` so far, used only to notice a shrink
        /// (truncation) by comparing against the on-disk size at EOF.
        offset: u64,
        /// Bytes read since the last complete (`\n`-terminated) line,
        /// carried across polls so a line split across two polls is not
        /// parsed until it is whole.
        partial: Vec<u8>,
    },
}

impl ClaudeTailer {
    pub fn new(path: impl Into<PathBuf>, replay: Replay) -> Self {
        ClaudeTailer {
            path: path.into(),
            replay_bytes: replay.bytes,
            state: TailState::Closed,
            first_open: true,
            warned_missing: false,
        }
    }

    /// Opens the file if closed, seeding the replay window on the very
    /// first successful open, then drains whatever is available.
    fn poll_impl(&mut self) -> Vec<StyledLine> {
        if matches!(self.state, TailState::Closed) {
            let mut file = match File::open(&self.path) {
                Ok(file) => file,
                Err(_) => {
                    if self.warned_missing {
                        return Vec::new();
                    }
                    self.warned_missing = true;
                    return vec![dim_status("· transcript not found — waiting")];
                }
            };
            self.warned_missing = false;
            if self.first_open {
                self.first_open = false;
                let size = file.metadata().map(|m| m.len()).unwrap_or(0);
                if size > self.replay_bytes {
                    let start = size - self.replay_bytes;
                    if file.seek(SeekFrom::Start(start)).is_ok() {
                        discard_one_line(&mut file);
                    }
                }
            }
            let offset = file.stream_position().unwrap_or(0);
            self.state = TailState::Open {
                file,
                offset,
                partial: Vec::new(),
            };
        }

        let TailState::Open {
            file,
            offset,
            partial,
        } = &mut self.state
        else {
            unreachable!("just ensured Open above")
        };
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    partial.extend_from_slice(&buf[..n]);
                    *offset += n as u64;
                }
                Err(_) => break,
            }
        }
        while let Some(pos) = partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = partial.drain(..=pos).collect();
            out.extend(render_claude_line(&String::from_utf8_lossy(&line)));
        }

        match fs::metadata(&self.path) {
            Ok(meta) if meta.len() < *offset => {
                out.push(dim_status("· transcript truncated — reloading"));
                self.state = TailState::Closed;
            }
            Ok(_) => {}
            Err(_) => {
                out.push(dim_status(
                    "· transcript removed — waiting for it to return",
                ));
                self.state = TailState::Closed;
                self.warned_missing = true;
            }
        }
        out
    }
}

impl Tailer for ClaudeTailer {
    fn poll(&mut self) -> Vec<StyledLine> {
        self.poll_impl()
    }
}

fn dim_status(text: &str) -> StyledLine {
    StyledLine(vec![Seg::new(Sem::Dim, text)])
}

/// Reads and discards bytes up to and including the next `\n`, or to EOF —
/// the partial line left at a replay seek point (`hermon.py:294`).
fn discard_one_line(file: &mut File) {
    let mut byte = [0u8; 1];
    loop {
        match file.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;

    /// A huge `idle_timeout` so `sessions()` always reads its sessions as
    /// fresh — used by tests that don't care about the `lsof` escalation,
    /// so they never trigger a real `lsof` call.
    const HUGE_IDLE: f64 = 1e12;

    fn now() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_secs_f64()
    }

    fn fixture_bytes() -> Vec<u8> {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_transcript.jsonl");
        fs::read(&path).unwrap_or_else(|e| panic!("read fixture {path:?}: {e}"))
    }

    fn temp_transcript() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("transcript.jsonl");
        (dir, path)
    }

    fn append(path: &Path, bytes: &[u8]) {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open transcript for append");
        f.write_all(bytes).expect("append to transcript");
    }

    fn assistant(text: &str, in_tok: u64, out_tok: u64, cost: f64) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}],"usage":{{"input_tokens":{in_tok},"output_tokens":{out_tok}}}}},"costUSD":{cost}}}"#
        )
    }

    /// An assistant text line with no `usage`, so it renders as exactly one
    /// line — what the tailer tests want when they're proving line framing
    /// (append/partial/truncate/delete), not stat rendering (already covered
    /// by `render::claude`'s tests).
    fn assistant_text(text: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    #[test]
    fn accumulates_tokens_cost_model_and_tool() {
        // Mirrors tests/test_render.py::TestSessionStats
        // ::test_accumulates_tokens_cost_model_and_tool.
        let (_dir, path) = temp_transcript();
        append(&path, &fixture_bytes());

        let mut s = ClaudeStats::new(&path);
        s.update();

        assert_eq!(s.model, "claude-fable-5");
        assert_eq!(s.in_tok, 125 + 250, "input + both cache legs, per message");
        assert_eq!(s.out_tok, 30);
        assert_eq!(s.last_tool, "Read");
        assert_eq!(s.elapsed(), Some(61.0));
        // Junk lines and the trailing `result` carry no message content, so
        // the last conversational event stands.
        assert_eq!(s.last_event, Some(LastEvent::AssistantText));
        assert_eq!(s.last_line, "Done ✓");
        // The `result` event's top-level usage is a turn summary, not a
        // message: counting it would double the totals.
        assert_eq!(s.cost(), Some(0.5));
    }

    #[test]
    fn result_total_cost_overrides_summed_message_costs() {
        let (_dir, path) = temp_transcript();
        append(&path, assistant("a", 10, 1, 0.01).as_bytes());
        append(&path, b"\n");
        append(&path, assistant("b", 10, 1, 0.02).as_bytes());
        append(&path, b"\n");

        let mut s = ClaudeStats::new(&path);
        s.update();
        assert!(
            s.cost().is_some_and(|c| (c - 0.03).abs() < 1e-9),
            "summed: {:?}",
            s.cost()
        );

        append(&path, br#"{"type":"result","total_cost_usd":0.5}"#);
        append(&path, b"\n");
        s.update();
        assert_eq!(s.cost(), Some(0.5), "the running result total wins");

        // Later per-message costs must not be added on top of it.
        append(&path, assistant("c", 10, 1, 0.07).as_bytes());
        append(&path, b"\n");
        s.update();
        assert_eq!(s.cost(), Some(0.5));
    }

    #[test]
    fn whole_file_and_byte_at_a_time_agree() {
        let bytes = fixture_bytes();
        let (_dir, path) = temp_transcript();

        append(&path, &bytes);
        let mut whole = ClaudeStats::new(&path);
        whole.update();

        fs::write(&path, b"").expect("truncate transcript");
        let mut drip = ClaudeStats::new(&path);
        for i in 0..bytes.len() {
            append(&path, &bytes[i..i + 1]);
            drip.update();
        }

        assert_eq!(drip, whole, "byte-at-a-time drifted from whole-file");
        assert_eq!(drip.offset, bytes.len() as u64);
    }

    #[test]
    fn offset_counts_raw_bytes_not_chars() {
        let (_dir, path) = temp_transcript();
        let line = r#"{"type":"user","message":{"role":"user","content":"héllo → ✓"}}"#;
        append(&path, line.as_bytes());
        append(&path, b"\n");

        let mut s = ClaudeStats::new(&path);
        s.update();
        assert_eq!(s.offset, line.len() as u64 + 1);
        assert!(s.offset > line.chars().count() as u64 + 1, "multibyte line");

        // A second line lands only if the offset stopped in the right place.
        append(&path, assistant("next", 7, 3, 0.0).as_bytes());
        append(&path, b"\n");
        s.update();
        assert_eq!((s.in_tok, s.out_tok), (7, 3));
    }

    #[test]
    fn partial_trailing_line_is_not_consumed() {
        let (_dir, path) = temp_transcript();
        let line = assistant("hi", 100, 10, 0.25);
        append(&path, line.as_bytes()); // no newline yet

        let mut s = ClaudeStats::new(&path);
        s.update();
        assert_eq!(s.offset, 0, "half-written line must not advance the offset");
        assert_eq!((s.in_tok, s.out_tok), (0, 0));
        assert_eq!(s.cost(), None);

        append(&path, b"\n");
        s.update();
        assert_eq!(s.offset, line.len() as u64 + 1);
        assert_eq!((s.in_tok, s.out_tok), (100, 10));
        assert_eq!(s.cost(), Some(0.25));
    }

    #[test]
    fn repeated_update_does_not_double_count() {
        // Mirrors tests/test_render.py::TestSessionStats::test_update_is_incremental.
        let (_dir, path) = temp_transcript();
        append(&path, assistant("a", 1, 1, 0.0).as_bytes());
        append(&path, b"\n");

        let mut s = ClaudeStats::new(&path);
        s.update();
        s.update();
        assert_eq!(s.in_tok, 1);

        append(&path, assistant("b", 2, 2, 0.0).as_bytes());
        append(&path, b"\n");
        s.update();
        assert_eq!((s.in_tok, s.out_tok), (3, 3));
    }

    #[test]
    fn truncation_resets_the_accumulator() {
        // Mirrors tests/test_render.py::TestSessionStats::test_truncation_resets_cleanly.
        let (_dir, path) = temp_transcript();
        append(&path, &fixture_bytes());

        let mut s = ClaudeStats::new(&path);
        s.update();
        assert!(s.in_tok > 0);

        fs::write(&path, b"{}\n").expect("replace transcript with a shorter one");
        s.update();
        assert_eq!(s, {
            let mut fresh = ClaudeStats::new(&path);
            fresh.offset = 3;
            fresh
        });

        append(&path, assistant("after", 1, 1, 0.0).as_bytes());
        append(&path, b"\n");
        s.update();
        assert_eq!((s.in_tok, s.out_tok), (1, 1));
    }

    #[test]
    fn last_event_and_last_line_track_the_newest_event() {
        let (_dir, path) = temp_transcript();
        let mut s = ClaudeStats::new(&path);

        append(
            &path,
            b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"fix  the\\n bug\"}}\n",
        );
        s.update();
        assert_eq!(s.last_event, Some(LastEvent::User));
        assert_eq!(s.last_line, "» fix the bug");

        append(&path, br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"looking"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#);
        append(&path, b"\n");
        s.update();
        assert_eq!(s.last_event, Some(LastEvent::ToolUse("Bash".into())));
        assert_eq!(s.last_line, r#"▶ Bash {"command":"ls"}"#);

        append(&path, br#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","is_error":true,"content":"boom"}]}}"#);
        append(&path, b"\n");
        s.update();
        assert_eq!(s.last_event, Some(LastEvent::ToolResult));
        assert_eq!(s.last_line, "◀ ERROR boom");

        append(&path, assistant("all done", 1, 1, 0.0).as_bytes());
        append(&path, b"\n");
        s.update();
        assert_eq!(s.last_event, Some(LastEvent::AssistantText));
        assert_eq!(s.last_line, "all done");
    }

    #[test]
    fn junk_lines_are_skipped_without_stalling_the_offset() {
        let (_dir, path) = temp_transcript();
        append(&path, b"{malformed\n[1, 2, 3]\n{}\nnot json at all\n");
        append(&path, assistant("ok", 5, 5, 0.0).as_bytes());
        append(&path, b"\n");

        let mut s = ClaudeStats::new(&path);
        s.update();
        assert_eq!((s.in_tok, s.out_tok), (5, 5));
        assert_eq!(s.last_line, "ok");
    }

    #[test]
    fn missing_file_is_a_no_op() {
        let (_dir, path) = temp_transcript();
        let mut s = ClaudeStats::new(&path);
        s.update();
        assert_eq!(s, ClaudeStats::new(&path));
    }

    #[test]
    fn defaults_match_the_python_placeholders() {
        let s = ClaudeStats::new("/nonexistent.jsonl");
        assert_eq!(s.model, "?");
        assert_eq!(s.last_tool, "-");
        assert_eq!(s.cost(), None);
        assert_eq!(s.elapsed(), None);
        assert_eq!(s.last_event, None);
        assert_eq!(s.last_line, "");
    }

    // -------------------------------------------------------- ClaudeSource

    #[test]
    fn sessions_returns_one_per_transcript() {
        let dir = TempDir::new().expect("create temp dir");
        let project = dir.path().join("proj-a");
        fs::create_dir_all(&project).expect("create project dir");
        let path = project.join("session-one.jsonl");
        append(&path, &fixture_bytes());

        let mut src = ClaudeSource::new(dir.path());
        let sessions = src.sessions(now(), HUGE_IDLE);

        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "session-one");
        assert_eq!(s.model, "claude-fable-5");
        assert_eq!(s.in_tok, 125 + 250);
        assert_eq!(s.out_tok, 30);
        assert_eq!(s.cost, Some(0.5));
        // The fixture's own timestamps are long past by the time this test
        // runs, so the freshly written file's mtime floors last_ts (see
        // `mtime_floors_a_stale_last_event_timestamp`); the exact
        // event-derived relationship (last_ts == started_at + 61s) is
        // covered directly on the accumulator by
        // `accumulates_tokens_cost_model_and_tool`, with no mtime involved.
        assert!(s.last_ts >= s.started_at + 61.0);
        assert!(!s.turn_done);
        assert!(!s.tool_pending);
        assert!(!s.ended);
    }

    #[test]
    fn sessions_covers_multiple_transcripts_across_project_dirs() {
        let dir = TempDir::new().expect("create temp dir");
        let a = dir.path().join("proj-a");
        let b = dir.path().join("proj-b");
        fs::create_dir_all(&a).expect("create project dir a");
        fs::create_dir_all(&b).expect("create project dir b");
        append(&a.join("s1.jsonl"), assistant("hi", 1, 1, 0.0).as_bytes());
        append(&a.join("s1.jsonl"), b"\n");
        append(&b.join("s2.jsonl"), assistant("yo", 2, 2, 0.0).as_bytes());
        append(&b.join("s2.jsonl"), b"\n");

        let mut src = ClaudeSource::new(dir.path());
        let mut ids: Vec<_> = src
            .sessions(now(), HUGE_IDLE)
            .into_iter()
            .map(|s| s.id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["s1".to_string(), "s2".to_string()]);
    }

    #[test]
    fn empty_dir_returns_empty_vec() {
        let dir = TempDir::new().expect("create temp dir");
        let mut src = ClaudeSource::new(dir.path());
        assert_eq!(src.sessions(now(), HUGE_IDLE), Vec::new());
    }

    #[test]
    fn missing_root_returns_empty_vec_not_error() {
        let mut src = ClaudeSource::new("/nonexistent/claude/projects/root");
        assert_eq!(src.sessions(now(), HUGE_IDLE), Vec::new());
    }

    #[test]
    fn malformed_lines_are_skipped_without_panic() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("junk.jsonl");
        append(&path, b"{malformed\n[1, 2, 3]\nnot json at all\n");

        let mut src = ClaudeSource::new(dir.path());
        let sessions = src.sessions(now(), HUGE_IDLE);

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].model, "?");
        assert_eq!(sessions[0].last_tool, "-");
        assert_eq!(sessions[0].cost, None);
    }

    #[test]
    fn sessions_reuses_the_accumulator_across_calls() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("s.jsonl");
        append(&path, assistant("a", 1, 1, 0.0).as_bytes());
        append(&path, b"\n");

        let mut src = ClaudeSource::new(dir.path());
        let first = src.sessions(now(), HUGE_IDLE);
        assert_eq!(first[0].in_tok, 1);

        append(&path, assistant("b", 2, 2, 0.0).as_bytes());
        append(&path, b"\n");
        let second = src.sessions(now(), HUGE_IDLE);
        // Incremental: totals accumulate rather than resetting each scan.
        assert_eq!(second[0].in_tok, 3);
        assert_eq!(second[0].out_tok, 3);
    }

    #[test]
    fn last_tool_reads_from_the_accumulator() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("s.jsonl");
        append(&path, &fixture_bytes());

        let mut src = ClaudeSource::new(dir.path());
        src.sessions(now(), HUGE_IDLE);
        assert_eq!(src.last_tool("s"), "Read");
    }

    #[test]
    fn last_tool_of_unknown_session_is_a_placeholder() {
        let dir = TempDir::new().expect("create temp dir");
        let mut src = ClaudeSource::new(dir.path());
        assert_eq!(src.last_tool("nope"), "-");
    }

    #[test]
    fn subagent_transcripts_are_not_scanned_as_sessions() {
        let dir = TempDir::new().expect("create temp dir");
        let session = dir.path().join("slug").join("9f7712");
        fs::create_dir_all(session.join("subagents")).expect("create fixture tree");
        let own = session.join("9f7712.jsonl");
        append(&own, assistant("a", 1, 1, 0.0).as_bytes());
        append(&own, b"\n");
        let sub = session.join("subagents").join("agent-1.jsonl");
        append(&sub, assistant("b", 2, 2, 0.0).as_bytes());
        append(&sub, b"\n");

        assert_eq!(scan_jsonl_files(dir.path(), RECENCY_WINDOW), vec![own]);
    }

    #[test]
    fn stale_transcripts_outside_the_recency_window_are_skipped() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("old.jsonl");
        append(&path, assistant("a", 1, 1, 0.0).as_bytes());
        append(&path, b"\n");
        let stale = SystemTime::now() - (RECENCY_WINDOW + Duration::from_secs(1));
        File::open(&path)
            .expect("open transcript")
            .set_modified(stale)
            .expect("set stale mtime");

        let mut src = ClaudeSource::new(dir.path());
        assert_eq!(src.sessions(now(), HUGE_IDLE), Vec::new());
    }

    #[test]
    fn mtime_floors_a_stale_last_event_timestamp() {
        use crate::source::{Liveness, classify};

        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("s.jsonl");
        // Last *timestamped* event is old, as if the transcript's tail
        // carries an untimestamped tool result appended just now — the
        // divergence this fix closes (`hermon.py:431`).
        append(
            &path,
            br#"{"type":"user","message":{"role":"user","content":"hi"},"timestamp":"2020-01-01T00:00:00Z"}"#,
        );
        append(&path, b"\n");
        let fresh = SystemTime::now();
        File::open(&path)
            .expect("open transcript")
            .set_modified(fresh)
            .expect("set fresh mtime");

        let mut src = ClaudeSource::new(dir.path());
        let sessions = src.sessions(now(), HUGE_IDLE);
        assert_eq!(sessions.len(), 1);
        let mtime = mtime_secs(&path).expect("mtime");
        assert!(
            (sessions[0].last_ts - mtime).abs() < 1.0,
            "last_ts should floor to mtime: {} vs {}",
            sessions[0].last_ts,
            mtime
        );

        let live = classify(&sessions[0], mtime, 300.0, 3600.0);
        assert_eq!(live, Liveness::Live);
    }

    #[test]
    fn event_timestamp_newer_than_mtime_wins_on_clock_skew() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("s.jsonl");
        // A timestamp far ahead of the file's real mtime, simulating clock
        // skew between whatever clock stamped the event and the
        // filesystem's clock. The larger value must survive.
        let future_ts = "2030-01-01T00:00:00Z";
        append(
            &path,
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":"hi"}},"timestamp":"{future_ts}"}}"#
            )
            .as_bytes(),
        );
        append(&path, b"\n");

        let mut src = ClaudeSource::new(dir.path());
        let sessions = src.sessions(now(), HUGE_IDLE);
        assert_eq!(sessions.len(), 1);
        let expected = parse_ts(future_ts).expect("parse future ts");
        assert_eq!(sessions[0].last_ts, expected);
    }

    // -------------------------------------------------------- lsof `-F a` parsing

    #[test]
    fn parse_write_access_true_for_write_line() {
        assert!(parse_write_access("p1234\nf5\naw\n"));
    }

    #[test]
    fn parse_write_access_false_for_read_only_line() {
        assert!(!parse_write_access("p1234\nf5\nar\n"));
    }

    #[test]
    fn parse_write_access_true_for_update_line() {
        assert!(parse_write_access("p1234\nf5\nau\n"));
    }

    #[test]
    fn parse_write_access_false_for_empty_output() {
        assert!(!parse_write_access(""));
    }

    #[test]
    fn parse_write_access_false_for_garbage() {
        assert!(!parse_write_access("not lsof output at all\n123\n\n"));
    }

    #[test]
    fn parse_write_access_true_when_any_descriptor_has_write_access() {
        // A read-only fd alongside a write fd: any match wins.
        assert!(parse_write_access("p1234\nf5\nar\nf6\naw\n"));
    }

    // -------------------------------------------------------- lsof cost bounding

    /// A probe stub that counts its own invocations instead of shelling out,
    /// so "at most one `lsof` spawn per session per tick" is provable
    /// without depending on `lsof` being installed.
    fn counting_probe(counter: Rc<Cell<usize>>, verdict: bool) -> WriteHandleProbe {
        Rc::new(move |_: &Path| {
            counter.set(counter.get() + 1);
            Some(verdict)
        })
    }

    fn source_with_probe(root: &Path, probe: WriteHandleProbe) -> ClaudeSource {
        ClaudeSource {
            root: root.to_path_buf(),
            stats: HashMap::new(),
            probe,
        }
    }

    #[test]
    fn fresh_session_never_probes_lsof() {
        let dir = TempDir::new().expect("create temp dir");
        append(
            &dir.path().join("s.jsonl"),
            assistant("a", 1, 1, 0.0).as_bytes(),
        );
        append(&dir.path().join("s.jsonl"), b"\n");

        let calls = Rc::new(Cell::new(0));
        let mut src = source_with_probe(dir.path(), counting_probe(calls.clone(), true));

        let sessions = src.sessions(now(), 100.0);
        assert_eq!(sessions.len(), 1);
        assert!(!sessions[0].force_live);
        assert_eq!(
            calls.get(),
            0,
            "a fresh transcript must not shell out to lsof"
        );
    }

    #[test]
    fn stale_far_beyond_the_window_and_untracked_never_probes_lsof() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("s.jsonl");
        append(&path, assistant("a", 1, 1, 0.0).as_bytes());
        append(&path, b"\n");
        let idle_timeout = 1.0;
        let stale =
            SystemTime::now() - Duration::from_secs_f64(idle_timeout * LSOF_STALE_MULT + 10.0);
        File::open(&path)
            .expect("open transcript")
            .set_modified(stale)
            .expect("set stale mtime");

        let calls = Rc::new(Cell::new(0));
        let mut src = source_with_probe(dir.path(), counting_probe(calls.clone(), true));

        let sessions = src.sessions(now(), idle_timeout);
        assert_eq!(sessions.len(), 1);
        assert!(!sessions[0].force_live);
        assert_eq!(
            calls.get(),
            0,
            "far-stale, never-tracked session must not shell out"
        );
    }

    #[test]
    fn stale_within_window_probes_at_most_once_per_tick() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("s.jsonl");
        append(&path, assistant("a", 1, 1, 0.0).as_bytes());
        append(&path, b"\n");
        let idle_timeout = 1.0;
        let stale = SystemTime::now() - Duration::from_secs_f64(idle_timeout * 2.0);
        File::open(&path)
            .expect("open transcript")
            .set_modified(stale)
            .expect("set stale mtime");

        let calls = Rc::new(Cell::new(0));
        let mut src = source_with_probe(dir.path(), counting_probe(calls.clone(), true));

        let now_ts = now();
        let first = src.sessions(now_ts, idle_timeout);
        assert_eq!(first.len(), 1);
        assert!(first[0].force_live);
        assert_eq!(
            calls.get(),
            1,
            "exactly one lsof spawn for the one stale session this tick"
        );

        // Second tick: still stale, and now "already tracked" — the cost
        // bound's other leg keeps checking it, but still only once.
        let second = src.sessions(now_ts, idle_timeout);
        assert_eq!(second.len(), 1);
        assert_eq!(
            calls.get(),
            2,
            "one more spawn on the second tick, not more"
        );
    }

    // -------------------------------------------------------- lsof integration

    #[test]
    fn open_write_handle_keeps_a_stale_session_live_past_idle_timeout() {
        use crate::source::{Liveness, classify};

        if !lsof_available() {
            eprintln!(
                "hermon: lsof not available on this system — skipping the open-write-\
                 handle integration test and checking the mtime-only fallback instead"
            );
            let dir = TempDir::new().expect("create temp dir");
            let path = dir.path().join("s.jsonl");
            append(&path, b"{}\n");
            assert_eq!(
                has_open_write_handle(&path),
                None,
                "lsof unavailable must read as None, not a guessed verdict"
            );
            return;
        }

        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("s.jsonl");
        append(&path, assistant("a", 1, 1, 0.0).as_bytes());
        append(&path, b"\n");
        let idle_timeout = 1.0;
        let stale = SystemTime::now() - Duration::from_secs_f64(idle_timeout * 2.0);
        File::open(&path)
            .expect("open transcript")
            .set_modified(stale)
            .expect("set stale mtime");

        // Hold our own write handle open, as a real Claude Code process
        // would while still working on this transcript.
        let handle = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open write handle");

        let mut src = ClaudeSource::new(dir.path());
        let now_ts = now();
        let sessions = src.sessions(now_ts, idle_timeout);
        assert_eq!(sessions.len(), 1);
        assert!(
            sessions[0].force_live,
            "a stale transcript with an open write handle must read live"
        );
        assert_eq!(
            classify(&sessions[0], now_ts, idle_timeout, 3600.0),
            Liveness::Live
        );

        drop(handle);
        let now_ts2 = now();
        let sessions2 = src.sessions(now_ts2, idle_timeout);
        assert_eq!(sessions2.len(), 1);
        assert!(
            !sessions2[0].force_live,
            "once the write handle closes, the stale transcript must read done"
        );
        assert_eq!(
            classify(&sessions2[0], now_ts2, idle_timeout, 3600.0),
            Liveness::Done
        );
    }

    // -------------------------------------------------------- ClaudeTailer

    const NO_REPLAY: Replay = Replay { bytes: 0, rows: 0 };
    const HUGE_REPLAY: Replay = Replay {
        bytes: 1_000_000,
        rows: 0,
    };

    #[test]
    fn tailer_waits_for_a_missing_file_and_self_heals() {
        let (_dir, path) = temp_transcript();
        let mut t = ClaudeTailer::new(&path, HUGE_REPLAY);

        let first = t.poll();
        assert_eq!(first.len(), 1);
        assert!(first[0].to_plain().contains("not found"), "{:?}", first);

        // No repeated status line while it stays missing.
        assert_eq!(t.poll(), Vec::new());
        assert_eq!(t.poll(), Vec::new());

        append(&path, assistant_text("hi").as_bytes());
        append(&path, b"\n");
        let out = t.poll();
        assert_eq!(
            out.iter().map(StyledLine::to_plain).collect::<Vec<_>>(),
            vec!["hi"]
        );
    }

    #[test]
    fn tailer_streams_appended_lines_exactly_once() {
        let (_dir, path) = temp_transcript();
        append(&path, assistant_text("one").as_bytes());
        append(&path, b"\n");

        let mut t = ClaudeTailer::new(&path, HUGE_REPLAY);
        assert_eq!(
            t.poll()
                .iter()
                .map(StyledLine::to_plain)
                .collect::<Vec<_>>(),
            vec!["one"]
        );
        assert_eq!(
            t.poll(),
            Vec::new(),
            "already-consumed bytes must not repeat"
        );

        append(&path, assistant_text("two").as_bytes());
        append(&path, b"\n");
        assert_eq!(
            t.poll()
                .iter()
                .map(StyledLine::to_plain)
                .collect::<Vec<_>>(),
            vec!["two"]
        );
    }

    #[test]
    fn tailer_buffers_a_partial_line_until_the_newline_arrives() {
        let (_dir, path) = temp_transcript();
        let mut t = ClaudeTailer::new(&path, HUGE_REPLAY);
        t.poll(); // consume the initial "not found" line

        let line = assistant_text("partial");
        append(&path, line.as_bytes()); // no trailing newline
        assert_eq!(
            t.poll(),
            Vec::new(),
            "unterminated line must not render yet"
        );
        assert_eq!(t.poll(), Vec::new(), "still nothing on a repeat poll");

        append(&path, b"\n");
        assert_eq!(
            t.poll()
                .iter()
                .map(StyledLine::to_plain)
                .collect::<Vec<_>>(),
            vec!["partial"]
        );
    }

    #[test]
    fn tailer_truncate_emits_reset_line_and_tailing_resumes() {
        let (_dir, path) = temp_transcript();
        append(&path, assistant_text("before").as_bytes());
        append(&path, b"\n");

        let mut t = ClaudeTailer::new(&path, HUGE_REPLAY);
        t.poll(); // replay "before"

        fs::write(&path, b"").expect("truncate transcript to empty");
        let out = t.poll();
        assert_eq!(out.len(), 1);
        assert!(out[0].to_plain().contains("truncated"), "{:?}", out);

        append(&path, assistant_text("after").as_bytes());
        append(&path, b"\n");
        assert_eq!(
            t.poll()
                .iter()
                .map(StyledLine::to_plain)
                .collect::<Vec<_>>(),
            vec!["after"],
            "tailing must resume from byte zero of the reloaded file"
        );
    }

    #[test]
    fn tailer_delete_recreate_emits_wait_line_and_self_heals() {
        let (_dir, path) = temp_transcript();
        append(&path, assistant_text("before").as_bytes());
        append(&path, b"\n");

        let mut t = ClaudeTailer::new(&path, HUGE_REPLAY);
        t.poll(); // replay "before"

        fs::remove_file(&path).expect("delete transcript");
        let out = t.poll();
        assert_eq!(out.len(), 1);
        assert!(out[0].to_plain().contains("removed"), "{:?}", out);
        assert_eq!(
            t.poll(),
            Vec::new(),
            "no repeated wait line while still missing"
        );

        append(&path, assistant_text("recreated").as_bytes());
        append(&path, b"\n");
        assert_eq!(
            t.poll()
                .iter()
                .map(StyledLine::to_plain)
                .collect::<Vec<_>>(),
            vec!["recreated"]
        );
    }

    #[test]
    fn tailer_replay_seeks_near_the_end_and_discards_the_partial_first_line() {
        let (_dir, path) = temp_transcript();
        let one = assistant_text("one") + "\n";
        let two = assistant_text("two") + "\n";
        append(&path, one.as_bytes());
        append(&path, two.as_bytes());

        // A replay window that lands inside `two`: the seek point's partial
        // line is discarded, so neither `one` (before the seek) nor the
        // fragment of `two` (partial at the seek point) should appear.
        let replay = Replay {
            bytes: (two.len() / 2) as u64,
            rows: 0,
        };
        let mut t = ClaudeTailer::new(&path, replay);
        assert_eq!(t.poll(), Vec::new());

        // Tailing resumes cleanly from that point onward.
        append(&path, assistant_text("three").as_bytes());
        append(&path, b"\n");
        assert_eq!(
            t.poll()
                .iter()
                .map(StyledLine::to_plain)
                .collect::<Vec<_>>(),
            vec!["three"]
        );
    }

    #[test]
    fn tailer_replay_zero_bytes_skips_all_existing_content() {
        let (_dir, path) = temp_transcript();
        append(&path, assistant_text("stale").as_bytes());
        append(&path, b"\n");

        let mut t = ClaudeTailer::new(&path, NO_REPLAY);
        assert_eq!(t.poll(), Vec::new());

        append(&path, assistant_text("fresh").as_bytes());
        append(&path, b"\n");
        assert_eq!(
            t.poll()
                .iter()
                .map(StyledLine::to_plain)
                .collect::<Vec<_>>(),
            vec!["fresh"]
        );
    }

    #[test]
    fn source_open_tailer_is_none_for_an_unknown_session() {
        let dir = TempDir::new().expect("create temp dir");
        let src = ClaudeSource::new(dir.path());
        assert!(src.open_tailer("nope", HUGE_REPLAY).is_none());
    }

    #[test]
    fn source_open_tailer_streams_a_session_seen_by_sessions() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("s.jsonl");
        append(&path, assistant_text("hi").as_bytes());
        append(&path, b"\n");

        let mut src = ClaudeSource::new(dir.path());
        src.sessions(now(), HUGE_IDLE);

        let mut tailer = src
            .open_tailer("s", HUGE_REPLAY)
            .expect("known session tails");
        assert_eq!(
            tailer
                .poll()
                .iter()
                .map(StyledLine::to_plain)
                .collect::<Vec<_>>(),
            vec!["hi"]
        );
    }
}
