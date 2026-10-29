use hermon::source::{Attn, LastEvent, Liveness, Replay, Source, classify, grok::GrokSource};
use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;
fn setup(root: &Path, parent: &str, id: &str) -> std::path::PathBuf {
    let p = root.join("sessions").join(parent).join(id);
    fs::create_dir_all(&p).unwrap();
    p
}
fn append(p: &Path, s: &str) {
    fs::OpenOptions::new()
        .append(true)
        .open(p)
        .unwrap()
        .write_all(s.as_bytes())
        .unwrap();
}
#[test]
fn observed_shapes_accounting_events_and_transport() {
    let root = TempDir::new().unwrap();
    let p = setup(root.path(), "%2Fwork%2Fdemo", "directory-id");
    fs::write(
        p.join("summary.json"),
        include_str!("fixtures/grok/summary.json"),
    )
    .unwrap();
    fs::write(
        p.join("usage.json"),
        include_str!("fixtures/grok/usage.json"),
    )
    .unwrap();
    fs::write(
        p.join("chat_history.jsonl"),
        include_str!("fixtures/grok/chat_history.jsonl"),
    )
    .unwrap();
    let mut source: Box<dyn Source> = Box::new(GrokSource::new(root.path()));
    let first = source.sessions().remove(0);
    assert_eq!(first.id, "nested-session");
    assert_eq!(first.title, "Demo build");
    assert_eq!(first.model, "grok-4.6-build");
    assert_eq!((first.in_tok, first.out_tok, first.cost), (100, 30, None));
    assert_eq!(first.started_at, 1789466400.0);
    assert!(first.tool_pending);
    assert_eq!(first.last_event, Some(LastEvent::ToolResult));
    assert!(!first.turn_done && !first.ended && !first.force_live);
    assert_eq!(source.sessions()[0], first);
    assert_eq!(
        serde_json::from_str::<hermon::source::SessionMeta>(
            &serde_json::to_string(&first).unwrap()
        )
        .unwrap(),
        first
    );
    append(
        &p.join("chat_history.jsonl"),
        "{\"type\":\"tool_result\",\"tool_call_id\":\"unknown\",\"content\":\"ok\"}\n",
    );
    assert!(source.sessions()[0].tool_pending);
    append(
        &p.join("chat_history.jsonl"),
        "{\"type\":\"tool_result\",\"tool_call_id\":\"call-b\",\"content\":\"ok\"}\n",
    );
    assert!(!source.sessions()[0].tool_pending);
    fs::write(p.join("summary.json"), "{").unwrap();
    fs::write(p.join("usage.json"), "{").unwrap();
    let retained = source.sessions().remove(0);
    assert_eq!(retained.title, first.title);
    assert_eq!(retained.in_tok, 100);
    fs::remove_file(p.join("usage.json")).unwrap();
    assert_eq!(source.sessions()[0].in_tok, 100);
    fs::write(
        p.join("replacement"),
        r#"{"session":{"inputTokens":150,"outputTokens":40}}"#,
    )
    .unwrap();
    fs::rename(p.join("replacement"), p.join("usage.json")).unwrap();
    assert_eq!(source.sessions()[0].in_tok, 150);
    assert!(
        source
            .open_tailer("../../arbitrary", Replay::default())
            .is_none()
    );
}
#[test]
fn fallbacks_neighbors_and_classification() {
    let root = TempDir::new().unwrap();
    let mut source = GrokSource::new(root.path());
    assert!(source.sessions().is_empty());
    let p = setup(root.path(), "%2Ftmp%2F%E9%9B%AA+%ZZ%252F", "fallback");
    fs::write(p.join("chat_history.jsonl"), "{\"type\":\"assistant\",\"tool_calls\":[{\"id\":\"a\",\"name\":\"shell\",\"arguments\":{}}]}\n{\"type\":\"user\",\"synthetic_reason\":\"context\",\"content\":\"not human\"}\n{\"type\":\"unknown\",\"tool_calls\":[{\"id\":\"bad\",\"name\":\"bad\"}]}\n").unwrap();
    let q = setup(root.path(), "%2Fother", "second");
    fs::write(q.join("summary.json"), r#"{"id":"flat","cwd":"/flat","current_model_id":"fallback-model","created_at":"invalid","updated_at":"2020-01-01T00:00:00Z","generated_title":" "}"#).unwrap();
    fs::write(
        q.join("chat_history.jsonl"),
        "{\"type\":\"user\",\"content\":\"hello\"}\n",
    )
    .unwrap();
    let sessions = source.sessions();
    assert_eq!(sessions.len(), 2);
    let s = sessions.iter().find(|s| s.id == "fallback").unwrap();
    assert_eq!(s.title, "/tmp/雪+%ZZ%2F");
    assert_eq!(s.last_event, Some(LastEvent::ToolUse("shell".into())));
    assert_eq!(classify(s, s.last_ts, 180., 3600.), Liveness::Live);
    assert_eq!(
        classify(s, s.last_ts + 31., 180., 3600.),
        Liveness::Attention(Attn::PermWait)
    );
    assert_eq!(
        classify(s, s.last_ts + 901., 180., 3600.),
        Liveness::Attention(Attn::Stuck)
    );
    assert_eq!(classify(s, s.last_ts + 3601., 180., 3600.), Liveness::Done);
    let s = sessions.iter().find(|s| s.id == "flat").unwrap();
    assert_eq!(s.title, "/flat");
    assert_eq!(s.model, "fallback-model");
    assert!(s.last_ts > 1700000000.);
    assert_eq!(classify(s, s.last_ts + 181., 180., 3600.), Liveness::Done);
    assert_eq!(source.last_tool("fallback"), "shell");
    assert_eq!(source.last_tool("unknown"), "-");
}
#[cfg(unix)]
#[test]
fn symlinks_are_not_followed() {
    use std::os::unix::fs::symlink;
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let p = setup(root.path(), "project", "session");
    fs::write(
        outside.path().join("chat"),
        "{\"type\":\"user\",\"content\":\"secret\"}\n",
    )
    .unwrap();
    symlink(outside.path().join("chat"), p.join("chat_history.jsonl")).unwrap();
    symlink(outside.path(), root.path().join("sessions/escape")).unwrap();
    let mut src = GrokSource::new(root.path());
    let s = src.sessions();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].last_event, None);
    let mut tail = src.open_tailer("session", Replay::default()).unwrap();
    assert!(!tail.poll().iter().any(|l| l.to_plain().contains("secret")));
}

#[test]
fn transcript_rewrite_resets_pending_and_partial_usage_retains_totals() {
    let root = TempDir::new().unwrap();
    let p = setup(root.path(), "cwd", "id");
    fs::write(
        p.join("chat_history.jsonl"),
        include_str!("fixtures/grok/chat_history.jsonl"),
    )
    .unwrap();
    fs::write(
        p.join("usage.json"),
        include_str!("fixtures/grok/usage.json"),
    )
    .unwrap();
    let mut src = GrokSource::new(root.path());
    assert!(src.sessions()[0].tool_pending);
    fs::write(
        p.join("chat_history.jsonl"),
        "{\"type\":\"user\",\"content\":\"new conversation\"}\n",
    )
    .unwrap();
    fs::write(p.join("usage.json"), "{\"session\":{}}").unwrap();
    let s = src.sessions().remove(0);
    assert!(!s.tool_pending);
    assert_eq!(s.last_event, Some(LastEvent::User));
    assert_eq!(s.last_tool, "-");
    assert_eq!(s.model, "?");
    assert_eq!(s.in_tok, 100);
    fs::write(p.join("summary.json"), r#"{"info":{"id":"nested","cwd":"/nested"},"id":"flat","cwd":"/flat","generated_title":"\u001btitle","last_active_at":"2030-01-01T00:00:00Z","updated_at":"invalid"}"#).unwrap();
    let s = src.sessions().remove(0);
    assert_eq!(s.id, "nested");
    assert!(!s.title.contains('\u{1b}'));
    assert_eq!(s.last_ts, 1893456000.0);
}
