//! OpenCodeSource tests against a fixture DB built from the captured
//! schema, mirroring `tests/test_opencode.py` `TestOpenCodeSource`.
//! Times in the fixture are epoch milliseconds, matching the real schema.

mod common;

use std::path::PathBuf;

use rusqlite::{Connection, params};
use tempfile::TempDir;

use common::{fixture_path, temp_db_from_schema};
use hermon::source::opencode::OpenCodeSource;
use hermon::source::{Liveness, classify};

const NOW: f64 = 1_700_000_000.0;
const NOW_MS: i64 = 1_700_000_000_000;
const IDLE: f64 = 180.0;
const FRESH: f64 = 3_600.0;
const MODEL_JSON: &str = r#"{"id":"claude-sonnet-5","providerID":"github-copilot"}"#;

fn open_fixture() -> (TempDir, PathBuf, Connection) {
    let (dir, db_path) = temp_db_from_schema(&fixture_path("opencode_schema.sql"));
    let conn = Connection::open(&db_path).expect("open writable setup connection");
    // the fixture deliberately omits tables like `project` that the kept
    // tables reference, so FK checks must be off for setup inserts
    conn.pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys");
    (dir, db_path, conn)
}

fn insert_session(
    conn: &Connection,
    id: &str,
    model: &str,
    created_ms: i64,
    updated_ms: i64,
    archived_ms: Option<i64>,
) {
    conn.execute(
        "INSERT INTO session (id, project_id, slug, directory, title, version, model,
         cost, tokens_input, tokens_output, tokens_cache_read, tokens_cache_write,
         time_created, time_updated, time_archived)
         VALUES (?1, 'prj', ?1, '/tmp', 'Live One', '1', ?2, 0.05, 100, 20, 50, 5,
                 ?3, ?4, ?5)",
        params![id, model, created_ms, updated_ms, archived_ms],
    )
    .expect("insert session");
}

fn insert_message(conn: &Connection, id: &str, session_id: &str, ts_ms: i64, data: &str) {
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?3, ?4)",
        params![id, session_id, ts_ms, data],
    )
    .expect("insert message");
}

fn insert_part(conn: &Connection, id: &str, session_id: &str, ts_ms: i64, data: &str) {
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
         VALUES (?1, 'msg_1', ?2, ?3, ?3, ?4)",
        params![id, session_id, ts_ms, data],
    )
    .expect("insert part");
}

fn msg(role: &str, finish: Option<&str>) -> String {
    match finish {
        Some(f) => format!(r#"{{"role":"{role}","finish":"{f}"}}"#),
        None => format!(r#"{{"role":"{role}"}}"#),
    }
}

/// One live session with a pending tool call, as in the Python fixture.
fn live_fixture() -> (TempDir, PathBuf) {
    let (dir, db_path, conn) = open_fixture();
    insert_session(
        &conn,
        "ses_live01",
        MODEL_JSON,
        NOW_MS - 120_000,
        NOW_MS - 5_000,
        None,
    );
    insert_message(
        &conn,
        "msg_1",
        "ses_live01",
        NOW_MS - 5_000,
        &msg("assistant", Some("tool-calls")),
    );
    (dir, db_path)
}

#[test]
fn live_tool_pending_session_meta() {
    let (_dir, db_path) = live_fixture();
    let mut src = OpenCodeSource::new(&db_path);
    let rows = src.sessions(NOW - 3_600.0);
    assert_eq!(rows.len(), 1);
    let s = &rows[0];
    assert_eq!(s.id, "ses_live01");
    assert_eq!(s.model, "claude-sonnet-5");
    assert_eq!(s.title, "Live One");
    assert_eq!(s.in_tok, 155); // input + cache_read + cache_write
    assert_eq!(s.out_tok, 20);
    assert!(s.cost.is_some_and(|c| (c - 0.05).abs() < 1e-9));
    assert!(s.tool_pending);
    assert!(!s.turn_done);
    assert!(!s.ended);
    assert_eq!(s.last_tool, "-");
    assert_eq!(s.last_line, "assistant · tool-calls");
    assert!(s.last_event.is_none());
    // tool-calls pending -> live even though 5s of silence have passed
    assert_eq!(classify(s, NOW, IDLE, FRESH), Liveness::Live);
}

#[test]
fn clean_stop_is_turn_done() {
    let (_dir, db_path, conn) = open_fixture();
    insert_session(
        &conn,
        "ses_done01",
        MODEL_JSON,
        NOW_MS - 120_000,
        NOW_MS - 5_000,
        None,
    );
    insert_message(
        &conn,
        "msg_1",
        "ses_done01",
        NOW_MS - 5_000,
        &msg("assistant", Some("stop")),
    );

    let mut src = OpenCodeSource::new(&db_path);
    let s = &src.sessions(NOW - 3_600.0)[0];
    assert!(s.turn_done);
    assert!(!s.tool_pending);
    assert_eq!(s.last_line, "assistant · stop");
    // done the instant the turn closes, regardless of recency
    assert_eq!(classify(s, NOW, IDLE, FRESH), Liveness::Done);
}

#[test]
fn archived_session_is_ended() {
    let (_dir, db_path, conn) = open_fixture();
    insert_session(
        &conn,
        "ses_arch01",
        MODEL_JSON,
        NOW_MS - 120_000,
        NOW_MS - 5_000,
        Some(NOW_MS - 1_000),
    );
    insert_message(
        &conn,
        "msg_1",
        "ses_arch01",
        NOW_MS - 5_000,
        &msg("assistant", Some("tool-calls")),
    );

    let mut src = OpenCodeSource::new(&db_path);
    let s = &src.sessions(NOW - 3_600.0)[0];
    assert!(s.ended);
    // ended wins over the pending tool call
    assert_eq!(classify(s, NOW, IDLE, FRESH), Liveness::Done);
}

#[test]
fn timestamps_convert_ms_to_seconds() {
    let (_dir, db_path) = live_fixture();
    let mut src = OpenCodeSource::new(&db_path);
    let s = &src.sessions(NOW - 3_600.0)[0];
    // stored as epoch milliseconds, surfaced as epoch seconds
    assert_eq!(s.started_at, NOW - 120.0);
    assert_eq!(s.last_ts, NOW - 5.0);
}

#[test]
fn since_bound_is_scaled_to_milliseconds() {
    let (_dir, db_path, conn) = open_fixture();
    // archived, so it only appears when time_created >= since * 1000
    let created_ms = 2_000_000_000; // epoch 2_000_000 seconds
    insert_session(
        &conn,
        "ses_old",
        MODEL_JSON,
        created_ms,
        created_ms,
        Some(created_ms),
    );

    let mut src = OpenCodeSource::new(&db_path);
    assert_eq!(src.sessions(1_000_000.0).len(), 1);
    // an unscaled bound (3_000_000 ms) would wrongly include the row
    assert!(src.sessions(3_000_000.0).is_empty());
}

#[test]
fn session_without_messages_has_no_turn_flags() {
    let (_dir, db_path, conn) = open_fixture();
    insert_session(
        &conn,
        "ses_empty",
        MODEL_JSON,
        NOW_MS - 120_000,
        NOW_MS - 5_000,
        None,
    );

    let mut src = OpenCodeSource::new(&db_path);
    let s = &src.sessions(NOW - 3_600.0)[0];
    assert!(!s.turn_done);
    assert!(!s.tool_pending);
    assert_eq!(s.last_line, "");
    assert_eq!(s.last_ts, NOW - 5.0);
}

#[test]
fn newest_message_row_decides_turn_state() {
    let (_dir, db_path, conn) = open_fixture();
    insert_session(
        &conn,
        "ses_mid",
        MODEL_JSON,
        NOW_MS - 120_000,
        NOW_MS - 5_000,
        None,
    );
    insert_message(
        &conn,
        "msg_1",
        "ses_mid",
        NOW_MS - 10_000,
        &msg("assistant", Some("stop")),
    );
    insert_message(
        &conn,
        "msg_2",
        "ses_mid",
        NOW_MS - 5_000,
        &msg("user", None),
    );

    let mut src = OpenCodeSource::new(&db_path);
    let s = &src.sessions(NOW - 3_600.0)[0];
    // the unanswered user message, not the earlier clean stop, is current
    assert!(!s.turn_done);
    assert!(!s.tool_pending);
    assert_eq!(s.last_line, "user");
    assert_eq!(classify(s, NOW, IDLE, FRESH), Liveness::Live);
}

#[test]
fn malformed_json_is_tolerated() {
    let (_dir, db_path, conn) = open_fixture();
    insert_session(
        &conn,
        "ses_bad",
        "not json",
        NOW_MS - 120_000,
        NOW_MS - 5_000,
        None,
    );
    insert_message(&conn, "msg_1", "ses_bad", NOW_MS - 5_000, "{not json");

    let mut src = OpenCodeSource::new(&db_path);
    let s = &src.sessions(NOW - 3_600.0)[0];
    assert_eq!(s.model, "?");
    assert!(!s.turn_done);
    assert!(!s.tool_pending);
    assert_eq!(s.last_line, "");
}

#[test]
fn missing_db_returns_empty() {
    let mut src = OpenCodeSource::new("/nonexistent/dir/opencode.db");
    assert!(src.sessions(0.0).is_empty());
    assert_eq!(src.last_tool("ses_x"), "-");
}

#[test]
fn last_tool_scans_recent_parts() {
    let (_dir, db_path) = live_fixture();
    let conn = Connection::open(&db_path).expect("reopen writable");
    insert_part(
        &conn,
        "prt_1",
        "ses_live01",
        NOW_MS - 4_000,
        r#"{"type":"text","text":"thinking"}"#,
    );
    insert_part(
        &conn,
        "prt_2",
        "ses_live01",
        NOW_MS - 3_000,
        r#"{"type":"tool","tool":"bash","state":{"status":"completed"}}"#,
    );

    let src = OpenCodeSource::new(&db_path);
    assert_eq!(src.last_tool("ses_live01"), "bash");
    assert_eq!(src.last_tool("nope"), "-");
}

#[test]
fn last_tool_scan_is_bounded_to_20_rows() {
    let (_dir, db_path) = live_fixture();
    let conn = Connection::open(&db_path).expect("reopen writable");
    insert_part(
        &conn,
        "prt_tool",
        "ses_live01",
        NOW_MS - 30_000,
        r#"{"type":"tool","tool":"bash","state":{"status":"completed"}}"#,
    );
    for i in 0..20 {
        insert_part(
            &conn,
            &format!("prt_{i}"),
            "ses_live01",
            NOW_MS - 20_000 + i,
            r#"{"type":"text","text":"chatter"}"#,
        );
    }

    let src = OpenCodeSource::new(&db_path);
    // the tool part is the 21st-newest row, past the bounded scan
    assert_eq!(src.last_tool("ses_live01"), "-");
}

/// OpenCode-derived `SessionMeta` feeds the exact same classifier as
/// Hermes, mirroring `tests/test_opencode.py:263`
/// `test_shares_turn_liveness_with_hermes`.
#[test]
fn shares_turn_liveness_with_hermes() {
    let (_dir, db_path) = live_fixture();
    let mut src = OpenCodeSource::new(&db_path);
    let s = &src.sessions(NOW - 3_600.0)[0];
    assert_eq!(classify(s, NOW, IDLE, FRESH), Liveness::Live);
}
