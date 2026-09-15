use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use hermon::source::codex::CodexSource;
use hermon::source::{Attn, LastEvent, Liveness, Replay, Source, classify};
use serde_json::{Value, json};
use tempfile::TempDir;

fn put(root: &Path, name: &str, data: &str) -> std::path::PathBuf {
    let path = root.join("sessions/2026/09/15").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, data).unwrap();
    path
}
fn append(path: &Path, value: Value) {
    writeln!(
        OpenOptions::new().append(true).open(path).unwrap(),
        "{value}"
    )
    .unwrap();
}
fn event(kind: &str, turn: &str) -> Value {
    json!({"type":"event_msg", "payload":{"type":kind,"turn_id":turn}})
}
fn session(id: &str) -> String {
    format!(
        "{}\n",
        json!({"type":"session_meta","payload":{"id":id,"cwd":"/example"}})
    )
}

#[test]
fn nested_concurrent_trait_object_and_transport() {
    let dir = TempDir::new().unwrap();
    put(
        dir.path(),
        "a.jsonl",
        include_str!("fixtures/codex/mixed.jsonl"),
    );
    put(
        dir.path(),
        "b.jsonl",
        include_str!("fixtures/codex/events.jsonl"),
    );
    let mut source: Box<dyn Source> = Box::new(CodexSource::new(dir.path()));
    let sessions = source.sessions();
    assert_eq!(sessions.len(), 2);
    let a = sessions.iter().find(|s| s.id == "session-a").unwrap();
    assert_eq!(a.title, "Check the sample");
    assert_eq!(a.model, "gpt-6-astra");
    assert_eq!((a.in_tok, a.out_tok, a.cost), (100, 20, None));
    assert_eq!(a.last_event, Some(LastEvent::AssistantText));
    assert!(a.turn_done && !a.ended && !a.tool_pending && !a.force_live);
    assert_eq!(a.started_at, 1_789_466_400.0);
    assert!(a.last_ts >= a.started_at);
    assert_eq!(classify(a, a.last_ts, 60.0, 3600.0), Liveness::Done);
    assert_eq!(source.last_tool("session-a"), "read_file");
    assert_eq!(source.last_tool("missing"), "-");
    assert!(
        source
            .open_tailer("../../outside", Replay::default())
            .is_none()
    );
    assert_eq!(source.sessions(), sessions);
    let encoded = serde_json::to_string(a).unwrap();
    let mut decoded = serde_json::from_str::<hermon::source::SessionMeta>(&encoded).unwrap();
    assert!((decoded.last_ts - a.last_ts).abs() < 1e-6);
    decoded.last_ts = a.last_ts;
    assert_eq!(decoded, *a);
    let mut tail = source
        .open_tailer(
            "session-a",
            Replay {
                bytes: u64::MAX,
                rows: 0,
            },
        )
        .unwrap();
    for line in tail.poll() {
        let encoded = serde_json::to_string(&line).unwrap();
        assert_eq!(
            serde_json::from_str::<hermon::render::StyledLine>(&encoded).unwrap(),
            line
        );
    }
    assert_eq!(source.sessions(), sessions);
}

#[test]
fn id_model_title_fallbacks_and_deterministic_sort() {
    let dir = TempDir::new().unwrap();
    let id = "12345678-1234-1234-1234-123456789abc";
    let a = put(dir.path(), &format!("rollout-timestamp-{id}.jsonl"), "{}\n");
    let b = put(dir.path(), "b.jsonl", &session("b"));
    put(dir.path(), "not-a-uuid.jsonl", "{}\n");
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000);
    for p in [&a, &b] {
        fs::File::open(p).unwrap().set_modified(time).unwrap();
    }
    let mut s = CodexSource::new(dir.path());
    let rows = s.sessions();
    assert_eq!(
        rows.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["b", id]
    );
    assert_eq!(rows[0].title, "/example");
    assert_eq!(rows[1].model, "unknown");
    assert_eq!(rows[1].started_at, 1000.0);
    append(
        &b,
        json!({"type":"turn_context","payload":{"model":"first"}}),
    );
    append(
        &b,
        json!({"type":"turn_context","payload":{"model":"second"}}),
    );
    assert_eq!(
        s.sessions().iter().find(|m| m.id == "b").unwrap().model,
        "second"
    );
}

#[test]
fn correlated_tools_completion_reopen_and_delayed_completion() {
    let dir = TempDir::new().unwrap();
    let p = put(dir.path(), "a.jsonl", &session("a"));
    let mut s = CodexSource::new(dir.path());
    append(&p, event("task_started", "one"));
    for id in ["a", "b"] {
        append(
            &p,
            json!({"type":"response_item","payload":{"type":"custom_tool_call","call_id":id,"name":"exec_command","input":"test"}}),
        );
    }
    let m = s.sessions().remove(0);
    assert!(m.tool_pending);
    assert_eq!(
        classify(&m, m.last_ts + 31.0, 60.0, 3600.0),
        Liveness::Attention(Attn::PermWait)
    );
    assert_eq!(
        classify(&m, m.last_ts + 301.0, 60.0, 3600.0),
        Liveness::Attention(Attn::Stuck)
    );
    append(
        &p,
        json!({"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"a","output":"ok"}}),
    );
    let m = s.sessions().remove(0);
    assert!(m.tool_pending);
    assert_eq!(m.last_event, Some(LastEvent::ToolResult));
    append(&p, event("task_complete", "one"));
    let m = s.sessions().remove(0);
    assert!(m.turn_done && !m.tool_pending && !m.ended);
    append(&p, event("task_started", "two"));
    append(&p, event("task_complete", "one"));
    append(
        &p,
        json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"still working"}]}}),
    );
    let m = s.sessions().remove(0);
    assert!(!m.turn_done);
    assert_eq!(classify(&m, m.last_ts, 60.0, 3600.0), Liveness::Live);
    assert_eq!(classify(&m, m.last_ts + 61.0, 60.0, 3600.0), Liveness::Done);
    append(&p, event("task_complete", "two"));
    assert!(s.sessions()[0].turn_done);
    append(
        &p,
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"again"}]}}),
    );
    assert!(!s.sessions()[0].turn_done);
    append(&p, event("task_complete", "two"));
    assert!(!s.sessions()[0].turn_done);
}

#[test]
fn accounting_precedence_idempotence_and_foreign_threads() {
    let dir = TempDir::new().unwrap();
    let p = put(dir.path(), "a.jsonl", &session("a"));
    let mut s = CodexSource::new(dir.path());
    let usage = json!({"type":"token_usage_record","payload":{"thread_id":"a","response_id":"r","usage":{"input_tokens":10,"output_tokens":3,"cached_input_tokens":8,"reasoning_output_tokens":2}}});
    append(&p, usage.clone());
    append(&p, usage);
    assert_eq!((s.sessions()[0].in_tok, s.sessions()[0].out_tok), (10, 3));
    append(
        &p,
        json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":30}}}}),
    );
    assert_eq!(s.sessions()[0].in_tok, 100);
    append(
        &p,
        json!({"type":"token_usage_record","payload":{"thread_id":"a","thread_token_usage":{"input_tokens":200,"output_tokens":60},"turn_token_usage":{"input_tokens":1000,"output_tokens":999}}}),
    );
    append(
        &p,
        json!({"type":"token_usage_record","payload":{"thread_id":"child","thread_token_usage":{"input_tokens":9000,"output_tokens":9000}}}),
    );
    assert_eq!((s.sessions()[0].in_tok, s.sessions()[0].out_tok), (200, 60));
}

#[test]
fn missing_root_and_unreadable_neighbor() {
    let dir = TempDir::new().unwrap();
    assert!(
        CodexSource::new(dir.path().join("missing"))
            .sessions()
            .is_empty()
    );
    let p = put(dir.path(), "bad.jsonl", &session("bad"));
    put(dir.path(), "good.jsonl", &session("good"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o0)).unwrap();
    }
    let mut s = CodexSource::new(dir.path());
    assert!(s.sessions().iter().any(|m| m.id == "good"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn new_user_activity_without_task_started_invalidates_old_turn_completion() {
    let dir = TempDir::new().unwrap();
    let p = put(dir.path(), "a.jsonl", &session("a"));
    let mut s = CodexSource::new(dir.path());
    append(&p, event("task_started", "old"));
    for prompt in ["first", "next"] {
        append(
            &p,
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":prompt}]}}),
        );
        s.sessions();
    }
    append(&p, event("task_complete", "old"));
    let m = s.sessions().remove(0);
    assert!(!m.turn_done);
    assert_eq!(m.title, "first");
    assert_eq!(m.last_line, "next");
}
