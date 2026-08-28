//! HermesSource acceptance tests: reads a fixture-schema state.db built at
//! `tests/fixtures/hermes_schema.sql` through a writable setup connection,
//! then exercises `HermesSource` (read-only) against it.

mod common;

use std::fs;

use hermon::source::Source;
use hermon::source::hermes::HermesSource;
use rusqlite::Connection;

use common::{fixture_path, temp_db_from_schema};

const NOW: f64 = 1_800_000_000.0;

fn seed(conn: &Connection) {
    conn.execute(
        "INSERT INTO sessions (id, source, model, title, started_at, ended_at,
                                input_tokens, output_tokens, cache_read_tokens,
                                estimated_cost_usd)
         VALUES ('live_sess', 'tui', 'claude-sonnet-5', 'Live Session', ?1, NULL,
                  100, 20, 50, 0.05)",
        [NOW - 600.0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES ('live_sess', 'user', 'please continue', ?1)",
        [NOW - 10.0],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO sessions (id, source, model, started_at, ended_at)
         VALUES ('tool_pending_sess', 'tui', 'claude-sonnet-5', ?1, NULL)",
        [NOW - 600.0],
    )
    .unwrap();
    conn.execute(
        r#"INSERT INTO messages (session_id, role, tool_calls, finish_reason, timestamp)
           VALUES ('tool_pending_sess', 'assistant',
                   '[{"function":{"name":"terminal","arguments":"ls -la"}}]',
                   'tool_calls', ?1)"#,
        [NOW - 100.0],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO sessions (id, source, model, started_at, ended_at)
         VALUES ('done_sess', 'tui', 'claude-sonnet-5', ?1, NULL)",
        [NOW - 800.0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES ('done_sess', 'user', 'run something', ?1)",
        [NOW - 200.0],
    )
    .unwrap();
    conn.execute(
        r#"INSERT INTO messages (session_id, role, tool_name, content, timestamp)
           VALUES ('done_sess', 'tool', 'search', '{"output":"found it","exit_code":0}', ?1)"#,
        [NOW - 150.0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, finish_reason, timestamp)
         VALUES ('done_sess', 'assistant', 'All done.', 'stop', ?1)",
        [NOW - 5.0],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO sessions (id, source, model, started_at, ended_at)
         VALUES ('ended_sess', 'tui', 'claude-sonnet-5', ?1, ?2)",
        [NOW - 5000.0, NOW - 4000.0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, finish_reason, timestamp)
         VALUES ('ended_sess', 'assistant', 'wrapping up', 'stop', ?1)",
        [NOW - 4001.0],
    )
    .unwrap();
}

fn seeded_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let (dir, db_path) = temp_db_from_schema(&fixture_path("hermes_schema.sql"));
    let conn = Connection::open(&db_path).expect("open temp db for seeding");
    seed(&conn);
    conn.close().expect("close seeding connection");
    (dir, db_path)
}

#[test]
fn live_session_reads_correctly() {
    let (_dir, db_path) = seeded_db();
    let mut src = HermesSource::new(db_path.to_str().unwrap());
    let sessions = src.sessions();

    let live = sessions
        .iter()
        .find(|s| s.id == "live_sess")
        .expect("live_sess present");
    assert!(!live.ended);
    assert!(!live.turn_done);
    assert!(!live.tool_pending);
    assert_eq!(live.model, "claude-sonnet-5");
    assert_eq!(live.title, "Live Session");
    assert_eq!(live.in_tok, 150); // input_tokens + cache_read_tokens
    assert_eq!(live.out_tok, 20);
    assert!((live.cost - 0.05).abs() < 1e-9);
    assert_eq!(live.last_ts, NOW - 10.0);
    assert_eq!(live.last_line, "» please continue");
}

#[test]
fn tool_pending_session_reads_correctly() {
    let (_dir, db_path) = seeded_db();
    let mut src = HermesSource::new(db_path.to_str().unwrap());
    let sessions = src.sessions();

    let pending = sessions
        .iter()
        .find(|s| s.id == "tool_pending_sess")
        .expect("tool_pending_sess present");
    assert!(!pending.ended);
    assert!(!pending.turn_done);
    assert!(pending.tool_pending);
    assert_eq!(pending.last_line, "▶ terminal ls -la");
}

#[test]
fn turn_done_session_reads_correctly() {
    let (_dir, db_path) = seeded_db();
    let mut src = HermesSource::new(db_path.to_str().unwrap());
    let sessions = src.sessions();

    let done = sessions
        .iter()
        .find(|s| s.id == "done_sess")
        .expect("done_sess present");
    assert!(!done.ended);
    assert!(done.turn_done);
    assert!(!done.tool_pending);
    assert_eq!(done.last_line, "All done.");

    assert_eq!(src.last_tool("done_sess"), "search");
}

#[test]
fn ended_session_reads_correctly() {
    let (_dir, db_path) = seeded_db();
    let mut src = HermesSource::new(db_path.to_str().unwrap());
    let sessions = src.sessions();

    let ended = sessions
        .iter()
        .find(|s| s.id == "ended_sess")
        .expect("ended_sess present");
    assert!(ended.ended);
}

#[test]
fn last_tool_returns_dash_when_none_found() {
    let (_dir, db_path) = seeded_db();
    let mut src = HermesSource::new(db_path.to_str().unwrap());
    assert_eq!(src.last_tool("live_sess"), "-");
    assert_eq!(src.last_tool("nope"), "-");
}

#[test]
fn missing_db_returns_empty_vec() {
    let mut src = HermesSource::new("/nonexistent/dir/state.db");
    assert_eq!(src.sessions(), Vec::new());
    assert_eq!(src.last_tool("anything"), "-");
}

#[test]
fn sessions_call_does_not_modify_db_mtime() {
    let (_dir, db_path) = seeded_db();
    let before = fs::metadata(&db_path)
        .expect("stat before")
        .modified()
        .expect("mtime before");

    let mut src = HermesSource::new(db_path.to_str().unwrap());
    let _ = src.sessions();

    let after = fs::metadata(&db_path)
        .expect("stat after")
        .modified()
        .expect("mtime after");
    assert_eq!(before, after, "sessions() must not write to the db file");
}
