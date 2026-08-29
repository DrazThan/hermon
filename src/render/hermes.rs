//! Renders Hermes `messages` table rows to displayable lines.
//!
//! Port of `hermon.py:722 render_hermes_row`. Defensive by contract: bad
//! JSON in `tool_calls`/`content` degrades to a skipped part or a plain-text
//! fallback, never a panic — mirrors `tests/test_hermes.py:103`'s hostile
//! shapes.

use serde_json::Value;

use crate::render::{Seg, Sem, StyledLine, clip};

/// Tool arguments are clipped at 120 chars, tool results at 200
/// (`hermon.py:744`, `hermon.py:760`).
const ARG_CLIP: usize = 120;
const RESULT_CLIP: usize = 200;

/// `Some(s)` only when `s` is non-empty, mirroring Python's `x or default`
/// truthiness for strings pulled out of loosely-shaped JSON.
fn truthy_str(v: Option<&str>) -> Option<&str> {
    v.filter(|s| !s.is_empty())
}

/// One `assistant` tool-call entry rendered to a line, or `None` if `call`
/// isn't a JSON object (`hermon.py:738` skips non-dict entries silently).
fn render_tool_call(call: &Value) -> Option<StyledLine> {
    let call_obj = call.as_object()?;
    let func = call_obj.get("function").and_then(|f| f.as_object());
    let name = truthy_str(func.and_then(|f| f.get("name")).and_then(|v| v.as_str()))
        .or_else(|| truthy_str(call_obj.get("name").and_then(|v| v.as_str())))
        .unwrap_or("?");
    let args = truthy_str(
        func.and_then(|f| f.get("arguments"))
            .and_then(|v| v.as_str()),
    )
    .unwrap_or("");
    Some(StyledLine(vec![
        Seg::new(Sem::Bold, format!("▶ {name}")),
        Seg::new(Sem::Plain, " "),
        Seg::new(Sem::Dim, clip(args, ARG_CLIP)),
    ]))
}

/// Renders one Hermes `messages` row (`hermon.py:722`). Never panics: bad
/// JSON in `content`/`tool_calls` degrades to a fallback or is skipped.
pub fn render_hermes_row(
    role: Option<&str>,
    content: Option<&str>,
    tool_calls: Option<&str>,
    tool_name: Option<&str>,
) -> Vec<StyledLine> {
    let mut out = Vec::new();
    let role = role.unwrap_or("?");

    match role {
        "assistant" => {
            if let Some(text) = content
                && !text.trim().is_empty()
            {
                out.push(StyledLine(vec![Seg::new(Sem::Plain, text.trim())]));
            }
            if let Some(raw) = tool_calls
                && let Ok(Value::Array(calls)) = serde_json::from_str::<Value>(raw)
            {
                out.extend(calls.iter().filter_map(render_tool_call));
            }
        }
        "tool" => {
            let raw = content.unwrap_or("");
            let mut text = raw.to_string();
            let mut is_error = false;
            if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(raw) {
                let err = obj.get("error").filter(|v| !v.is_null());
                let code = obj.get("exit_code");
                let code_is_zero_or_none = matches!(code, None | Some(Value::Null))
                    || code.and_then(|v| v.as_i64()) == Some(0);
                is_error =
                    err.is_some_and(|v| !matches!(v, Value::Bool(false))) || !code_is_zero_or_none;
                text = if let Some(err) = err {
                    match err {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    }
                } else {
                    obj.get("output")
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_else(|| raw.to_string())
                };
            }
            out.push(if is_error {
                StyledLine(vec![
                    Seg::new(Sem::Error, "◀ ERROR"),
                    Seg::new(Sem::Plain, " "),
                    Seg::new(Sem::Dim, clip(&text, RESULT_CLIP)),
                ])
            } else {
                StyledLine(vec![Seg::new(
                    Sem::Dim,
                    format!(
                        "◀ {} {}",
                        tool_name.unwrap_or("result"),
                        clip(&text, RESULT_CLIP)
                    ),
                )])
            });
        }
        "user" => {
            if let Some(text) = content
                && !text.trim().is_empty()
            {
                out.push(StyledLine(vec![
                    Seg::new(Sem::User, "» user:"),
                    Seg::new(Sem::Plain, format!(" {}", text.trim())),
                ]));
            }
        }
        "system" => out.push(StyledLine(vec![Seg::new(Sem::Dim, "· system")])),
        other => out.push(StyledLine(vec![Seg::new(Sem::Dim, format!("· {other}"))])),
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[StyledLine]) -> Vec<String> {
        lines.iter().map(|l| l.to_plain()).collect()
    }

    #[test]
    fn assistant_text() {
        let out = render_hermes_row(Some("assistant"), Some("hello world"), None, None);
        assert_eq!(plain(&out), vec!["hello world"]);
    }

    #[test]
    fn assistant_tool_calls_openai_shape() {
        let args = serde_json::to_string(
            &serde_json::json!({"command": format!("ls {}", "x".repeat(300))}),
        )
        .unwrap();
        let calls = serde_json::to_string(&serde_json::json!([{
            "id": "toolu_x", "type": "function",
            "function": {"name": "terminal", "arguments": args},
        }]))
        .unwrap();
        let out = render_hermes_row(Some("assistant"), None, Some(&calls), None);
        assert_eq!(out.len(), 1);
        let line = out[0].to_plain();
        assert!(line.contains("▶ terminal"), "{line}");
        assert!(line.contains('…'), "{line}");
        assert!(line.len() < 200, "{line}");
    }

    #[test]
    fn tool_result_json_output_extracted() {
        let content =
            serde_json::json!({"output": "OK-SMOKE", "exit_code": 0, "error": null}).to_string();
        let out = render_hermes_row(Some("tool"), Some(&content), None, Some("terminal"));
        assert_eq!(out.len(), 1);
        let line = out[0].to_plain();
        assert!(line.contains("◀ terminal"), "{line}");
        assert!(line.contains("OK-SMOKE"), "{line}");
        assert!(!line.contains("exit_code"), "{line}");
    }

    #[test]
    fn tool_result_error_via_exit_code() {
        let content =
            serde_json::json!({"output": "boom", "exit_code": 1, "error": null}).to_string();
        let out = render_hermes_row(Some("tool"), Some(&content), None, Some("terminal"));
        assert!(out[0].to_plain().contains("◀ ERROR"));
    }

    #[test]
    fn tool_result_error_via_error_field() {
        let content =
            serde_json::json!({"output": null, "exit_code": 0, "error": "timeout"}).to_string();
        let out = render_hermes_row(Some("tool"), Some(&content), None, Some("web"));
        let line = out[0].to_plain();
        assert!(line.contains("◀ ERROR"), "{line}");
        assert!(line.contains("timeout"), "{line}");
    }

    #[test]
    fn tool_result_plain_text_content() {
        let out = render_hermes_row(Some("tool"), Some("not json at all"), None, Some("search"));
        let line = out[0].to_plain();
        assert!(line.contains("◀ search"), "{line}");
        assert!(line.contains("not json at all"), "{line}");
    }

    #[test]
    fn user_prompt() {
        let out = render_hermes_row(Some("user"), Some("run the tests"), None, None);
        let line = out[0].to_plain();
        assert!(line.contains("» user:"), "{line}");
        assert!(line.contains("run the tests"), "{line}");
    }

    #[test]
    fn unknown_role_dim_marker() {
        let out = render_hermes_row(Some("developer"), Some("x"), None, None);
        assert_eq!(plain(&out), vec!["· developer"]);
    }

    #[test]
    fn missing_role_falls_back_to_question_mark() {
        let out = render_hermes_row(None, None, None, None);
        assert_eq!(plain(&out), vec!["· ?"]);
    }

    #[test]
    fn never_panics_on_hostile_rows() {
        type Row<'a> = (
            Option<&'a str>,
            Option<&'a str>,
            Option<&'a str>,
            Option<&'a str>,
        );
        let hostile: Vec<Row> = vec![
            (Some("assistant"), None, Some("{broken json"), None),
            (
                Some("assistant"),
                None,
                Some(r#"[null, 42, {"function": "str"}]"#),
                None,
            ),
            (Some("tool"), None, None, None),
            (None, None, None, None),
            (Some("user"), None, None, None),
            (Some("tool"), Some(r#"{"weird": true}"#), None, None),
        ];
        for (role, content, tool_calls, tool_name) in hostile {
            render_hermes_row(role, content, tool_calls, tool_name); // must not panic
        }
    }
}
