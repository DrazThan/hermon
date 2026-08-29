//! OpenCodeTailer tests against a fixture DB built from the captured
//! schema. The interesting behaviour is what happens when a `part` row is
//! rewritten in place — the pending → completed flip a rowid cursor would
//! miss and a stateless renderer would print twice. Times are epoch
//! milliseconds, matching the real schema.

mod common;

use std::fs;
use std::path::Path;

use rusqlite::{Connection, params};
use tempfile::TempDir;

use common::{fixture_path, temp_db_from_schema};
use hermon::source::opencode::OpenCodeSource;
use hermon::source::{Replay, Tailer};

const NOW_MS: i64 = 1_700_000_000_000;
const MODEL_JSON: &str = r#"{"id":"claude-sonnet-5","providerID":"github-copilot"}"#;
const SES: &str = "ses_tail01";

/// Replay everything the small fixtures hold, so the tests exercise
/// transitions rather than the replay bound (which has its own test).
const ALL: Replay = Replay {
    bytes: 0,
    rows: 100,
};

fn setup_conn(db_path: &Path) -> Connection {
    let conn = Connection::open(db_path).expect("open writable setup connection");
    // the fixture omits tables the kept ones reference (e.g. `project`)
    conn.pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys");
    conn
}

/// A session with one assistant message, ready for parts to be hung off.
fn fixture() -> (TempDir, std::path::PathBuf, Connection) {
    let (dir, db_path) = temp_db_from_schema(&fixture_path("opencode_schema.sql"));
    let conn = setup_conn(&db_path);
    insert_session(&conn, None);
    insert_message(
        &conn,
        "msg_1",
        r#"{"role":"assistant","finish":"tool-calls"}"#,
    );
    (dir, db_path, conn)
}

fn insert_session(conn: &Connection, archived_ms: Option<i64>) {
    conn.execute(
        "INSERT INTO session (id, project_id, slug, directory, title, version, model,
         cost, tokens_input, tokens_output, tokens_cache_read, tokens_cache_write,
         time_created, time_updated, time_archived)
         VALUES (?1, 'prj', ?1, '/tmp', 'Tailed', '1', ?2, 0.05, 100, 20, 50, 5,
                 ?3, ?3, ?4)",
        params![SES, MODEL_JSON, NOW_MS - 120_000, archived_ms],
    )
    .expect("insert session");
}

fn insert_message(conn: &Connection, id: &str, data: &str) {
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?3, ?4)",
        params![id, SES, NOW_MS - 100_000, data],
    )
    .expect("insert message");
}

fn insert_part(conn: &Connection, id: &str, updated_ms: i64, data: &str) {
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
         VALUES (?1, 'msg_1', ?2, ?3, ?3, ?4)",
        params![id, SES, updated_ms, data],
    )
    .expect("insert part");
}

/// The in-place rewrite that makes OpenCode different: same row, new data,
/// bumped `time_updated`.
fn update_part(conn: &Connection, id: &str, updated_ms: i64, data: &str) {
    let n = conn
        .execute(
            "UPDATE part SET data = ?2, time_updated = ?3 WHERE id = ?1",
            params![id, data, updated_ms],
        )
        .expect("update part");
    assert_eq!(n, 1, "expected to update part {id}");
}

/// Bumps only the timestamp — a cosmetic re-touch, which the watermark
/// re-reads but the renderer must not print again.
fn touch_part(conn: &Connection, id: &str, updated_ms: i64) {
    conn.execute(
        "UPDATE part SET time_updated = ?2 WHERE id = ?1",
        params![id, updated_ms],
    )
    .expect("touch part");
}

fn tool_part(status: &str, extra: &str) -> String {
    format!(r#"{{"type":"tool","tool":"bash","state":{{"status":"{status}"{extra}}}}}"#)
}

fn tailer(db_path: &Path, replay: Replay) -> Box<dyn Tailer> {
    OpenCodeSource::new(db_path)
        .open_tailer(SES, replay)
        .expect("opencode tailer")
}

fn poll(t: &mut dyn Tailer) -> Vec<String> {
    t.poll().iter().map(|l| l.to_plain()).collect()
}

/// Everything but the periodic `Σ` counters, which have their own test.
fn parts_of(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().filter(|l| !l.starts_with('Σ')).collect()
}

#[test]
fn in_place_completion_emits_the_result_exactly_once() {
    let (_dir, db_path, conn) = fixture();
    insert_part(
        &conn,
        "prt_1",
        NOW_MS - 10_000,
        &tool_part("running", r#","input":{"command":"ls"}"#),
    );
    let mut t = tailer(&db_path, ALL);

    assert_eq!(parts_of(poll(t.as_mut())), [r#"▶ bash {"command":"ls"}"#]);

    update_part(
        &conn,
        "prt_1",
        NOW_MS - 5_000,
        &tool_part("completed", r#","input":{"command":"ls"},"output":"file1""#),
    );
    assert_eq!(parts_of(poll(t.as_mut())), ["◀ bash file1"]);

    // the same row keeps resurfacing only if touched; either way, silence
    assert!(parts_of(poll(t.as_mut())).is_empty());
    touch_part(&conn, "prt_1", NOW_MS - 1_000);
    assert!(parts_of(poll(t.as_mut())).is_empty());
}

#[test]
fn in_place_error_emits_the_error_once() {
    let (_dir, db_path, conn) = fixture();
    insert_part(&conn, "prt_1", NOW_MS - 10_000, &tool_part("running", ""));
    let mut t = tailer(&db_path, ALL);
    assert_eq!(parts_of(poll(t.as_mut())).len(), 1);

    update_part(
        &conn,
        "prt_1",
        NOW_MS - 5_000,
        &tool_part("error", r#","error":"user rejected permission""#),
    );
    assert_eq!(
        parts_of(poll(t.as_mut())),
        ["◀ ERROR bash: user rejected permission"]
    );
    assert!(parts_of(poll(t.as_mut())).is_empty());
}

#[test]
fn a_touched_old_part_is_not_reprinted() {
    let (_dir, db_path, conn) = fixture();
    insert_part(
        &conn,
        "prt_1",
        NOW_MS - 10_000,
        r#"{"type":"text","text":"hello"}"#,
    );
    insert_part(
        &conn,
        "prt_2",
        NOW_MS - 9_000,
        &tool_part("completed", r#","output":"done""#),
    );
    let mut t = tailer(&db_path, ALL);
    assert_eq!(
        parts_of(poll(t.as_mut())),
        ["hello", r#"▶ bash {}"#, "◀ bash done"]
    );

    // both rows re-surface past the watermark, neither may print again
    touch_part(&conn, "prt_1", NOW_MS - 2_000);
    touch_part(&conn, "prt_2", NOW_MS - 1_000);
    assert!(parts_of(poll(t.as_mut())).is_empty());
}

#[test]
fn replay_seeds_only_the_last_n_parts() {
    let (_dir, db_path, conn) = fixture();
    for i in 1..=5 {
        insert_part(
            &conn,
            &format!("prt_{i}"),
            NOW_MS - 10_000 + i64::from(i) * 100,
            &format!(r#"{{"type":"text","text":"line {i}"}}"#),
        );
    }
    let mut t = tailer(&db_path, Replay { bytes: 0, rows: 2 });
    assert_eq!(parts_of(poll(t.as_mut())), ["line 4", "line 5"]);

    // and it keeps streaming from there
    insert_part(
        &conn,
        "prt_6",
        NOW_MS - 1_000,
        r#"{"type":"text","text":"line 6"}"#,
    );
    assert_eq!(parts_of(poll(t.as_mut())), ["line 6"]);
}

#[test]
fn an_empty_session_replays_nothing_then_streams() {
    let (_dir, db_path, conn) = fixture();
    let mut t = tailer(&db_path, ALL);
    assert!(parts_of(poll(t.as_mut())).is_empty());

    insert_part(&conn, "prt_1", 1, r#"{"type":"text","text":"first ever"}"#);
    assert_eq!(parts_of(poll(t.as_mut())), ["first ever"]);
}

#[test]
fn a_part_without_its_message_is_skipped() {
    // the join is inner, as in the Python: a part whose message row has not
    // landed yet waits rather than rendering with an unknown role
    let (_dir, db_path, conn) = fixture();
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
         VALUES ('prt_orphan', 'msg_missing', ?1, ?2, ?2, ?3)",
        params![SES, NOW_MS - 10_000, r#"{"type":"text","text":"orphan"}"#],
    )
    .expect("insert orphan part");
    let mut t = tailer(&db_path, ALL);
    assert!(parts_of(poll(t.as_mut())).is_empty());
}

#[test]
fn user_and_assistant_roles_come_from_the_message_row() {
    let (_dir, db_path, conn) = fixture();
    insert_message(&conn, "msg_user", r#"{"role":"user"}"#);
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
         VALUES ('prt_u', 'msg_user', ?1, ?2, ?2, ?3)",
        params![SES, NOW_MS - 10_000, r#"{"type":"text","text":"do it"}"#],
    )
    .expect("insert user part");
    insert_part(
        &conn,
        "prt_a",
        NOW_MS - 9_000,
        r#"{"type":"text","text":"on it"}"#,
    );

    let mut t = tailer(&db_path, ALL);
    assert_eq!(parts_of(poll(t.as_mut())), ["» user: do it", "on it"]);
}

#[test]
fn stats_line_appears_periodically_and_only_when_it_changes() {
    let (_dir, db_path, _conn) = fixture();
    let mut t = tailer(&db_path, ALL);

    let first = poll(t.as_mut());
    assert_eq!(first, ["Σ in:155 out:20 $0.0500  [claude-sonnet-5]"]);
    // ~2s of polls at PANE_TICK before the next stats read, and the
    // unchanged counters stay silent when it comes
    for _ in 0..20 {
        assert!(poll(t.as_mut()).is_empty());
    }
}

#[test]
fn changed_counters_reprint_the_stats_line() {
    let (_dir, db_path, conn) = fixture();
    let mut t = tailer(&db_path, ALL);
    assert_eq!(poll(t.as_mut()).len(), 1);

    conn.execute(
        "UPDATE session SET tokens_output = 999 WHERE id = ?1",
        params![SES],
    )
    .expect("bump tokens");
    // polls 2..7 are silent, the seventh reads the session again
    let mut seen = Vec::new();
    for _ in 0..7 {
        seen.extend(poll(t.as_mut()));
    }
    assert_eq!(seen, ["Σ in:155 out:999 $0.0500  [claude-sonnet-5]"]);
}

#[test]
fn an_archived_session_closes_exactly_once_then_goes_quiet() {
    let (_dir, db_path, conn) = fixture();
    insert_part(
        &conn,
        "prt_1",
        NOW_MS - 10_000,
        r#"{"type":"text","text":"bye"}"#,
    );
    conn.execute(
        "UPDATE session SET time_archived = ?2 WHERE id = ?1",
        params![SES, NOW_MS],
    )
    .expect("archive session");

    let mut t = tailer(&db_path, ALL);
    let lines = poll(t.as_mut());
    assert_eq!(
        lines.iter().filter(|l| *l == "Σ session archived").count(),
        1
    );
    assert_eq!(lines.first().map(String::as_str), Some("bye"));

    // quiescent afterwards, even though new parts keep landing
    insert_part(
        &conn,
        "prt_2",
        NOW_MS + 1,
        r#"{"type":"text","text":"late"}"#,
    );
    for _ in 0..8 {
        assert!(poll(t.as_mut()).is_empty());
    }
}

#[test]
fn a_missing_db_waits_once_and_then_self_heals() {
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("opencode.db");
    let mut t = tailer(&db_path, ALL);

    assert_eq!(poll(t.as_mut()), ["· opencode db unavailable — waiting"]);
    for _ in 0..3 {
        assert!(
            poll(t.as_mut()).is_empty(),
            "the wait notice must not repeat"
        );
    }

    let schema = fs::read_to_string(fixture_path("opencode_schema.sql")).expect("read schema");
    let conn = Connection::open(&db_path).expect("create db");
    conn.execute_batch(&schema).expect("apply schema");
    conn.pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys");
    insert_session(&conn, None);
    insert_message(&conn, "msg_1", r#"{"role":"assistant"}"#);
    insert_part(
        &conn,
        "prt_1",
        NOW_MS - 1_000,
        r#"{"type":"text","text":"back"}"#,
    );

    assert_eq!(parts_of(poll(t.as_mut())), ["back"]);
}

#[test]
fn a_malformed_part_row_degrades_without_stopping_the_tail() {
    let (_dir, db_path, conn) = fixture();
    insert_part(&conn, "prt_bad", NOW_MS - 10_000, "{not json");
    insert_part(
        &conn,
        "prt_ok",
        NOW_MS - 9_000,
        r#"{"type":"text","text":"still here"}"#,
    );
    let mut t = tailer(&db_path, ALL);
    assert_eq!(parts_of(poll(t.as_mut())), ["· parse-skip", "still here"]);
}
