//! GeminiSource acceptance tests (#106): discovery, patch-aware message
//! state, logs.json fallback, trait use, and Gm: protocol compatibility.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tempfile::TempDir;

use hermon::remote::proto::{AgentMsg, Decoded, HostCmd, decode_agent_msg, encode_agent_msg};
use hermon::source::gemini::{GeminiSource, PREFIX};
use hermon::source::{LastEvent, Liveness, Replay, Source, classify};

const IDLE: f64 = 180.0;
const FRESH: f64 = 3_600.0;

fn fixture_home() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("gemini")
}

/// A Gemini home whose `tmp/` is a copy of the sanitized fixtures, so tests
/// can mutate without touching the tree.
fn copied_fixture_home() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().to_path_buf();
    let tmp = home.join("tmp");
    copy_dir(&fixture_home(), &tmp);
    (dir, home)
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("mkdir");
    for entry in fs::read_dir(src).expect("read fixture dir") {
        let entry = entry.expect("entry");
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("ft").is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), to).expect("copy");
        }
    }
}

fn gemini_home() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().to_path_buf();
    fs::create_dir_all(home.join("tmp")).expect("tmp");
    (dir, home)
}

fn write_session(home: &Path, project: &str, filename: &str, body: &str) -> PathBuf {
    let chats = home.join("tmp").join(project).join("chats");
    fs::create_dir_all(&chats).expect("chats");
    let path = chats.join(filename);
    fs::write(&path, body).expect("write session");
    path
}

fn write_project_root(home: &Path, project: &str, cwd: &str) {
    let dir = home.join("tmp").join(project);
    fs::create_dir_all(&dir).expect("project");
    fs::write(dir.join(".project_root"), cwd).expect("project_root");
}

fn write_logs(home: &Path, project: &str, body: &str) {
    let dir = home.join("tmp").join(project);
    fs::create_dir_all(&dir).expect("project");
    fs::write(dir.join("logs.json"), body).expect("logs");
}

fn header(session: &str, ts: &str) -> String {
    format!(
        r#"{{"sessionId":"{session}","projectHash":"abc","startTime":"{ts}","lastUpdated":"{ts}","kind":"main"}}"#
    )
}

fn set_messages(ts: &str, messages: &str) -> String {
    format!(r#"{{"$set":{{"messages":{messages},"lastUpdated":"{ts}"}}}}"#)
}

fn set_updated(ts: &str) -> String {
    format!(r#"{{"$set":{{"lastUpdated":"{ts}"}}}}"#)
}

fn user_msg(id: &str, ts: &str, text: &str) -> String {
    format!(r#"{{"id":"{id}","timestamp":"{ts}","type":"user","content":[{{"text":"{text}"}}]}}"#)
}

fn info_msg(id: &str, ts: &str, text: &str) -> String {
    format!(r#"{{"id":"{id}","timestamp":"{ts}","type":"info","content":"{text}"}}"#)
}

fn model_msg(id: &str, ts: &str, text: &str) -> String {
    format!(r#"{{"id":"{id}","timestamp":"{ts}","type":"model","content":[{{"text":"{text}"}}]}}"#)
}

fn ctx_msg(ts: &str) -> String {
    format!(
        r#"{{"id":"ctx1","timestamp":"{ts}","type":"user","content":[{{"text":"<session_context>\nThis is the Gemini CLI. We are setting up the context for our chat.\n</session_context>"}}]}}"#
    )
}

fn jsonl(lines: &[&str]) -> String {
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

fn by_id<'a>(
    sessions: &'a [hermon::source::SessionMeta],
    id: &str,
) -> &'a hermon::source::SessionMeta {
    sessions
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("missing session {id}: {sessions:?}"))
}

#[test]
fn fixture_name_and_hash_projects() {
    let (_dir, home) = copied_fixture_home();
    // A hash-looking sibling next to the copied fixtures.
    write_session(
        &home,
        "0d8fed00d5c57334e0d65fb493850763b2c16b06f97610db",
        "session-2026-09-15T18-00-ccc33333.jsonl",
        &jsonl(&[
            &header(
                "cccccccc-3333-4000-8000-cccccccccccc",
                "2026-09-15T18:00:20.000Z",
            ),
            &set_messages(
                "2026-09-15T18:00:20.001Z",
                &format!("[{}]", ctx_msg("2026-09-15T18:00:20.001Z")),
            ),
        ]),
    );

    let mut src = GeminiSource::new(&home);
    let sessions = src.sessions();
    let ids: Vec<_> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert!(
        ids.contains(&"aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa"),
        "{ids:?}"
    );
    assert!(
        ids.contains(&"bbbbbbbb-2222-4000-8000-bbbbbbbbbbbb"),
        "{ids:?}"
    );
    assert!(
        ids.contains(&"cccccccc-3333-4000-8000-cccccccccccc"),
        "{ids:?}"
    );

    let named = by_id(&sessions, "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa");
    assert_eq!(named.title, "list the files");
    // logs.json adds a later model line (different messageId); same-id user
    // text stays the transcript's.
    assert_eq!(named.last_event, Some(LastEvent::AssistantText));
    assert_eq!(named.last_line, "fallback model text");
    assert_eq!(named.model, "unknown");
    assert_eq!(named.in_tok, 0);
    assert_eq!(named.out_tok, 0);
    assert_eq!(named.cost, None);
    assert_eq!(named.last_tool, "-");
    assert!(!named.tool_pending);
    assert!(!named.turn_done);
    assert!(!named.ended);
    assert!(!named.force_live);

    let hashed = by_id(&sessions, "bbbbbbbb-2222-4000-8000-bbbbbbbbbbbb");
    // No .project_root: truthful project label, not a guessed cwd.
    assert_eq!(hashed.title, "hashed-project");
    assert_eq!(
        hashed.last_event, None,
        "info/context are not conversational"
    );
    assert_eq!(hashed.last_line, "");
}

#[test]
fn missing_project_root_is_label_not_guessed_path() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "deadbeefcafebabe",
        "session-2026-09-15T18-00-ddd44444.jsonl",
        &jsonl(&[&header(
            "dddddddd-4444-4000-8000-dddddddddddd",
            "2026-09-15T18:00:00.000Z",
        )]),
    );
    let mut src = GeminiSource::new(&home);
    let s = &src.sessions()[0];
    assert_eq!(s.title, "deadbeefcafebabe");
    assert!(!s.title.starts_with('/'), "{}", s.title);
}

#[test]
fn project_root_is_display_metadata_only() {
    let (_dir, home) = gemini_home();
    write_project_root(&home, "demo", "/workspace/demo");
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-eee55555.jsonl",
        &jsonl(&[&header(
            "eeeeeeee-5555-4000-8000-eeeeeeeeeeee",
            "2026-09-15T18:00:00.000Z",
        )]),
    );
    let mut src = GeminiSource::new(&home);
    let s = &src.sessions()[0];
    assert_eq!(s.title, "/workspace/demo");
}

#[test]
fn concurrent_sessions_in_one_project() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[&header(
            "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
            "2026-09-15T18:00:00.000Z",
        )]),
    );
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-01-bbb22222.jsonl",
        &jsonl(&[&header(
            "bbbbbbbb-2222-4000-8000-bbbbbbbbbbbb",
            "2026-09-15T18:01:00.000Z",
        )]),
    );
    let mut src = GeminiSource::new(&home);
    let mut ids: Vec<_> = src.sessions().into_iter().map(|s| s.id).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa".to_string(),
            "bbbbbbbb-2222-4000-8000-bbbbbbbbbbbb".to_string(),
        ]
    );
}

#[test]
fn header_patch_metadata_and_direct_records() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &set_messages(
                "2026-09-15T18:00:00.001Z",
                &format!("[{}]", ctx_msg("2026-09-15T18:00:00.001Z")),
            ),
            &user_msg("u1", "2026-09-15T18:00:01.000Z", "hello there"),
            &set_updated("2026-09-15T18:00:01.000Z"),
            &info_msg(
                "i1",
                "2026-09-15T18:00:02.000Z",
                "Waiting for authentication...",
            ),
            &set_updated("2026-09-15T18:00:02.000Z"),
            &model_msg("m1", "2026-09-15T18:00:03.000Z", "hi back"),
        ]),
    );
    let mut src = GeminiSource::new(&home);
    let s = &src.sessions()[0];
    assert_eq!(s.id, "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa");
    assert_eq!(s.title, "hello there");
    assert_eq!(s.last_event, Some(LastEvent::AssistantText));
    assert_eq!(s.last_line, "hi back");
    assert_eq!(s.model, "unknown");
    assert_eq!((s.in_tok, s.out_tok, s.cost), (0, 0, None));
}

#[test]
fn set_messages_replaces_instead_of_appending() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &set_messages(
                "2026-09-15T18:00:01.000Z",
                &format!(
                    "[{},{}]",
                    ctx_msg("2026-09-15T18:00:00.001Z"),
                    user_msg("u1", "2026-09-15T18:00:01.000Z", "first")
                ),
            ),
            &set_messages(
                "2026-09-15T18:00:02.000Z",
                &format!(
                    "[{}]",
                    user_msg("u1", "2026-09-15T18:00:02.000Z", "replaced")
                ),
            ),
        ]),
    );
    let mut src = GeminiSource::new(&home);
    let s = &src.sessions()[0];
    assert_eq!(s.title, "replaced");
    assert_eq!(s.last_line, "» replaced");
    assert!(!s.last_line.contains("first"), "{}", s.last_line);
    // Identical $set again must not duplicate.
    let mut extra = fs::read_to_string(&path).unwrap();
    extra.push_str(&set_messages(
        "2026-09-15T18:00:02.000Z",
        &format!(
            "[{}]",
            user_msg("u1", "2026-09-15T18:00:02.000Z", "replaced")
        ),
    ));
    extra.push('\n');
    fs::write(&path, extra).unwrap();
    let again = src.sessions();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].title, "replaced");
    assert_eq!(again[0].last_line, "» replaced");
}

#[test]
fn direct_record_update_and_remove_via_set() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u1", "2026-09-15T18:00:01.000Z", "one"),
            &user_msg("u1", "2026-09-15T18:00:02.000Z", "two"),
            &user_msg("u2", "2026-09-15T18:00:03.000Z", "keep"),
            &set_messages(
                "2026-09-15T18:00:04.000Z",
                &format!("[{}]", user_msg("u2", "2026-09-15T18:00:04.000Z", "keep")),
            ),
        ]),
    );
    let mut src = GeminiSource::new(&home);
    let s = &src.sessions()[0];
    assert_eq!(s.title, "keep");
    assert_eq!(s.last_line, "» keep");
}

#[test]
fn context_and_info_are_not_user_events() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &set_messages(
                "2026-09-15T18:00:00.001Z",
                &format!("[{}]", ctx_msg("2026-09-15T18:00:00.001Z")),
            ),
            &info_msg("i1", "2026-09-15T18:00:02.000Z", "Authentication succeeded"),
        ]),
    );
    let mut src = GeminiSource::new(&home);
    let s = &src.sessions()[0];
    assert_eq!(s.last_event, None);
    assert_eq!(s.last_line, "");
    assert!(!s.title.contains("session_context"), "{}", s.title);
}

#[test]
fn repeated_sessions_poll_does_not_duplicate() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
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
    let a = src.sessions();
    let b = src.sessions();
    assert_eq!(a, b);
    assert_eq!(a.len(), 1);
}

#[test]
fn filename_id_until_header_then_reconciles() {
    let (_dir, home) = gemini_home();
    let path = write_session(&home, "demo", "session-2026-09-15T18-00-fff66666.jsonl", "");
    let mut src = GeminiSource::new(&home);
    let first = src.sessions();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, "fff66666");

    fs::write(
        &path,
        jsonl(&[&header(
            "ffffffff-6666-4000-8000-ffffffffffff",
            "2026-09-15T18:00:00.000Z",
        )]),
    )
    .unwrap();
    let second = src.sessions();
    assert_eq!(second.len(), 1, "header must not add a duplicate row");
    assert_eq!(second[0].id, "ffffffff-6666-4000-8000-ffffffffffff");
}

#[test]
fn missing_model_and_accounting_fallbacks() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[&header(
            "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
            "2026-09-15T18:00:00.000Z",
        )]),
    );
    let mut src = GeminiSource::new(&home);
    let s = &src.sessions()[0];
    assert_eq!(s.model, "unknown");
    assert_eq!(s.in_tok, 0);
    assert_eq!(s.out_tok, 0);
    assert_eq!(s.cost, None);
    assert_eq!(src.last_tool(&s.id), "-");
}

#[test]
fn timeout_liveness_uses_unchanged_classify() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
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
    let s = src.sessions().into_iter().next().unwrap();
    assert_eq!(classify(&s, s.last_ts + 10.0, IDLE, FRESH), Liveness::Live);
    assert_eq!(
        classify(&s, s.last_ts + IDLE + 1.0, IDLE, FRESH),
        Liveness::Done
    );
    assert_ne!(
        classify(&s, s.last_ts + 31.0, IDLE, FRESH),
        Liveness::Attention(hermon::source::Attn::PermWait)
    );
}

#[test]
fn source_trait_and_unknown_ids_cannot_open_files() {
    let (_dir, home) = gemini_home();
    let path = write_session(
        &home,
        "demo",
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
    let src: &mut dyn Source = &mut src;
    let sessions = src.sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(src.last_tool("aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa"), "-");
    assert!(
        src.open_tailer("/etc/passwd", Replay::DEFAULT).is_none(),
        "path-like ids must not select files"
    );
    assert!(
        src.open_tailer(path.to_str().unwrap(), Replay::DEFAULT)
            .is_none()
    );
    assert!(src.open_tailer("nope", Replay::DEFAULT).is_none());
    assert!(
        src.open_tailer("aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa", Replay::DEFAULT)
            .is_some()
    );
}

#[test]
fn missing_and_unreadable_stores_are_empty() {
    let mut src = GeminiSource::new("/nonexistent/gemini-home");
    assert!(src.sessions().is_empty());

    let (_dir, home) = gemini_home();
    let mut src = GeminiSource::new(&home);
    assert!(src.sessions().is_empty());
}

#[test]
fn unreadable_neighbor_does_not_hide_good_session() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[&header(
            "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
            "2026-09-15T18:00:00.000Z",
        )]),
    );
    let bad = write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-zzz99999.jsonl",
        &jsonl(&[&header(
            "zzzzzzzz-9999-4000-8000-zzzzzzzzzzzz",
            "2026-09-15T18:00:00.000Z",
        )]),
    );
    let mut perms = fs::metadata(&bad).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&bad, perms).unwrap();

    let mut src = GeminiSource::new(&home);
    let sessions = src.sessions();
    let ids: Vec<_> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert!(
        ids.contains(&"aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa"),
        "{ids:?}"
    );

    let mut perms = fs::metadata(&bad).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&bad, perms).unwrap();
}

#[test]
fn malformed_and_unsupported_records_are_skipped() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            "{not json}",
            "[1,2,3]",
            r#"{"$unset":{"messages":true}}"#,
            r#"{"$set":{"unknownField":1}}"#,
            r#"{"$push":{"messages":[]}}"#,
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u1", "2026-09-15T18:00:01.000Z", "ok"),
        ]),
    );
    let mut src = GeminiSource::new(&home);
    let s = &src.sessions()[0];
    assert_eq!(s.title, "ok");
}

#[test]
fn logs_matching_unrelated_and_dedup() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[
            &header(
                "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
                "2026-09-15T18:00:00.000Z",
            ),
            &user_msg("u1", "2026-09-15T18:00:01.000Z", "from transcript"),
        ]),
    );
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-bbb22222.jsonl",
        &jsonl(&[&header(
            "bbbbbbbb-2222-4000-8000-bbbbbbbbbbbb",
            "2026-09-15T18:00:00.000Z",
        )]),
    );
    write_logs(
        &home,
        "demo",
        r#"[
          {"sessionId":"aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa","messageId":"u1","type":"user","message":"from logs should lose","timestamp":"2026-09-15T18:00:09.000Z"},
          {"sessionId":"aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa","messageId":"log2","type":"model","message":"fallback assistant","timestamp":"2026-09-15T18:00:04.000Z"},
          {"sessionId":"bbbbbbbb-2222-4000-8000-bbbbbbbbbbbb","messageId":"x","type":"user","message":"other","timestamp":"2026-09-15T19:00:00.000Z"}
        ]"#,
    );
    let mut src = GeminiSource::new(&home);
    let sessions = src.sessions();
    let a = by_id(&sessions, "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa");
    assert_eq!(a.title, "from transcript", "transcript wins same messageId");
    assert_eq!(a.last_event, Some(LastEvent::AssistantText));
    assert_eq!(a.last_line, "fallback assistant");

    let b = by_id(&sessions, "bbbbbbbb-2222-4000-8000-bbbbbbbbbbbb");
    assert_eq!(b.title, "other");
    // Unrelated session must not inherit logs.json mtime as last_ts for A
    // beyond matching entries. B's matching log may set B's last_ts.
    let before = a.last_ts;
    let logs_path = home.join("tmp/demo/logs.json");
    // Touch logs.json to a far-future mtime; matching entries (not the
    // file mtime) are the only logs-derived activity allowed.
    let future = SystemTime::now() + Duration::from_secs(86_400);
    let f = fs::File::open(&logs_path).unwrap();
    f.set_modified(future).unwrap();
    drop(f);
    let again = src.sessions();
    let a2 = by_id(&again, "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa");
    let future_ts = future.duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
    assert!(
        (a2.last_ts - before).abs() < 2.0,
        "project-wide logs mtime must not refresh session A: before={before} after={}",
        a2.last_ts
    );
    assert!(
        a2.last_ts < future_ts - 1000.0,
        "must not adopt logs.json mtime {future_ts}: {}",
        a2.last_ts
    );
}

#[test]
fn logs_cache_and_partial_replacement() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[&header(
            "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
            "2026-09-15T18:00:00.000Z",
        )]),
    );
    write_logs(
        &home,
        "demo",
        r#"[{"sessionId":"aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa","messageId":"log1","type":"user","message":"cached prompt","timestamp":"2026-09-15T18:00:01.000Z"}]"#,
    );
    let mut src = GeminiSource::new(&home);
    let s = src.sessions();
    assert_eq!(s[0].title, "cached prompt");
    let again = src.sessions();
    assert_eq!(again[0].title, "cached prompt");

    write_logs(&home, "demo", "{");
    let again = src.sessions();
    assert_eq!(
        again[0].title, "cached prompt",
        "partial replacement retains last valid fallback"
    );

    write_logs(
        &home,
        "demo",
        r#"[{"sessionId":"aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa","messageId":"log2","type":"user","message":"new prompt","timestamp":"2026-09-15T18:00:02.000Z"}]"#,
    );
    let third = src.sessions();
    assert_eq!(third[0].title, "new prompt");
}

#[test]
fn record_limit_is_finite_and_does_not_panic() {
    let (_dir, home) = gemini_home();
    let mut body = header(
        "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
        "2026-09-15T18:00:00.000Z",
    );
    body.push('\n');
    for i in 0..(hermon::source::gemini::MAX_JSONL_RECORDS + 50) {
        body.push_str(&user_msg(&format!("u{i}"), "2026-09-15T18:00:01.000Z", "x"));
        body.push('\n');
    }
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &body,
    );
    let mut src = GeminiSource::new(&home);
    let sessions = src.sessions();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].last_event == Some(LastEvent::User) || sessions[0].title == "x");
}

#[test]
fn gm_prefix_round_trips_the_wire_protocol_without_schema_change() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
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
    let mut meta = src.sessions().remove(0);
    meta.id = format!("{}:{}", PREFIX, meta.id);

    let snap = AgentMsg::Snap {
        sessions: vec![meta.clone()],
    };
    let line = encode_agent_msg(&snap);
    match decode_agent_msg(&line) {
        Decoded::Msg(AgentMsg::Snap { sessions }) => {
            assert_eq!(sessions[0].id, meta.id);
            assert!(sessions[0].id.starts_with("Gm:"));
            assert_eq!(sessions[0].last_event, Some(LastEvent::User));
        }
        other => panic!("expected Snap, got {other:?}"),
    }

    let open = encode_agent_msg(&AgentMsg::Hello {
        proto_version: 1,
        hostname: "box".into(),
        sources: vec!["gemini".into()],
    });
    assert!(matches!(
        decode_agent_msg(&open),
        Decoded::Msg(AgentMsg::Hello { .. })
    ));

    let cmd = serde_json::to_string(&HostCmd::OpenTail {
        key: meta.id.clone(),
        replay: Replay::DEFAULT,
    })
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&cmd).unwrap();
    assert_eq!(v["key"], meta.id);
    let (prefix, rest) = meta.id.split_once(':').unwrap();
    assert_eq!(prefix, "Gm");
    assert_eq!(rest, "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa");
}

#[cfg(unix)]
#[test]
fn symlink_outside_tmp_is_not_discovered() {
    let (_dir, home) = gemini_home();
    write_session(
        &home,
        "demo",
        "session-2026-09-15T18-00-aaa11111.jsonl",
        &jsonl(&[&header(
            "aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa",
            "2026-09-15T18:00:00.000Z",
        )]),
    );
    let outside = home.join("outside.jsonl");
    fs::write(
        &outside,
        jsonl(&[&header(
            "eeeeeeee-0000-4000-8000-eeeeeeeeeeee",
            "2026-09-15T18:00:00.000Z",
        )]),
    )
    .unwrap();
    let chats = home.join("tmp/demo/chats");
    std::os::unix::fs::symlink(
        &outside,
        chats.join("session-2026-09-15T18-00-esc11111.jsonl"),
    )
    .unwrap();

    let mut src = GeminiSource::new(&home);
    let ids: Vec<_> = src.sessions().into_iter().map(|s| s.id).collect();
    assert_eq!(
        ids,
        vec!["aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa".to_string()]
    );
}
