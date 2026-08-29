//! Renders OpenCode session parts.
//!
//! Port of `hermon.py:877 render_opencode_part`. OpenCode is the odd one
//! out: Claude and Hermes rows are append-only, one row per finalized
//! event, but an OpenCode tool `part` is **updated in place** as it moves
//! pending → completed/error — the same row, not a new one. Rendering it
//! statelessly would print the tool call twice; keying a cursor on rowid
//! would miss the flip entirely. So the renderer is pure but
//! stateful-by-argument: the caller hands back what it last rendered for
//! that row ([`PartStatus`]) and gets both the new lines and the status to
//! remember.
//!
//! A part is therefore rendered at most twice: `▶ tool` when first seen
//! (immediately followed by `◀ result` too, if it had already finished by
//! the time we polled — a fast tool call can complete between two polls,
//! and both lines still need to show), then `◀ result`/`◀ ERROR` alone
//! when the status actually changes. Non-tool parts (text, reasoning,
//! file, patch, step-start/finish) render once, on first sight.
//!
//! Malformed rows degrade to a dim marker; nothing here can panic.

use serde_json::{Map, Value};

use super::{Seg, Sem, StyledLine, clip};

/// What was last rendered for one `part` row, remembered by the caller and
/// passed back on the next sighting of that row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartStatus {
    /// A non-tool part, already rendered once (`hermon.py`'s `"shown"`).
    Shown,
    /// A tool part, holding the `state.status` last rendered for it.
    Tool(String),
}

impl PartStatus {
    /// The Python-side string this status compares as, so the transition
    /// rules below read exactly like `hermon.py:912`.
    fn as_str(&self) -> &str {
        match self {
            PartStatus::Shown => "shown",
            PartStatus::Tool(s) => s,
        }
    }
}

/// Lines for one `part` row given the status last rendered for it (`None`
/// if never seen), plus the status to remember.
///
/// The returned status is `None` only where the Python returns
/// `prev_status` unchanged and that was itself `None`: an unparseable row,
/// or a tool part whose `state` carries no `status` at all.
pub fn render_opencode_part(
    role: Option<&str>,
    raw_data: &str,
    prev: Option<&PartStatus>,
) -> (Vec<StyledLine>, Option<PartStatus>) {
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(raw_data) else {
        return (vec![dim("· parse-skip")], prev.cloned());
    };

    if data.get("type").and_then(Value::as_str) != Some("tool") {
        return match prev {
            Some(seen) => (Vec::new(), Some(seen.clone())),
            None => (simple_part(role, &data), Some(PartStatus::Shown)),
        };
    }

    let state = data.get("state").and_then(Value::as_object);
    let status = state.and_then(|s| s.get("status")).and_then(Value::as_str);
    let finished = matches!(status, Some("completed" | "error"));

    let mut lines = Vec::new();
    match prev.map(PartStatus::as_str) {
        None => {
            lines.push(tool_call(&data, state));
            if finished {
                lines.push(tool_result(&data, state, status));
            }
        }
        Some(seen) => {
            if finished && status != Some(seen) {
                lines.push(tool_result(&data, state, status));
            }
        }
    }
    let next = match status {
        Some(s) => Some(PartStatus::Tool(s.to_string())),
        None => prev.cloned(),
    };
    (lines, next)
}

/// `▶ bash {"command": "ls"}` — the call, with its input clipped to 120
/// chars (`hermon.py:823`).
///
/// The input is re-serialized from the parsed row, so it reads
/// `{"command":"ls"}` where the Python's `json.dumps` gives
/// `{"command": "ls"}`, and multi-key inputs come out in key order rather
/// than the order OpenCode wrote them (serde_json orders object keys unless
/// the `preserve_order` feature pulls in another dependency). Same
/// characters otherwise, and the clip budget is unchanged.
fn tool_call(data: &Map<String, Value>, state: Option<&Map<String, Value>>) -> StyledLine {
    let name = tool_name(data);
    let arg = state
        .and_then(|s| s.get("input"))
        .map_or_else(|| "{}".to_string(), Value::to_string);
    StyledLine(vec![
        Seg::new(Sem::Bold, format!("▶ {name}")),
        Seg::new(Sem::Plain, " "),
        Seg::new(Sem::Dim, clip(&arg, 120)),
    ])
}

/// `◀ bash file1 file2`, or `◀ ERROR bash: …` when the tool failed
/// (`hermon.py:834`).
fn tool_result(
    data: &Map<String, Value>,
    state: Option<&Map<String, Value>>,
    status: Option<&str>,
) -> StyledLine {
    let name = tool_name(data);
    let field = |key: &str| text_of(state.and_then(|s| s.get(key)));
    if status == Some("error") {
        return StyledLine(vec![
            Seg::new(Sem::Error, "◀ ERROR"),
            Seg::new(Sem::Plain, " "),
            Seg::new(Sem::Dim, format!("{name}: {}", clip(&field("error"), 180))),
        ]);
    }
    StyledLine(vec![Seg::new(
        Sem::Dim,
        format!("◀ {name} {}", clip(&field("output"), 200)),
    )])
}

/// Everything that is not a tool call: prompt and response text verbatim,
/// every other part type as a dim type marker (`hermon.py:864`).
///
/// Where the Python wraps text to the terminal width, this emits one
/// logical line per source line and leaves wrapping to the pane, as the
/// rest of the Rust renderers do.
fn simple_part(role: Option<&str>, data: &Map<String, Value>) -> Vec<StyledLine> {
    let ptype = data.get("type").and_then(Value::as_str);
    if ptype != Some("text") {
        let label = ptype.filter(|t| !t.is_empty()).unwrap_or("unknown");
        return vec![dim(format!("· {label}"))];
    }
    let text = text_of(data.get("text"));
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 && role == Some("user") {
                StyledLine(vec![
                    Seg::new(Sem::User, "» user:"),
                    Seg::new(Sem::Plain, format!(" {line}")),
                ])
            } else {
                StyledLine(vec![Seg::new(Sem::Plain, line)])
            }
        })
        .collect()
}

fn tool_name(data: &Map<String, Value>) -> &str {
    data.get("tool").and_then(Value::as_str).unwrap_or("?")
}

/// A JSON field as display text: strings unquoted, anything else as its
/// JSON form, missing as empty — the Rust side of the Python's `str()`.
fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn dim(text: impl Into<String>) -> StyledLine {
    StyledLine(vec![Seg::new(Sem::Dim, text)])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transition matrix, case by case, mirroring
    /// `tests/test_opencode.py:98-152`.
    fn plain(lines: &[StyledLine]) -> Vec<String> {
        lines.iter().map(StyledLine::to_plain).collect()
    }

    fn tool(status: &str, extra: &str) -> String {
        format!(
            r#"{{"type":"tool","tool":"bash","state":{{"status":"{status}"{extra}}}}}"#,
            extra = if extra.is_empty() {
                String::new()
            } else {
                format!(",{extra}")
            }
        )
    }

    #[test]
    fn never_seen_pending_tool_shows_only_the_call() {
        let data = tool("running", r#""input":{"command":"ls"}"#);
        let (lines, status) = render_opencode_part(Some("assistant"), &data, None);
        assert_eq!(plain(&lines), [r#"▶ bash {"command":"ls"}"#]);
        assert_eq!(status, Some(PartStatus::Tool("running".into())));
    }

    #[test]
    fn tool_input_is_clipped_at_120_chars() {
        let long = "x".repeat(300);
        let data = tool("running", &format!(r#""input":{{"command":"ls {long}"}}"#));
        let (lines, _) = render_opencode_part(Some("assistant"), &data, None);
        let out = plain(&lines);
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("▶ bash "), "{}", out[0]);
        assert!(out[0].ends_with('…'), "{}", out[0]);
    }

    #[test]
    fn never_seen_completed_tool_shows_call_and_result() {
        // a fast tool call can complete between two polls
        let data = r#"{"type":"tool","tool":"read","state":{"status":"completed",
            "input":{"filePath":"x.py"},"output":"print('hi')"}}"#;
        let (lines, status) = render_opencode_part(Some("assistant"), data, None);
        let out = plain(&lines);
        assert_eq!(out.len(), 2);
        assert!(out[0].starts_with("▶ read"), "{}", out[0]);
        assert_eq!(out[1], "◀ read print('hi')");
        assert_eq!(status, Some(PartStatus::Tool("completed".into())));
    }

    #[test]
    fn never_seen_errored_tool_shows_call_and_error() {
        let data = r#"{"type":"tool","tool":"edit","state":{"status":"error",
            "input":{},"error":"user rejected permission"}}"#;
        let (lines, status) = render_opencode_part(Some("assistant"), data, None);
        let out = plain(&lines);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1], "◀ ERROR edit: user rejected permission");
        assert_eq!(status, Some(PartStatus::Tool("error".into())));
        assert_eq!(lines[1].0[0].sem, Sem::Error);
    }

    #[test]
    fn pending_to_completed_shows_only_the_result() {
        let pending = tool("running", r#""input":{"command":"ls"}"#);
        let (_, status) = render_opencode_part(Some("assistant"), &pending, None);
        let done = tool(
            "completed",
            r#""input":{"command":"ls"},"output":"file1\nfile2""#,
        );
        let (lines, next) = render_opencode_part(Some("assistant"), &done, status.as_ref());
        assert_eq!(plain(&lines), ["◀ bash file1 file2"]);
        assert_eq!(next, Some(PartStatus::Tool("completed".into())));
    }

    #[test]
    fn pending_to_error_shows_only_the_error() {
        let pending = tool("running", "");
        let (_, status) = render_opencode_part(Some("assistant"), &pending, None);
        let failed = tool("error", r#""error":"boom""#);
        let (lines, _) = render_opencode_part(Some("assistant"), &failed, status.as_ref());
        assert_eq!(plain(&lines), ["◀ ERROR bash: boom"]);
    }

    #[test]
    fn still_pending_renders_nothing() {
        let pending = tool("running", r#""input":{"command":"ls"}"#);
        let (_, status) = render_opencode_part(Some("assistant"), &pending, None);
        let (lines, next) = render_opencode_part(Some("assistant"), &pending, status.as_ref());
        assert!(lines.is_empty());
        assert_eq!(next, status);
    }

    #[test]
    fn completed_tool_re_touched_renders_nothing() {
        let done = tool("completed", r#""output":"ok""#);
        let (first, status) = render_opencode_part(Some("assistant"), &done, None);
        assert_eq!(first.len(), 2);
        let (lines, _) = render_opencode_part(Some("assistant"), &done, status.as_ref());
        assert!(lines.is_empty());
    }

    #[test]
    fn tool_result_is_clipped_at_200_chars() {
        let done = tool("completed", &format!(r#""output":"{}""#, "y".repeat(500)));
        let (lines, _) = render_opencode_part(Some("assistant"), &done, None);
        let out = plain(&lines);
        assert_eq!(out[1].chars().count(), "◀ bash ".chars().count() + 200);
        assert!(out[1].ends_with('…'));
    }

    #[test]
    fn user_text_part_is_prefixed() {
        let data = r#"{"type":"text","text":"implement the feature"}"#;
        let (lines, status) = render_opencode_part(Some("user"), data, None);
        assert_eq!(plain(&lines), ["» user: implement the feature"]);
        assert_eq!(lines[0].0[0].sem, Sem::User);
        assert_eq!(status, Some(PartStatus::Shown));
    }

    #[test]
    fn assistant_text_part_is_plain() {
        let data = r#"{"type":"text","text":"Here's the plan."}"#;
        let (lines, _) = render_opencode_part(Some("assistant"), data, None);
        assert_eq!(plain(&lines), ["Here's the plan."]);
    }

    #[test]
    fn multi_line_text_becomes_one_line_each() {
        let data = r#"{"type":"text","text":"one\ntwo"}"#;
        let (lines, _) = render_opencode_part(Some("user"), data, None);
        assert_eq!(plain(&lines), ["» user: one", "two"]);
    }

    #[test]
    fn empty_text_part_renders_nothing_but_is_marked_shown() {
        let data = r#"{"type":"text","text":"   "}"#;
        let (lines, status) = render_opencode_part(Some("assistant"), data, None);
        assert!(lines.is_empty());
        assert_eq!(status, Some(PartStatus::Shown));
    }

    #[test]
    fn non_tool_part_is_rendered_exactly_once() {
        let data = r#"{"type":"text","text":"hello"}"#;
        let (first, status) = render_opencode_part(Some("assistant"), data, None);
        assert_eq!(plain(&first), ["hello"]);
        let (again, next) = render_opencode_part(Some("assistant"), data, status.as_ref());
        assert!(again.is_empty());
        assert_eq!(next, Some(PartStatus::Shown));
    }

    #[test]
    fn other_part_types_are_dim_markers() {
        for ptype in ["reasoning", "step-start", "step-finish", "file", "patch"] {
            let data = format!(r#"{{"type":"{ptype}"}}"#);
            let (lines, status) = render_opencode_part(Some("assistant"), &data, None);
            assert_eq!(plain(&lines), [format!("· {ptype}")]);
            assert_eq!(status, Some(PartStatus::Shown));
        }
    }

    #[test]
    fn missing_type_is_an_unknown_marker() {
        let (lines, _) = render_opencode_part(Some("assistant"), r#"{"text":"x"}"#, None);
        assert_eq!(plain(&lines), ["· unknown"]);
    }

    #[test]
    fn malformed_json_is_skipped_keeping_the_previous_status() {
        let (lines, status) = render_opencode_part(Some("assistant"), "{not json", None);
        assert_eq!(plain(&lines), ["· parse-skip"]);
        assert_eq!(status, None);

        let prev = PartStatus::Tool("running".into());
        let (_, kept) = render_opencode_part(Some("assistant"), "{not json", Some(&prev));
        assert_eq!(kept, Some(prev));
    }

    #[test]
    fn non_object_json_is_skipped() {
        for raw in ["[1,2,3]", "null", "true", "\"string\"", "12"] {
            let (lines, _) = render_opencode_part(Some("assistant"), raw, None);
            assert_eq!(plain(&lines), ["· parse-skip"], "raw: {raw}");
        }
    }

    #[test]
    fn hostile_shapes_render_without_panicking() {
        let hostile = [
            r#"{"type":"tool","tool":"x","state":"not a dict"}"#,
            r#"{"type":"tool","state":{"status":"completed"}}"#,
            r#"{"type":"tool","tool":42,"state":{"status":7}}"#,
            r#"{"type":"tool","tool":"x","state":{"status":"error","error":{"code":1}}}"#,
            r#"{"type":null}"#,
            r#"{"type":"text","text":null}"#,
            r#"{"type":"text","text":[1,2]}"#,
            "{}",
        ];
        for raw in hostile {
            let (_, status) = render_opencode_part(None, raw, None);
            render_opencode_part(Some("assistant"), raw, status.as_ref());
        }
    }

    #[test]
    fn tool_without_a_status_stays_unseen() {
        // No status to remember, so the Python keeps `prev_status` — the
        // row is treated as never-seen until a status appears.
        let data = r#"{"type":"tool","tool":"x","state":{}}"#;
        let (lines, status) = render_opencode_part(Some("assistant"), data, None);
        assert_eq!(plain(&lines), ["▶ x {}"]);
        assert_eq!(status, None);
    }
}
