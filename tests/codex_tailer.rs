use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use hermon::source::codex::CodexSource;
use hermon::source::{Replay, Source, Tailer};
use serde_json::{Value, json};
use tempfile::TempDir;

const ALL: Replay = Replay {
    bytes: u64::MAX,
    rows: 0,
};
fn raw(value: Value) -> String {
    format!("{value}\n")
}
fn message(text: &str) -> String {
    raw(
        json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}}),
    )
}
fn append(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}
fn setup(data: &str) -> (TempDir, PathBuf, CodexSource) {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("sessions")).unwrap();
    let path = dir.path().join("sessions/a.jsonl");
    fs::write(
        &path,
        raw(json!({"type":"session_meta","payload":{"id":"a"}})) + data,
    )
    .unwrap();
    let mut source = CodexSource::new(dir.path());
    source.sessions();
    (dir, path, source)
}
fn plain(tail: &mut dyn Tailer) -> Vec<String> {
    tail.poll().iter().map(|l| l.to_plain()).collect()
}

#[test]
fn mixed_and_event_only_fixtures_have_no_duplicate_messages() {
    for (fixture, id, expected) in [
        (
            include_str!("fixtures/codex/mixed.jsonl"),
            "session-a",
            vec![
                "Check the sample",
                "▶ exec_command echo sample",
                "▶ read_file sample.txt",
                "  sample",
                "The sample is ready.",
            ],
        ),
        (
            include_str!("fixtures/codex/events.jsonl"),
            "session-b",
            vec![
                "Repeat a word",
                "▶ command echo example · example",
                "example",
            ],
        ),
    ] {
        let (_dir, _path, s) = setup(fixture);
        let mut tail = s.open_tailer(id, ALL).unwrap();
        assert_eq!(plain(&mut *tail), expected);
        assert!(tail.poll().is_empty());
    }
}

#[test]
fn multibyte_byte_at_a_time_and_partial_lines() {
    let (_dir, path, s) = setup("");
    let mut t = s.open_tailer("a", ALL).unwrap();
    assert!(t.poll().is_empty());
    let data = message("café 🦀");
    for b in data.as_bytes().iter().take(data.len() - 1) {
        append(&path, &[*b]);
        assert!(t.poll().is_empty());
    }
    append(&path, b"\n");
    assert_eq!(plain(&mut *t), vec!["café 🦀"]);
    assert!(t.poll().is_empty());
}

#[test]
fn replay_zero_partial_exact_boundary_and_prefix_reconciliation() {
    let before = message("before");
    let after = message("after");
    let (_dir, path, mut s) = setup(&(before + &after));
    let mut zero = s
        .open_tailer(
            "a",
            Replay {
                bytes: 0,
                rows: 1000,
            },
        )
        .unwrap();
    assert!(zero.poll().is_empty());
    let mut partial = s
        .open_tailer(
            "a",
            Replay {
                bytes: (after.len() / 2) as u64,
                rows: 0,
            },
        )
        .unwrap();
    assert!(partial.poll().is_empty());
    let mut exact = s
        .open_tailer(
            "a",
            Replay {
                bytes: after.len() as u64,
                rows: 0,
            },
        )
        .unwrap();
    assert_eq!(plain(&mut *exact), vec!["after"]);
    append(&path, message("fresh").as_bytes());
    for t in [&mut zero, &mut partial, &mut exact] {
        assert_eq!(plain(&mut **t), vec!["fresh"]);
        assert!(t.poll().is_empty());
    }
    let duplicate = raw(
        json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","text":"fresh"}}}),
    );
    append(&path, duplicate.as_bytes());
    s.sessions();
    let mut mid = s
        .open_tailer(
            "a",
            Replay {
                bytes: duplicate.len() as u64,
                rows: 0,
            },
        )
        .unwrap();
    assert!(
        mid.poll().is_empty(),
        "prefix canonical stream suppresses duplicate in replay window"
    );
}

#[test]
fn replay_zero_skips_an_existing_incomplete_record() {
    let data = message("old incomplete");
    let (_dir, path, s) = setup(&data[..data.len() - 1]);
    let mut t = s.open_tailer("a", Replay { bytes: 0, rows: 0 }).unwrap();
    assert!(t.poll().is_empty());
    append(&path, b"\n");
    append(&path, message("new").as_bytes());
    assert_eq!(plain(&mut *t), vec!["new"]);
}

#[test]
fn malformed_unknown_oversized_and_sanitized_records_recover() {
    let (_dir, path, s) = setup("");
    let mut t = s.open_tailer("a", ALL).unwrap();
    t.poll();
    append(&path, b"bad\nbad\n");
    assert_eq!(plain(&mut *t), vec!["· parse-skip"]);
    append(&path, b"bad\n");
    assert!(t.poll().is_empty());
    append(&path, b"{\"type\":\"future_record\"}\n");
    assert!(t.poll().is_empty());
    append(&path, &vec![b'x'; 1024 * 1024 + 100]);
    assert_eq!(plain(&mut *t), vec!["· oversized record skipped"]);
    append(&path, b"\n");
    append(&path, message("safe\u{1b}[31m text").as_bytes());
    let out = plain(&mut *t);
    assert_eq!(out, vec!["safe�[31m text"]);
    append(&path, b"broken\n");
    assert_eq!(plain(&mut *t), vec!["· parse-skip"]);
}

#[test]
fn equal_size_replacement_truncation_and_delete_recreate() {
    let (_dir, path, mut s) = setup(&message("before"));
    let original = fs::read_to_string(&path).unwrap();
    let mut t = s.open_tailer("a", ALL).unwrap();
    assert_eq!(plain(&mut *t), vec!["before"]);
    fs::write(&path, original.replace("before", "after!")).unwrap();
    let out = plain(&mut *t);
    assert_eq!(out.len(), 2);
    assert_eq!(out[1], "after!");
    assert_eq!(s.sessions()[0].last_line, "after!");
    let replacement = path.with_extension("replacement");
    fs::write(&replacement, original.replace("before", "other!")).unwrap();
    fs::rename(&replacement, &path).unwrap();
    assert_eq!(plain(&mut *t)[1], "other!");
    fs::write(&path, "").unwrap();
    assert!(plain(&mut *t)[0].contains("restarting"));
    append(&path, message("short").as_bytes());
    assert_eq!(plain(&mut *t), vec!["short"]);
    fs::remove_file(&path).unwrap();
    assert!(plain(&mut *t)[0].contains("unavailable"));
    assert!(t.poll().is_empty());
    assert!(s.sessions().is_empty());
    fs::write(&path, original).unwrap();
    assert_eq!(plain(&mut *t)[1], "before");
    assert_eq!(s.sessions()[0].id, "a");
}

#[test]
fn event_stream_selection_ids_and_repeated_text_across_turns() {
    let (_dir, path, s) = setup("");
    let mut t = s.open_tailer("a", ALL).unwrap();
    t.poll();
    let event = raw(
        json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"answer","text":"same"}}}),
    );
    append(&path, event.as_bytes());
    assert_eq!(plain(&mut *t), vec!["same"]);
    append(&path, message("same").as_bytes());
    append(&path, event.as_bytes());
    assert!(t.poll().is_empty());
    append(&path,raw(json!({"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"same"}})).as_bytes());
    assert!(t.poll().is_empty());
    append(
        &path,
        raw(json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"next"}}))
            .as_bytes(),
    );
    append(&path, event.as_bytes());
    assert_eq!(plain(&mut *t), vec!["same"]);
    // No IDs: repeated text within the chosen stream is legitimate too.
    let anonymous = raw(
        json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","text":"same"}}}),
    );
    append(&path, anonymous.repeat(2).as_bytes());
    assert_eq!(plain(&mut *t), vec!["same", "same"]);
}

#[test]
fn completion_text_is_fallback_only() {
    let (_dir, path, s) = setup("");
    let mut t = s.open_tailer("a", ALL).unwrap();
    t.poll();
    let done = raw(
        json!({"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"fallback"}}),
    );
    append(&path, done.repeat(2).as_bytes());
    assert_eq!(plain(&mut *t), vec!["fallback"]);
    append(&path, done.as_bytes());
    assert!(t.poll().is_empty());
}

#[cfg(unix)]
#[test]
fn symlink_files_directories_and_post_discovery_redirection_are_rejected() {
    use std::os::unix::fs::symlink;
    let (dir, path, mut s) = setup(&message("safe"));
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(
        outside.join("secret.jsonl"),
        raw(json!({"type":"session_meta","payload":{"id":"secret"}})),
    )
    .unwrap();
    symlink(&outside, dir.path().join("sessions/escape")).unwrap();
    symlink(
        outside.join("secret.jsonl"),
        dir.path().join("sessions/secret.jsonl"),
    )
    .unwrap();
    assert_eq!(s.sessions().len(), 1);
    let mut t = s.open_tailer("a", ALL).unwrap();
    t.poll();
    fs::remove_file(&path).unwrap();
    symlink(outside.join("secret.jsonl"), &path).unwrap();
    assert!(plain(&mut *t)[0].contains("unavailable"));
    assert!(s.sessions().is_empty());
    fs::remove_file(&path).unwrap();
    fs::write(&path, message("recovered")).unwrap();
    assert_eq!(plain(&mut *t)[1], "recovered");
}

#[test]
fn deferred_user_does_not_overwrite_later_assistant_or_tool_metadata() {
    let user = raw(
        json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"u","text":"question"}}}),
    );
    let (_dir, _path, mut s) = setup(&(user + &message("answer")));
    let mut t = s.open_tailer("a", ALL).unwrap();
    assert_eq!(plain(&mut *t), vec!["question", "answer"]);
    let m = s.sessions().remove(0);
    assert_eq!(m.last_event, Some(hermon::source::LastEvent::AssistantText));
    assert_eq!(m.last_line, "answer");
}

#[test]
fn canonical_tools_suppress_completed_command_duplicates_in_either_order() {
    let command = raw(
        json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"call","command":"echo example","aggregated_output":"example"}}}),
    );
    let call = raw(
        json!({"type":"response_item","payload":{"type":"custom_tool_call","call_id":"call","name":"exec_command","input":"echo example"}}),
    );
    let result = raw(
        json!({"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call","output":"example"}}),
    );
    for data in [command.clone() + &call + &result, call + &result + &command] {
        let (_dir, _path, s) = setup(&data);
        let mut t = s.open_tailer("a", ALL).unwrap();
        assert_eq!(
            plain(&mut *t),
            vec!["▶ exec_command echo example", "  example"]
        );
    }
}
