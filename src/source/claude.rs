//! Claude Code transcript source (`~/.claude/projects/**/*.jsonl`).

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::render::{clip, parse_ts};
use crate::source::LastEvent;

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
}
