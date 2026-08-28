//! Engine acceptance tests: the background poll loop against fixture
//! stores, using the real wall clock since [`Engine::spawn`] has no
//! injectable `now` (same trade-off as `tests/roster.rs`'s `ls` binary
//! tests).

mod common;

use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use common::{fixture_path, temp_db_from_schema};
use hermon::config::EngineConfig;
use hermon::engine::{Engine, Event, Lifecycle, UiCmd};

const RECV_TIMEOUT: Duration = Duration::from_secs(2);
const TICK: Duration = Duration::from_millis(50);

fn wall_clock_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_secs_f64()
}

fn config(hermes_db: &std::path::Path) -> EngineConfig {
    EngineConfig {
        claude_dir: "/nonexistent/claude/projects".to_string(),
        hermes_db: hermes_db.display().to_string(),
        opencode_db: "/nonexistent/opencode.db".to_string(),
        idle_timeout: 180.0,
        fresh_window: 300.0,
        interval: TICK,
    }
}

/// A live Hermes session: a user message with no turn-completion signal yet.
fn seed_live_session(conn: &Connection, now: f64) {
    conn.execute(
        "INSERT INTO sessions (id, source, model, title, started_at, ended_at,
                                input_tokens, output_tokens)
         VALUES ('sess_engine1', 'tui', 'claude-sonnet-5', 'Engine test', ?1, NULL, 10, 5)",
        [now - 60.0],
    )
    .expect("insert session");
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES ('sess_engine1', 'user', 'go', ?1)",
        [now - 5.0],
    )
    .expect("insert user message");
}

/// Closes the session's turn: a clean assistant stop with no pending tool
/// call flips `turn_done`, which `classify` reads as `Liveness::Done`
/// immediately, no timeout needed (`src/source/mod.rs turn_liveness_raw`).
fn finish_session(conn: &Connection, now: f64) {
    conn.execute(
        "INSERT INTO messages (session_id, role, content, finish_reason, timestamp)
         VALUES ('sess_engine1', 'assistant', 'done', 'stop', ?1)",
        [now],
    )
    .expect("insert closing message");
}

#[test]
fn engine_emits_roster_then_a_lifecycle_finished_on_turn_done() {
    let (_dir, db_path) = temp_db_from_schema(&fixture_path("hermes_schema.sql"));
    let now = wall_clock_now();
    {
        let conn = Connection::open(&db_path).expect("open seeding connection");
        seed_live_session(&conn, now);
    }

    let (tx, engine_rx) = mpsc::channel();
    let (_ui_tx, rx) = mpsc::channel::<UiCmd>();
    let handle = Engine::spawn(config(&db_path), tx, rx);

    // Roster arrives within a couple of ticks and carries the live session.
    let mut saw_live = false;
    for _ in 0..5 {
        match engine_rx.recv_timeout(RECV_TIMEOUT).expect("roster event") {
            Event::Roster(rows) => {
                if let Some(row) = rows.iter().find(|r| r.key == "H:ngine1") {
                    assert_eq!(row.state, hermon::source::Liveness::Live);
                    saw_live = true;
                    break;
                }
            }
            Event::Lifecycle { .. } | Event::Alert => {}
        }
    }
    assert!(saw_live, "engine never reported the live session");

    // Flip turn_done mid-run; the next tick should see the session finish.
    {
        let conn = Connection::open(&db_path).expect("open mutation connection");
        finish_session(&conn, wall_clock_now());
    }

    let mut finished = false;
    for _ in 0..40 {
        match engine_rx
            .recv_timeout(RECV_TIMEOUT)
            .expect("event after mutation")
        {
            Event::Lifecycle {
                key,
                change: Lifecycle::Finished,
            } if key == "H:ngine1" => {
                finished = true;
                break;
            }
            _ => {}
        }
    }
    assert!(finished, "no Lifecycle::Finished for the closed session");

    _ui_tx.send(UiCmd::Shutdown).expect("send shutdown");
    let start = Instant::now();
    handle.join().expect("engine thread panicked");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "engine took too long to shut down: {:?}",
        start.elapsed()
    );
}

#[test]
fn shutdown_joins_promptly_with_no_sessions() {
    let (tx, engine_rx) = mpsc::channel();
    let (ui_tx, rx) = mpsc::channel();
    let handle = Engine::spawn(
        EngineConfig {
            claude_dir: "/nonexistent/claude/projects".to_string(),
            hermes_db: "/nonexistent/state.db".to_string(),
            opencode_db: "/nonexistent/opencode.db".to_string(),
            idle_timeout: 180.0,
            fresh_window: 300.0,
            interval: TICK,
        },
        tx,
        rx,
    );

    match engine_rx.recv_timeout(RECV_TIMEOUT).expect("roster event") {
        Event::Roster(rows) => assert!(rows.is_empty()),
        other => panic!("expected an empty Roster first, got {other:?}"),
    }

    ui_tx.send(UiCmd::Shutdown).expect("send shutdown");
    let start = Instant::now();
    handle.join().expect("engine thread panicked");
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "shutdown took {:?}, expected well under a tick's timeout slack",
        start.elapsed()
    );
}

/// A UI that hung up (dropped its `Event` receiver) must end the loop via
/// the failing `tx.send`, without ever sending `UiCmd::Shutdown`.
#[test]
fn a_hung_up_ui_ends_the_loop_via_the_failing_send() {
    let (tx, engine_rx) = mpsc::channel();
    let (_ui_tx, rx) = mpsc::channel::<UiCmd>();
    let handle = Engine::spawn(
        EngineConfig {
            claude_dir: "/nonexistent/claude/projects".to_string(),
            hermes_db: "/nonexistent/state.db".to_string(),
            opencode_db: "/nonexistent/opencode.db".to_string(),
            idle_timeout: 180.0,
            fresh_window: 300.0,
            interval: TICK,
        },
        tx,
        rx,
    );
    drop(engine_rx);

    let start = Instant::now();
    handle.join().expect("engine thread panicked");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "engine kept running after its event receiver was dropped: {:?}",
        start.elapsed()
    );
}
