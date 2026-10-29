//! Renders one Claude Code transcript line (`hermon.py:183
//! render_claude_line`).
//!
//! [`render_claude_line`] is pure and defensive by contract: it never
//! panics and never dumps raw JSON. Malformed input collapses to a single
//! dim `· parse-skip` line; a shape it doesn't recognise collapses to a
//! single dim `· <type>` line. Unlike the Python original, it does not wrap
//! text to a terminal width — that is the pane widget's job here (see the
//! module doc on [`crate::render`]), so text blocks become one logical line
//! with internal whitespace collapsed, and the caller wraps as needed.

use serde_json::{Map, Value};

use super::{Seg, Sem, StyledLine, clip};
use crate::source::claude::{ARG_CLIP, RESULT_CLIP, count, event_cost, result_text, usage_in};

/// Render one raw transcript line to zero or more display lines.
pub fn render_claude_line(raw: &str) -> Vec<StyledLine> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let ev = match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(ev)) => ev,
        _ => return vec![dim("· parse-skip")],
    };

    let etype = ev.get("type").and_then(Value::as_str);
    let empty_msg = Map::new();
    let msg = match ev.get("message") {
        Some(Value::Object(m)) => m,
        _ => &empty_msg,
    };
    let role = msg.get("role").and_then(Value::as_str);
    let content = msg.get("content");

    if etype == Some("assistant") || role == Some("assistant") {
        render_assistant(msg, content, &ev)
    } else if etype == Some("user") || role == Some("user") {
        render_user(content)
    } else if etype == Some("result") {
        vec![render_result(&ev)]
    } else {
        vec![dim(format!("· {}", etype.unwrap_or("unknown")))]
    }
}

fn render_assistant(
    msg: &Map<String, Value>,
    content: Option<&Value>,
    ev: &Map<String, Value>,
) -> Vec<StyledLine> {
    let mut out = Vec::new();
    if let Some(Value::Array(blocks)) = content {
        for block in blocks {
            let Some(block) = block.as_object() else {
                continue;
            };
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = collapse_ws(block.get("text").and_then(Value::as_str).unwrap_or(""));
                    if !text.is_empty() {
                        out.push(plain(text));
                    }
                }
                Some("tool_use") => {
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("?");
                    let input = block
                        .get("input")
                        .cloned()
                        .unwrap_or(Value::Object(Map::new()));
                    let arg = serde_json::to_string(&input).unwrap_or_default();
                    out.push(StyledLine(vec![
                        Seg::new(Sem::Tool, format!("▶ {name}")),
                        Seg::new(Sem::Plain, " "),
                        Seg::new(Sem::Dim, clip(&arg, ARG_CLIP)),
                    ]));
                }
                other => out.push(dim(format!("· {}", other.unwrap_or("unknown")))),
            }
        }
    }
    if let Some(Value::Object(usage)) = msg.get("usage")
        && !usage.is_empty()
    {
        out.push(stat_line(usage, ev, Sem::Stat));
    }
    out
}

fn render_user(content: Option<&Value>) -> Vec<StyledLine> {
    match content {
        Some(Value::String(text)) => vec![user_line(&collapse_ws(text))],
        Some(Value::Array(blocks)) => {
            let mut out = Vec::new();
            for block in blocks {
                let Some(block) = block.as_object() else {
                    continue;
                };
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_result") => {
                        let text = clip(&result_text(block.get("content")), RESULT_CLIP);
                        let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
                        out.push(if is_error {
                            StyledLine(vec![
                                Seg::new(Sem::Error, "◀ ERROR"),
                                Seg::new(Sem::Plain, " "),
                                Seg::new(Sem::Dim, text),
                            ])
                        } else {
                            dim(format!("◀ result {text}"))
                        });
                    }
                    Some("text") => {
                        let text =
                            collapse_ws(block.get("text").and_then(Value::as_str).unwrap_or(""));
                        if !text.is_empty() {
                            out.push(user_line(&text));
                        }
                    }
                    other => out.push(dim(format!("· {}", other.unwrap_or("unknown")))),
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

fn render_result(ev: &Map<String, Value>) -> StyledLine {
    let empty = Map::new();
    let usage = match ev.get("usage") {
        Some(Value::Object(u)) => u,
        _ => &empty,
    };
    stat_line(usage, ev, Sem::Ok)
}

/// The `Σ in:… out:… [$cost]` summary line shared by the per-message usage
/// block (`Sem::Stat`, cyan) and the terminal `result` event (`Sem::Ok`,
/// green) — `hermon.py:225` and `hermon.py:254` respectively.
fn stat_line(usage: &Map<String, Value>, ev: &Map<String, Value>, sem: Sem) -> StyledLine {
    let in_tok = usage_in(usage);
    let out_tok = count(usage.get("output_tokens"));
    let mut text = format!("Σ in:{} out:{}", thousands(in_tok), thousands(out_tok));
    if let Some(cost) = event_cost(ev) {
        text.push_str(&format!(" ${cost:.4}"));
    }
    StyledLine(vec![Seg::new(sem, text)])
}

fn user_line(collapsed_text: &str) -> StyledLine {
    StyledLine(vec![
        Seg::new(Sem::User, "» user:"),
        Seg::new(Sem::Plain, " "),
        Seg::new(Sem::Plain, collapsed_text.to_string()),
    ])
}

fn plain(text: impl Into<String>) -> StyledLine {
    StyledLine(vec![Seg::new(Sem::Plain, text)])
}

fn dim(text: impl Into<String>) -> StyledLine {
    StyledLine(vec![Seg::new(Sem::Dim, text)])
}

/// Collapse all whitespace runs (including embedded newlines) to single
/// spaces and trim the ends — what `textwrap.wrap` does to text before
/// wrapping it (`hermon.py:214`, `hermon.py:235`), minus the wrapping since
/// that is the pane widget's job here.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Thousands-grouped integer, matching Python's `f"{n:,}"` (`hermon.py:227`).
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().rev().enumerate() {
        if i != 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_of(lines: &[StyledLine]) -> Vec<String> {
        lines.iter().map(StyledLine::to_plain).collect()
    }

    fn sems_of(line: &StyledLine) -> Vec<Sem> {
        line.0.iter().map(|s| s.sem).collect()
    }

    fn assistant(content: Value, extra: Value) -> String {
        let mut msg = serde_json::json!({"role": "assistant", "content": content});
        if let Value::Object(map) = &extra {
            if let Some(usage) = map.get("usage") {
                msg["usage"] = usage.clone();
            }
            if let Some(model) = map.get("model") {
                msg["model"] = model.clone();
            }
        }
        let mut ev = serde_json::json!({"type": "assistant", "message": msg});
        if let Value::Object(map) = &extra {
            for k in ["total_cost_usd", "cost_usd", "costUSD"] {
                if let Some(v) = map.get(k) {
                    ev[k] = v.clone();
                }
            }
        }
        ev.to_string()
    }

    fn user(content: Value) -> String {
        serde_json::json!({"type": "user", "message": {"role": "user", "content": content}})
            .to_string()
    }

    // -------------------------------------------------------- shapes

    #[test]
    fn assistant_text_is_a_plain_line() {
        let line = assistant(
            serde_json::json!([{"type": "text", "text": "hello   world\n again"}]),
            Value::Null,
        );
        let out = render_claude_line(&line);
        assert_eq!(plain_of(&out), vec!["hello world again"]);
        assert_eq!(sems_of(&out[0]), vec![Sem::Plain]);
    }

    #[test]
    fn tool_use_shows_name_and_clipped_input() {
        let line = assistant(
            serde_json::json!([{"type": "tool_use", "name": "Bash", "input": {"command": "x".repeat(500)}}]),
            Value::Null,
        );
        let out = render_claude_line(&line);
        assert_eq!(out.len(), 1);
        let plain = out[0].to_plain();
        assert!(plain.starts_with("▶ Bash "));
        assert!(plain.contains('…'));
        assert_eq!(sems_of(&out[0]), vec![Sem::Tool, Sem::Plain, Sem::Dim]);
    }

    #[test]
    fn assistant_usage_produces_stat_line() {
        let line = assistant(
            serde_json::json!([{"type": "text", "text": "ok"}]),
            serde_json::json!({"usage": {"input_tokens": 1200, "output_tokens": 34, "cache_read_input_tokens": 800}}),
        );
        let out = render_claude_line(&line);
        let last = out.last().unwrap();
        assert_eq!(last.to_plain(), "Σ in:2,000 out:34");
        assert_eq!(sems_of(last), vec![Sem::Stat]);
    }

    #[test]
    fn assistant_usage_with_cost_appends_dollar_amount() {
        let line = assistant(
            serde_json::json!([{"type": "text", "text": "ok"}]),
            serde_json::json!({"usage": {"input_tokens": 1, "output_tokens": 1}, "costUSD": 0.5}),
        );
        let out = render_claude_line(&line);
        assert_eq!(out.last().unwrap().to_plain(), "Σ in:1 out:1 $0.5000");
    }

    #[test]
    fn empty_usage_object_yields_no_stat_line() {
        let line = assistant(
            serde_json::json!([{"type": "text", "text": "ok"}]),
            serde_json::json!({"usage": {}}),
        );
        let out = render_claude_line(&line);
        assert_eq!(plain_of(&out), vec!["ok"]);
    }

    #[test]
    fn unknown_assistant_block_type_is_dim_marker() {
        let line = assistant(
            serde_json::json!([{"type": "thinking", "thinking": "hmm"}]),
            Value::Null,
        );
        let out = render_claude_line(&line);
        assert_eq!(plain_of(&out), vec!["· thinking"]);
        assert_eq!(sems_of(&out[0]), vec![Sem::Dim]);
    }

    #[test]
    fn non_dict_assistant_blocks_are_skipped() {
        let line = assistant(serde_json::json!([null, 7, "x"]), Value::Null);
        assert_eq!(render_claude_line(&line), Vec::new());
    }

    #[test]
    fn user_string_prompt_gets_prefix() {
        let line = user(serde_json::json!("do  the\nthing"));
        let out = render_claude_line(&line);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].to_plain(), "» user: do the thing");
        assert_eq!(sems_of(&out[0]), vec![Sem::User, Sem::Plain, Sem::Plain]);
    }

    #[test]
    fn user_empty_string_still_emits_prefix_line() {
        let line = user(serde_json::json!(""));
        let out = render_claude_line(&line);
        assert_eq!(plain_of(&out), vec!["» user: "]);
    }

    #[test]
    fn user_text_block_gets_prefix() {
        let line = user(serde_json::json!([{"type": "text", "text": "hi there"}]));
        let out = render_claude_line(&line);
        assert_eq!(plain_of(&out), vec!["» user: hi there"]);
    }

    #[test]
    fn user_blank_text_block_yields_nothing() {
        let line = user(serde_json::json!([{"type": "text", "text": "   "}]));
        assert_eq!(render_claude_line(&line), Vec::new());
    }

    #[test]
    fn tool_result_string_content() {
        let line = user(serde_json::json!([{"type": "tool_result", "content": "y".repeat(500)}]));
        let out = render_claude_line(&line);
        assert_eq!(out.len(), 1);
        let plain = out[0].to_plain();
        assert!(plain.starts_with("◀ result "));
        assert!(plain.contains('…'));
        assert_eq!(sems_of(&out[0]), vec![Sem::Dim]);
    }

    #[test]
    fn tool_result_block_list_content() {
        let line = user(
            serde_json::json!([{"type": "tool_result", "content": [{"type": "text", "text": "block text"}]}]),
        );
        let out = render_claude_line(&line);
        assert_eq!(out[0].to_plain(), "◀ result block text");
    }

    #[test]
    fn tool_result_error_marker() {
        let line =
            user(serde_json::json!([{"type": "tool_result", "content": "boom", "is_error": true}]));
        let out = render_claude_line(&line);
        assert_eq!(out[0].to_plain(), "◀ ERROR boom");
        assert_eq!(sems_of(&out[0]), vec![Sem::Error, Sem::Plain, Sem::Dim]);
    }

    #[test]
    fn unknown_user_block_type_is_dim_marker() {
        let line = user(serde_json::json!([{"type": "image"}]));
        let out = render_claude_line(&line);
        assert_eq!(plain_of(&out), vec!["· image"]);
    }

    #[test]
    fn result_event_with_cost() {
        let line = serde_json::json!({
            "type": "result",
            "total_cost_usd": 0.1234,
            "usage": {"input_tokens": 10, "output_tokens": 5},
        })
        .to_string();
        let out = render_claude_line(&line);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].to_plain(), "Σ in:10 out:5 $0.1234");
        assert_eq!(sems_of(&out[0]), vec![Sem::Ok]);
    }

    #[test]
    fn result_event_without_usage_defaults_to_zero() {
        let line = serde_json::json!({"type": "result"}).to_string();
        let out = render_claude_line(&line);
        assert_eq!(out[0].to_plain(), "Σ in:0 out:0");
    }

    #[test]
    fn unknown_event_type_single_dim_line() {
        let line = serde_json::json!({"type": "wormhole", "junk": [1, 2]}).to_string();
        assert_eq!(plain_of(&render_claude_line(&line)), vec!["· wormhole"]);
    }

    #[test]
    fn missing_type_reads_as_unknown() {
        assert_eq!(plain_of(&render_claude_line("{}")), vec!["· unknown"]);
    }

    #[test]
    fn malformed_json_is_skipped_not_raised() {
        assert_eq!(
            plain_of(&render_claude_line("{not json")),
            vec!["· parse-skip"]
        );
    }

    #[test]
    fn non_object_json_is_skipped() {
        for line in ["[1, 2, 3]", "null", "true", "\"string\"", "12"] {
            assert_eq!(
                plain_of(&render_claude_line(line)),
                vec!["· parse-skip"],
                "line: {line}"
            );
        }
    }

    #[test]
    fn blank_line_yields_nothing() {
        assert_eq!(render_claude_line("   \n"), Vec::new());
        assert_eq!(render_claude_line(""), Vec::new());
    }

    #[test]
    fn survives_repeated_empty_object_lines() {
        for _ in 0..50 {
            assert_eq!(plain_of(&render_claude_line("{}")), vec!["· unknown"]);
        }
    }

    // -------------------------------------------------------- hostile shapes
    // Mirrors tests/test_render.py::test_never_raises_on_hostile_shapes.

    #[test]
    fn never_panics_on_hostile_shapes() {
        let hostile = [
            serde_json::json!({"type": "assistant", "message": "not a dict"}).to_string(),
            serde_json::json!({"type": "assistant", "message": {"content": 42}}).to_string(),
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [null, 7, "x"]}})
                .to_string(),
            serde_json::json!({"type": "user", "message": {"role": "user", "content": {"weird": true}}}).to_string(),
            serde_json::json!({"type": "result", "usage": "nope", "total_cost_usd": "free"}).to_string(),
            "null".to_string(),
            "true".to_string(),
            "\"string\"".to_string(),
            "12".to_string(),
        ];
        for line in hostile {
            let _ = render_claude_line(&line);
        }
    }

    #[test]
    fn hostile_assistant_message_not_a_dict_yields_nothing() {
        let line = serde_json::json!({"type": "assistant", "message": "not a dict"}).to_string();
        assert_eq!(render_claude_line(&line), Vec::new());
    }

    #[test]
    fn hostile_user_content_object_yields_nothing() {
        let line = serde_json::json!({"type": "user", "message": {"role": "user", "content": {"weird": true}}}).to_string();
        assert_eq!(render_claude_line(&line), Vec::new());
    }

    #[test]
    fn hostile_result_with_non_numeric_fields_defaults_safely() {
        let line = serde_json::json!({"type": "result", "usage": "nope", "total_cost_usd": "free"})
            .to_string();
        let out = render_claude_line(&line);
        assert_eq!(out[0].to_plain(), "Σ in:0 out:0");
    }

    // -------------------------------------------------------- helpers

    #[test]
    fn thousands_groups_by_three() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(10), "10");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(2_000), "2,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
