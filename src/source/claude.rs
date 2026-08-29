//! Claude Code transcript source (`~/.claude/projects/**/*.jsonl`).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::render::{clip, parse_ts};
use crate::source::{LastEvent, Replay, SessionMeta, Tailer};

/// Tool arguments are clipped at 120 chars, tool results at 200
/// (`hermon.py:212`, `hermon.py:243`).
const ARG_CLIP: usize = 120;
const RESULT_CLIP: usize = 200;

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
    /// summed per-message costs (`hermon.py:343`).
    pub fn cost(&self) -> f64 {
        self.cost_reported.unwrap_or(self.cost_sum)
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
fn count(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_f64).map_or(0, |n| n as u64)
}

/// Input tokens plus both cache legs (`hermon.py:166 _usage_in`).
fn usage_in(usage: &Map<String, Value>) -> u64 {
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
fn event_cost(ev: &Map<String, Value>) -> Option<f64> {
    ["total_cost_usd", "cost_usd", "costUSD"]
        .iter()
        .find_map(|k| ev.get(*k).and_then(Value::as_f64))
}

/// `tool_result` content is a plain string or a list of blocks
/// (`hermon.py:155 _result_text`).
fn result_text(content: Option<&Value>) -> String {
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
    stats: HashMap<String, ClaudeStats>,
}

impl ClaudeSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ClaudeSource {
            root: root.into(),
            stats: HashMap::new(),
        }
    }

    /// One [`SessionMeta`] per transcript modified within
    /// [`RECENCY_WINDOW`]. Claude carries no turn-completion signal, so
    /// `turn_done`, `tool_pending` and `ended` are always false — the
    /// engine special-cases this source's liveness from `last_ts`/mtime
    /// instead of calling `turn_liveness` (`hermon.py:431`).
    pub fn sessions(&mut self) -> Vec<SessionMeta> {
        scan_jsonl_files(&self.root, RECENCY_WINDOW)
            .into_iter()
            .filter_map(|path| self.session_for(path))
            .collect()
    }

    fn session_for(&mut self, path: PathBuf) -> Option<SessionMeta> {
        let id = path.file_stem()?.to_str()?.to_string();
        let mtime = mtime_secs(&path);
        let stats = self
            .stats
            .entry(id.clone())
            .or_insert_with(|| ClaudeStats::new(path));
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
    /// — this source is used by
    /// concrete type, so the trait's default never applies to it.
    /// `None` until the Claude transcript tailer lands.
    pub fn open_tailer(&self, _session_id: &str, _replay: Replay) -> Option<Box<dyn Tailer>> {
        None
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
/// `window` of now, sorted for a deterministic scan order. I/O errors
/// (missing root, permission denied) are swallowed and yield an empty
/// scan, matching Python's `except OSError: return []` (`hermon.py:424`).
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
            walk_jsonl(&path, cutoff, out);
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

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;

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
        assert_eq!(s.cost(), 0.5);
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
        assert!((s.cost() - 0.03).abs() < 1e-9, "summed: {}", s.cost());

        append(&path, br#"{"type":"result","total_cost_usd":0.5}"#);
        append(&path, b"\n");
        s.update();
        assert_eq!(s.cost(), 0.5, "the running result total wins");

        // Later per-message costs must not be added on top of it.
        append(&path, assistant("c", 10, 1, 0.07).as_bytes());
        append(&path, b"\n");
        s.update();
        assert_eq!(s.cost(), 0.5);
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
        assert_eq!(s.cost(), 0.0);

        append(&path, b"\n");
        s.update();
        assert_eq!(s.offset, line.len() as u64 + 1);
        assert_eq!((s.in_tok, s.out_tok), (100, 10));
        assert_eq!(s.cost(), 0.25);
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
        assert_eq!(s.cost(), 0.0);
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
        let sessions = src.sessions();

        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "session-one");
        assert_eq!(s.model, "claude-fable-5");
        assert_eq!(s.in_tok, 125 + 250);
        assert_eq!(s.out_tok, 30);
        assert_eq!(s.cost, 0.5);
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
        let mut ids: Vec<_> = src.sessions().into_iter().map(|s| s.id).collect();
        ids.sort();
        assert_eq!(ids, vec!["s1".to_string(), "s2".to_string()]);
    }

    #[test]
    fn empty_dir_returns_empty_vec() {
        let dir = TempDir::new().expect("create temp dir");
        let mut src = ClaudeSource::new(dir.path());
        assert_eq!(src.sessions(), Vec::new());
    }

    #[test]
    fn missing_root_returns_empty_vec_not_error() {
        let mut src = ClaudeSource::new("/nonexistent/claude/projects/root");
        assert_eq!(src.sessions(), Vec::new());
    }

    #[test]
    fn malformed_lines_are_skipped_without_panic() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("junk.jsonl");
        append(&path, b"{malformed\n[1, 2, 3]\nnot json at all\n");

        let mut src = ClaudeSource::new(dir.path());
        let sessions = src.sessions();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].model, "?");
        assert_eq!(sessions[0].last_tool, "-");
        assert_eq!(sessions[0].cost, 0.0);
    }

    #[test]
    fn sessions_reuses_the_accumulator_across_calls() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("s.jsonl");
        append(&path, assistant("a", 1, 1, 0.0).as_bytes());
        append(&path, b"\n");

        let mut src = ClaudeSource::new(dir.path());
        let first = src.sessions();
        assert_eq!(first[0].in_tok, 1);

        append(&path, assistant("b", 2, 2, 0.0).as_bytes());
        append(&path, b"\n");
        let second = src.sessions();
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
        src.sessions();
        assert_eq!(src.last_tool("s"), "Read");
    }

    #[test]
    fn last_tool_of_unknown_session_is_a_placeholder() {
        let dir = TempDir::new().expect("create temp dir");
        let mut src = ClaudeSource::new(dir.path());
        assert_eq!(src.last_tool("nope"), "-");
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
        assert_eq!(src.sessions(), Vec::new());
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
        let sessions = src.sessions();
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
        let sessions = src.sessions();
        assert_eq!(sessions.len(), 1);
        let expected = parse_ts(future_ts).expect("parse future ts");
        assert_eq!(sessions[0].last_ts, expected);
    }
}
