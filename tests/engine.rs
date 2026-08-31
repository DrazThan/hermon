//! Engine acceptance tests: the background poll loop against fixture
//! stores, using the real wall clock since [`Engine::spawn`] has no
//! injectable `now` (same trade-off as `tests/roster.rs`'s `ls` binary
//! tests).

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use common::{fixture_path, temp_db_from_schema};
use hermon::config::EngineConfig;
use hermon::engine::{Deck, Engine, Event, Lifecycle, PANE_TICK, UiCmd};
use hermon::notify::NotifyCfg;
use hermon::render::{Seg, Sem, StyledLine};
use hermon::roster::RosterRow;
use hermon::source::{Liveness, Replay, Tailer};

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
        hermes_log: "/nonexistent/agent.log".to_string(),
        idle_timeout: 180.0,
        fresh_window: 300.0,
        interval: TICK,
        linger: 60.0,
        max_panes: 8,
        notify: NotifyCfg::default(),
        replay: Replay::DEFAULT,
        remotes: Vec::new(),
        remote_flags: Vec::new(),
        docker_auto: false,
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
            Event::Ticker(_)
            | Event::Lifecycle { .. }
            | Event::PaneLines { .. }
            | Event::Alert(_) => {}
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
                change: Lifecycle::Finished(_),
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
            hermes_log: "/nonexistent/agent.log".to_string(),
            idle_timeout: 180.0,
            fresh_window: 300.0,
            interval: TICK,
            linger: 60.0,
            max_panes: 8,
            notify: NotifyCfg::default(),
            replay: Replay::DEFAULT,
            remotes: Vec::new(),
            remote_flags: Vec::new(),
            docker_auto: false,
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
            hermes_log: "/nonexistent/agent.log".to_string(),
            idle_timeout: 180.0,
            fresh_window: 300.0,
            interval: TICK,
            linger: 60.0,
            max_panes: 8,
            notify: NotifyCfg::default(),
            replay: Replay::DEFAULT,
            remotes: Vec::new(),
            remote_flags: Vec::new(),
            docker_auto: false,
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

// ------------------------------------------------------------ pane tailing

const PANE_KEY: &str = "C:aaaaaa";
const PANE_SESSION_ID: &str = "session-aaaaaa";

/// A tailer that emits one line per poll and counts its polls, so a test
/// can prove a closed pane really does go quiet.
struct FakeTailer {
    polls: Arc<AtomicUsize>,
}

impl Tailer for FakeTailer {
    fn poll(&mut self) -> Vec<StyledLine> {
        let n = self.polls.fetch_add(1, Ordering::SeqCst) + 1;
        vec![StyledLine(vec![Seg::new(Sem::Plain, format!("line {n}"))])]
    }
}

/// One always-live session tailed by [`FakeTailer`] — the pane path with
/// no store on disk. Every `open_tailer` call is recorded for the test to
/// check afterwards rather than asserted on the engine thread, where a
/// panic would only show up as a failed join.
struct FakeDeck {
    opens: Arc<Mutex<Vec<(String, String, Replay)>>>,
    polls: Arc<AtomicUsize>,
}

impl FakeDeck {
    fn new() -> Self {
        FakeDeck {
            opens: Arc::new(Mutex::new(Vec::new())),
            polls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Deck for FakeDeck {
    fn roster(&mut self, now: f64, _fresh_window: f64, _idle_timeout: f64) -> Vec<RosterRow> {
        vec![RosterRow {
            id: PANE_SESSION_ID.to_string(),
            key: PANE_KEY.to_string(),
            state: Liveness::Live,
            model: "claude-sonnet-5".to_string(),
            last_tool: "-".to_string(),
            last_line: String::new(),
            in_tok: 0,
            out_tok: 0,
            cost: Some(0.0),
            elapsed: None,
            last_ts: now,
            title: String::new(),
            attn_elapsed: None,
        }]
    }

    fn open_tailer(
        &mut self,
        key: &str,
        session_id: &str,
        replay: Replay,
    ) -> Option<Box<dyn Tailer>> {
        self.opens.lock().expect("opens lock").push((
            key.to_string(),
            session_id.to_string(),
            replay,
        ));
        Some(Box::new(FakeTailer {
            polls: self.polls.clone(),
        }))
    }
}

/// Scans slowly on purpose: the pane cadence is what these tests watch, and
/// a fast scan would bury it in roster events.
fn pane_config() -> EngineConfig {
    EngineConfig {
        claude_dir: "/nonexistent/claude/projects".to_string(),
        hermes_db: "/nonexistent/state.db".to_string(),
        opencode_db: "/nonexistent/opencode.db".to_string(),
        hermes_log: "/nonexistent/agent.log".to_string(),
        idle_timeout: 180.0,
        fresh_window: 300.0,
        interval: Duration::from_millis(500),
        linger: 60.0,
        max_panes: 8,
        notify: NotifyCfg::default(),
        replay: Replay::DEFAULT,
        remotes: Vec::new(),
        remote_flags: Vec::new(),
        docker_auto: false,
    }
}

/// The next [`Event::PaneLines`], ignoring the roster traffic around it.
/// The deadline is many fast ticks wide — this asserts "promptly", not a
/// tick count.
fn next_pane_lines(rx: &Receiver<Event>) -> Option<(String, Vec<StyledLine>)> {
    let deadline = Instant::now() + RECV_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match rx.recv_timeout(remaining) {
            Ok(Event::PaneLines { key, lines }) => return Some((key, lines)),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

#[test]
fn opening_a_pane_streams_its_tailer_lines() {
    let deck = FakeDeck::new();
    let opens = deck.opens.clone();
    let (tx, ev_rx) = mpsc::channel();
    let (ui_tx, rx) = mpsc::channel();
    let handle = Engine::spawn_with(pane_config(), deck, tx, rx);

    ui_tx
        .send(UiCmd::OpenPane(PANE_KEY.to_string()))
        .expect("send OpenPane");

    let (key, lines) = next_pane_lines(&ev_rx).expect("no PaneLines after OpenPane");
    assert_eq!(key, PANE_KEY);
    assert_eq!(lines[0].to_plain(), "line 1", "the replay poll comes first");

    // The pane keeps streaming on later fast ticks, not just once on open.
    let (_, lines) = next_pane_lines(&ev_rx).expect("no PaneLines on a later tick");
    assert_eq!(lines[0].to_plain(), "line 2");

    // Opening is once per pane, with the row's full session id — the key
    // carries only a shortened one — and the default replay window.
    assert_eq!(
        *opens.lock().expect("opens lock"),
        vec![(
            PANE_KEY.to_string(),
            PANE_SESSION_ID.to_string(),
            Replay::DEFAULT
        )]
    );

    ui_tx.send(UiCmd::Shutdown).expect("send shutdown");
    handle.join().expect("engine thread panicked");
}

#[test]
fn a_closed_pane_is_not_polled_again() {
    let deck = FakeDeck::new();
    let polls = deck.polls.clone();
    let (tx, ev_rx) = mpsc::channel();
    let (ui_tx, rx) = mpsc::channel();
    let handle = Engine::spawn_with(pane_config(), deck, tx, rx);

    ui_tx
        .send(UiCmd::OpenPane(PANE_KEY.to_string()))
        .expect("send OpenPane");
    next_pane_lines(&ev_rx).expect("no PaneLines after OpenPane");

    ui_tx
        .send(UiCmd::ClosePane(PANE_KEY.to_string()))
        .expect("send ClosePane");

    // Let the close settle, drop whatever was already in flight, then take
    // a reading and prove nothing moves after it.
    thread::sleep(4 * PANE_TICK);
    for _ in ev_rx.try_iter() {}
    let settled = polls.load(Ordering::SeqCst);

    thread::sleep(4 * PANE_TICK);
    assert_eq!(
        polls.load(Ordering::SeqCst),
        settled,
        "a closed pane is still being polled"
    );
    let stray: Vec<Event> = ev_rx.try_iter().collect();
    assert!(
        !stray.iter().any(|e| matches!(e, Event::PaneLines { .. })),
        "lines kept arriving after ClosePane: {stray:?}"
    );

    ui_tx.send(UiCmd::Shutdown).expect("send shutdown");
    handle.join().expect("engine thread panicked");
}

/// A key no session answers to (a pane opened for a session that has since
/// left the deck) opens nothing at all, rather than tailing the wrong one.
#[test]
fn a_key_that_is_not_on_the_deck_opens_nothing() {
    let deck = FakeDeck::new();
    let opens = deck.opens.clone();
    let polls = deck.polls.clone();
    let (tx, ev_rx) = mpsc::channel();
    let (ui_tx, rx) = mpsc::channel();
    let handle = Engine::spawn_with(pane_config(), deck, tx, rx);

    ui_tx
        .send(UiCmd::OpenPane("Z:nosuch".to_string()))
        .expect("send OpenPane");
    thread::sleep(4 * PANE_TICK);

    assert!(opens.lock().expect("opens lock").is_empty());
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    let seen: Vec<Event> = ev_rx.try_iter().collect();
    assert!(
        !seen.iter().any(|e| matches!(e, Event::PaneLines { .. })),
        "an unknown key produced pane lines: {seen:?}"
    );

    ui_tx.send(UiCmd::Shutdown).expect("send shutdown");
    handle.join().expect("engine thread panicked");
}
