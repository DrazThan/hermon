//! HermesTailer acceptance tests: tail a fixture-schema state.db through a
//! writable setup connection while `HermesTailer` reads it read-only, one
//! poll at a time — mirroring how `hermon render H:<key>` drives it.

mod common;

use hermon::render::Sem;
use hermon::source::hermes::HermesTailer;
use hermon::source::{Replay, Source, Tailer};
use rusqlite::Connection;

use common::{fixture_path, temp_db_from_schema};

const NOW: f64 = 1_800_000_000.0;

fn insert_session(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT INTO sessions (id, source, model, started_at, ended_at)
         VALUES (?1, 'tui', 'claude-sonnet-5', ?2, NULL)",
        rusqlite::params![id, NOW - 600.0],
    )
    .unwrap();
}

fn insert_message(conn: &Connection, session_id: &str, role: &str, content: &str) {
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![session_id, role, content, NOW],
    )
    .unwrap();
}

#[test]
fn new_rows_arrive_exactly_once_and_in_id_order() {
    let (_dir, db_path) = temp_db_from_schema(&fixture_path("hermes_schema.sql"));
    let conn = Connection::open(&db_path).unwrap();
    insert_session(&conn, "sess1");

    let mut tailer = HermesTailer::new(db_path.clone(), "sess1".to_string(), 40);

    // Nothing yet: first poll is just the replay window (empty).
    let first = tailer.poll();
    assert!(first.is_empty(), "{first:?}");

    insert_message(&conn, "sess1", "user", "one");
    insert_message(&conn, "sess1", "assistant", "two");

    let second = tailer.poll();
    let plain: Vec<String> = second.iter().map(|l| l.to_plain()).collect();
    assert_eq!(plain.len(), 2, "{plain:?}");
    assert!(plain[0].contains("one"), "{plain:?}");
    assert!(plain[1].contains("two"), "{plain:?}");

    // No new rows: quiet poll.
    let third = tailer.poll();
    assert!(third.is_empty(), "{third:?}");

    insert_message(&conn, "sess1", "user", "three");
    let fourth = tailer.poll();
    let plain: Vec<String> = fourth.iter().map(|l| l.to_plain()).collect();
    assert_eq!(plain.len(), 1);
    assert!(plain[0].contains("three"), "{plain:?}");
}

#[test]
fn replay_seeds_only_the_last_n_rows() {
    let (_dir, db_path) = temp_db_from_schema(&fixture_path("hermes_schema.sql"));
    let conn = Connection::open(&db_path).unwrap();
    insert_session(&conn, "sess1");
    for i in 0..10 {
        insert_message(&conn, "sess1", "user", &format!("msg{i}"));
    }

    let mut tailer = HermesTailer::new(db_path.clone(), "sess1".to_string(), 3);
    let lines = tailer.poll();
    let plain: Vec<String> = lines.iter().map(|l| l.to_plain()).collect();

    assert_eq!(plain.len(), 3, "{plain:?}");
    assert!(plain[0].contains("msg7"), "{plain:?}");
    assert!(plain[1].contains("msg8"), "{plain:?}");
    assert!(plain[2].contains("msg9"), "{plain:?}");

    // Nothing new since: quiet poll.
    assert!(tailer.poll().is_empty());
}

#[test]
fn ended_session_closes_once_then_goes_quiescent() {
    let (_dir, db_path) = temp_db_from_schema(&fixture_path("hermes_schema.sql"));
    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO sessions (id, source, model, started_at, ended_at,
                                input_tokens, output_tokens)
         VALUES ('sess1', 'tui', 'claude-sonnet-5', ?1, ?2, 10, 5)",
        rusqlite::params![NOW - 600.0, NOW - 10.0],
    )
    .unwrap();
    insert_message(&conn, "sess1", "assistant", "wrapping up");

    let mut tailer = HermesTailer::new(db_path.clone(), "sess1".to_string(), 40);
    let first = tailer.poll();
    let ended: Vec<&hermon::render::StyledLine> = first
        .iter()
        .filter(|l| l.to_plain().contains("Σ session ended"))
        .collect();
    assert_eq!(ended.len(), 1, "{first:?}");
    assert_eq!(ended[0].0[0].sem, Sem::Ok);

    // More rows land after the session closed; the tailer must not surface them.
    insert_message(&conn, "sess1", "user", "should never be seen");
    let second = tailer.poll();
    assert!(second.is_empty(), "{second:?}");
    let third = tailer.poll();
    assert!(third.is_empty(), "{third:?}");
}

#[test]
fn missing_db_warns_once_then_self_heals() {
    let (dir, db_path) = temp_db_from_schema(&fixture_path("hermes_schema.sql"));
    let missing_path = dir.path().join("not_there_yet.db");

    let mut tailer = HermesTailer::new(missing_path.clone(), "sess1".to_string(), 40);

    let first = tailer.poll();
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].to_plain().contains("hermes db unavailable"));
    assert_eq!(first[0].0[0].sem, Sem::Dim);

    // Still missing: no repeated warning.
    let second = tailer.poll();
    assert!(second.is_empty(), "{second:?}");

    // The store appears (rename the already-schema'd temp db into place).
    std::fs::rename(&db_path, &missing_path).unwrap();
    let conn = Connection::open(&missing_path).unwrap();
    insert_session(&conn, "sess1");
    insert_message(&conn, "sess1", "user", "hello again");

    let third = tailer.poll();
    let plain: Vec<String> = third.iter().map(|l| l.to_plain()).collect();
    assert!(plain.iter().any(|l| l.contains("hello again")), "{plain:?}");
    assert!(
        !plain.iter().any(|l| l.contains("unavailable")),
        "{plain:?}"
    );
}

#[test]
fn hermes_source_open_tailer_returns_a_tailer() {
    let (_dir, db_path) = temp_db_from_schema(&fixture_path("hermes_schema.sql"));
    let conn = Connection::open(&db_path).unwrap();
    insert_session(&conn, "sess1");
    insert_message(&conn, "sess1", "user", "hi");

    let src = hermon::source::hermes::HermesSource::new(db_path.to_str().unwrap());
    let mut tailer = src
        .open_tailer("sess1", Replay::DEFAULT)
        .expect("hermes source always yields a tailer");
    let lines = tailer.poll();
    assert!(lines.iter().any(|l| l.to_plain().contains("hi")));
}
