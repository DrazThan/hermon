//! GeminiTailer acceptance tests (#106): replay budgets, reconstruction,
//! revision notices, and file-level recovery.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use hermon::source::gemini::{GeminiSource, GeminiTailer};
use hermon::source::{Replay, Source, Tailer};

const NO_REPLAY: Replay = Replay { bytes: 0, rows: 0 };
const HUGE: Replay = Replay {
    bytes: 1_000_000,
    rows: 0,
};

fn gemini_home() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().to_path_buf();
    fs::create_dir_all(home.join("tmp")).expect("tmp");
    (dir, home)
}

fn write_session(home: &Path, filename: &str, body: &str) -> PathBuf {
    let chats = home.join("tmp").join("demo").join("chats");
    fs::create_dir_all(&chats).expect("chats");
    let path = chats.join(filename);
    fs::write(&path, body).expect("write");
    path
}

fn header(session: &str, ts: &str) -> String {
    format!(
        r#"{{"sessionId":"{session}","projectHash":"abc","startTime":"{ts}","lastUpdated":"{ts}","kind":"main"}}"#
    )
}

fn user_msg(id: &str, ts: &str, text: &str) -> String {
    format!(r#"{{"id":"{id}","timestamp":"{ts}","type":"user","content":[{{"text":"{text}"}}]}}"#)
}

fn model_msg(id: &str, ts: &str, text: &str) -> String {
    format!(r#"{{"id":"{id}","timestamp":"{ts}","type":"model","content":[{{"text":"{text}"}}]}}"#)
}

fn set_messages(ts: &str, messages: &str) -> String {
    format!(r#"{{"$set":{{"messages":{messages},"lastUpdated":"{ts}"}}}}"#)
}

fn jsonl(lines: &[&str]) -> String {
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

fn plains(lines: &[hermon::render::StyledLine]) -> Vec<String> {
    lines.iter().map(|l| l.to_plain()).collect()
}

fn jail(home: &Path) -> PathBuf {
    home.join("tmp")
}

#[test]
fn replay_zero_initializes_without_emitting() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u1", "2026-09-15T18:00:01.000Z", "stale"),
        ]),
    );
    let mut t = GeminiTailer::new(&path, jail(&home), NO_REPLAY);
    assert!(t.poll().is_empty(), "bytes=0 emits no existing messages");
    assert!(t.poll().is_empty(), "unchanged tick stays quiet");

    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(f, "{}", user_msg("u2", "2026-09-15T18:00:02.000Z", "fresh")).unwrap();
    drop(f);
    let out = plains(&t.poll());
    assert!(
        out.iter().any(|l| l.contains("fresh")),
        "append after init: {out:?}"
    );
    assert!(
        out.iter().all(|l| !l.contains("stale")),
        "must not replay stale: {out:?}"
    );
}

#[test]
fn replay_nonzero_emits_suffix_of_normalized_text() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u1", "2026-09-15T18:00:01.000Z", "one"),
            &user_msg("u2", "2026-09-15T18:00:02.000Z", "two"),
            &user_msg("u3", "2026-09-15T18:00:03.000Z", "three"),
        ]),
    );
    let mut t = GeminiTailer::new(&path, jail(&home), HUGE);
    let out = plains(&t.poll());
    assert!(out.iter().any(|l| l.contains("one")), "{out:?}");
    assert!(out.iter().any(|l| l.contains("two")), "{out:?}");
    assert!(out.iter().any(|l| l.contains("three")), "{out:?}");
    assert!(t.poll().is_empty(), "second poll is quiet");

    // Tiny budget: only the newest message's text bytes should fit.
    let mut t = GeminiTailer::new(&path, jail(&home), Replay { bytes: 4, rows: 0 });
    let out = plains(&t.poll());
    assert!(
        out.iter().any(|l| l.contains("three")),
        "suffix includes the newest: {out:?}"
    );
    assert!(
        out.iter().all(|l| !l.contains("one")),
        "budget excludes earlier text: {out:?}"
    );
}

#[test]
fn reconstructs_when_a_raw_suffix_would_lack_the_header() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &set_messages(
                "2026-09-15T18:00:01.000Z",
                &format!(
                    "[{}]",
                    user_msg("u1", "2026-09-15T18:00:01.000Z", "base snapshot")
                ),
            ),
            &user_msg("u2", "2026-09-15T18:00:02.000Z", "later"),
        ]),
    );
    // A byte-seek into this file would miss the $set snapshot. Reconstruction
    // from the start still yields both messages.
    let mut t = GeminiTailer::new(&path, jail(&home), HUGE);
    let out = plains(&t.poll());
    assert!(out.iter().any(|l| l.contains("base snapshot")), "{out:?}");
    assert!(out.iter().any(|l| l.contains("later")), "{out:?}");
}

#[test]
fn identical_set_and_repeated_polls_do_not_duplicate() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &set_messages(
                "2026-09-15T18:00:01.000Z",
                &format!("[{}]", user_msg("u1", "2026-09-15T18:00:01.000Z", "hello")),
            ),
        ]),
    );
    let mut t = GeminiTailer::new(&path, jail(&home), HUGE);
    let first = plains(&t.poll());
    assert_eq!(first.iter().filter(|l| l.contains("hello")).count(), 1);
    assert!(t.poll().is_empty());

    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(
        f,
        "{}",
        set_messages(
            "2026-09-15T18:00:01.000Z",
            &format!("[{}]", user_msg("u1", "2026-09-15T18:00:01.000Z", "hello")),
        )
    )
    .unwrap();
    drop(f);
    assert!(
        t.poll().is_empty(),
        "identical $set must not re-emit the same message"
    );
}

#[test]
fn replacement_emits_revision_notice_and_current_context() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u1", "2026-09-15T18:00:01.000Z", "old prompt"),
            &user_msg("u2", "2026-09-15T18:00:02.000Z", "also old"),
        ]),
    );
    let mut t = GeminiTailer::new(&path, jail(&home), HUGE);
    t.poll();

    fs::write(
        &path,
        jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u3", "2026-09-15T18:00:03.000Z", "new prompt"),
        ]),
    )
    .unwrap();
    let out = plains(&t.poll());
    assert!(
        out.iter()
            .any(|l| l.contains("revised") || l.contains("truncated")),
        "replacement must not silently keep old pane lines: {out:?}"
    );
    assert!(
        out.iter().any(|l| l.contains("new prompt")),
        "bounded current context: {out:?}"
    );
}

#[test]
fn utf8_split_line_waits_for_newline() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[&header(
            "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
            "2026-09-15T18:00:00.000Z",
        )]),
    );
    let mut t = GeminiTailer::new(&path, jail(&home), NO_REPLAY);
    t.poll();

    let line = user_msg("u1", "2026-09-15T18:00:01.000Z", "caf\\u00e9 now");
    // Split a real multibyte sequence across two writes.
    let full = user_msg("u1", "2026-09-15T18:00:01.000Z", "café");
    let bytes = full.as_bytes();
    let split = bytes
        .iter()
        .position(|&b| b >= 0x80)
        .expect("multibyte in café");
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(&bytes[..split]).unwrap();
    f.flush().unwrap();
    drop(f);
    assert!(
        t.poll().is_empty(),
        "unterminated (and split-utf8) line must not render yet"
    );

    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(&bytes[split..]).unwrap();
    f.write_all(b"\n").unwrap();
    drop(f);
    let out = plains(&t.poll());
    assert!(
        out.iter().any(|l| l.contains("café")),
        "completed utf8 line: {out:?} (partial was {line})"
    );
}

#[test]
fn malformed_complete_lines_do_not_stall() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "session-2026-09-15T18-00-aaa11111.jsonl",
        "{not json}\n[1,2,3]\n",
    );
    let mut t = GeminiTailer::new(&path, jail(&home), HUGE);
    let _ = t.poll();
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(
        f,
        "{}",
        user_msg("u1", "2026-09-15T18:00:01.000Z", "recovered")
    )
    .unwrap();
    drop(f);
    let out = plains(&t.poll());
    assert!(out.iter().any(|l| l.contains("recovered")), "{out:?}");
}

#[test]
fn truncation_reloads_without_panic() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u1", "2026-09-15T18:00:01.000Z", "before"),
        ]),
    );
    let mut t = GeminiTailer::new(&path, jail(&home), HUGE);
    t.poll();
    fs::write(&path, b"").unwrap();
    let out = plains(&t.poll());
    assert!(
        out.iter()
            .any(|l| l.contains("truncated") || l.contains("revised")),
        "{out:?}"
    );
    fs::write(
        &path,
        jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u2", "2026-09-15T18:00:02.000Z", "after"),
        ]),
    )
    .unwrap();
    let out = plains(&t.poll());
    assert!(out.iter().any(|l| l.contains("after")), "{out:?}");
}

#[test]
fn equal_size_replacement_recovers() {
    let (_dir, home) = gemini_home();
    let a = jsonl(&[
        &header(
            "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
            "2026-09-15T18:00:00.000Z",
        ),
        &user_msg("u1", "2026-09-15T18:00:01.000Z", "alpha"),
    ]);
    let b = jsonl(&[
        &header(
            "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
            "2026-09-15T18:00:00.000Z",
        ),
        &user_msg("u1", "2026-09-15T18:00:01.000Z", "omega"),
    ]);
    let n = a.len().max(b.len());
    fn pad(mut s: String, n: usize) -> String {
        while s.len() < n {
            s.push('\n');
        }
        s
    }
    let a = pad(a, n);
    let b = pad(b, n);
    assert_eq!(a.len(), b.len());

    let path = write_session(&home, "session-2026-09-15T18-00-aaa11111.jsonl", &a);
    let mut t = GeminiTailer::new(&path, jail(&home), HUGE);
    let first = plains(&t.poll());
    assert!(first.iter().any(|l| l.contains("alpha")), "{first:?}");

    fs::write(&path, &b).unwrap();
    let out = plains(&t.poll());
    assert!(
        out.iter()
            .any(|l| l.contains("revised") || l.contains("omega") || l.contains("truncated")),
        "equal-size replacement: {out:?}"
    );
}

#[test]
fn delete_recreate_self_heals() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u1", "2026-09-15T18:00:01.000Z", "before"),
        ]),
    );
    let mut t = GeminiTailer::new(&path, jail(&home), HUGE);
    t.poll();
    fs::remove_file(&path).unwrap();
    let out = plains(&t.poll());
    assert!(
        out.iter()
            .any(|l| l.contains("removed") || l.contains("not found")),
        "{out:?}"
    );
    assert!(t.poll().is_empty(), "no repeated wait line");
    fs::write(
        &path,
        jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u2", "2026-09-15T18:00:02.000Z", "recreated"),
        ]),
    )
    .unwrap();
    let out = plains(&t.poll());
    assert!(
        out.iter()
            .any(|l| l.contains("recreated") || l.contains("revised")),
        "{out:?}"
    );
}

#[test]
fn missing_file_warns_once_then_heals() {
    let (_dir, home) = gemini_home();
    let chats = home.join("tmp/demo/chats");
    fs::create_dir_all(&chats).unwrap();
    let path = chats.join("session-2026-09-15T18-00-aaa11111.jsonl");
    let mut t = GeminiTailer::new(&path, jail(&home), HUGE);
    let first = plains(&t.poll());
    assert!(first.iter().any(|l| l.contains("not found")), "{first:?}");
    assert!(t.poll().is_empty());
    fs::write(
        &path,
        jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u1", "2026-09-15T18:00:01.000Z", "hello"),
        ]),
    )
    .unwrap();
    let out = plains(&t.poll());
    assert!(out.iter().any(|l| l.contains("hello")), "{out:?}");
}

#[test]
fn source_open_tailer_and_append() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u1", "2026-09-15T18:00:01.000Z", "hello"),
        ]),
    );
    let mut src = GeminiSource::new(&home);
    src.sessions();
    let mut t = src
        .open_tailer("aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa", HUGE)
        .expect("known id");
    let out = plains(&t.poll());
    assert!(out.iter().any(|l| l.contains("hello")), "{out:?}");

    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(
        f,
        "{}",
        model_msg("m1", "2026-09-15T18:00:02.000Z", "response")
    )
    .unwrap();
    drop(f);
    let out = plains(&t.poll());
    assert!(out.iter().any(|l| l.contains("response")), "{out:?}");
    assert!(t.poll().is_empty());
}

#[test]
fn info_is_not_emitted_as_user_text() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            r#"{"id":"i1","timestamp":"2026-09-15T18:00:01.000Z","type":"info","content":"Waiting for authentication..."}"#,
        ]),
    );
    let mut t = GeminiTailer::new(&path, jail(&home), HUGE);
    let out = plains(&t.poll());
    assert!(
        out.iter().all(|l| !l.starts_with("»")),
        "info is not a user prompt: {out:?}"
    );
}

#[test]
fn limit_notice_fires_once() {
    let (_dir, home) = gemini_home();
    let mut body = header(
        "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
        "2026-09-15T18:00:00.000Z",
    );
    body.push('\n');
    for i in 0..(hermon::source::gemini::MAX_JSONL_RECORDS + 8) {
        body.push_str(&user_msg(&format!("u{i}"), "2026-09-15T18:00:01.000Z", "x"));
        body.push('\n');
    }
    let path = write_session(&home, "session-2026-09-15T18-00-aaa11111.jsonl", &body);
    let mut t = GeminiTailer::new(&path, jail(&home), HUGE);
    let first = plains(&t.poll());
    let notices = first
        .iter()
        .filter(|l| l.contains("parse/state limit") || l.contains("truncated"))
        .count();
    assert_eq!(notices, 1, "{first:?}");
    let second = plains(&t.poll());
    assert!(
        second.iter().all(|l| !l.contains("parse/state limit")),
        "one notice: {second:?}"
    );
}
