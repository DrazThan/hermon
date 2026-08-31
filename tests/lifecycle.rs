//! Full-engine lifecycle walks: linger, resurrection, and max-panes
//! eviction, driven end to end through [`Engine::spawn_with_clock`] rather
//! than by calling the state-machine helpers directly (those have their own
//! unit tests in `src/engine.rs`). A [`FakeClock`] the test steps by hand
//! stands in for wall-clock seconds, so a 60-second linger is provable
//! without a 60-second sleep; [`FakeDeck`] holds a handful of sessions the
//! test can flip live/done/gone at will.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hermon::config::EngineConfig;
use hermon::engine::{Cause, Clock, Deck, Engine, Event, Lifecycle, PANE_TICK, UiCmd};
use hermon::notify::NotifyCfg;
use hermon::render::{Seg, Sem, StyledLine};
use hermon::roster::RosterRow;
use hermon::source::{Liveness, Replay, Tailer};

const TICK: Duration = Duration::from_millis(15);
const RECV_TIMEOUT: Duration = Duration::from_secs(2);
/// Long enough to span several [`PANE_TICK`]s — the cadence pane polling
/// actually runs on, independent of the scan `TICK` these tests configure —
/// so a "the pane stopped" or "the pane kept going" check isn't racing the
/// engine's own fast-tick timer.
const SETTLE: Duration = Duration::from_millis(PANE_TICK.as_millis() as u64 * 3);

fn config(linger: f64, max_panes: usize) -> EngineConfig {
    EngineConfig {
        claude_dir: "/nonexistent/claude/projects".to_string(),
        hermes_db: "/nonexistent/state.db".to_string(),
        opencode_db: "/nonexistent/opencode.db".to_string(),
        hermes_log: "/nonexistent/agent.log".to_string(),
        idle_timeout: 180.0,
        // Wide enough that the roster's own fresh-window row-dropping never
        // interferes — these tests are about the engine's linger clock, not
        // roster.rs's.
        fresh_window: 1_000_000.0,
        interval: TICK,
        linger,
        max_panes,
        notify: NotifyCfg::default(),
        replay: Replay::DEFAULT,
        remotes: Vec::new(),
        remote_flags: Vec::new(),
        docker_auto: false,
    }
}

/// A wall clock the test steps by hand.
#[derive(Clone)]
struct FakeClock(Arc<Mutex<f64>>);

impl FakeClock {
    fn new(start: f64) -> Self {
        FakeClock(Arc::new(Mutex::new(start)))
    }

    fn advance(&self, dt: f64) {
        let mut guard = self.0.lock().expect("clock lock");
        *guard += dt;
    }

    fn as_clock(&self) -> Clock {
        let inner = self.0.clone();
        Arc::new(move || *inner.lock().expect("clock lock"))
    }
}

struct StubTailer;

impl Tailer for StubTailer {
    fn poll(&mut self) -> Vec<StyledLine> {
        vec![StyledLine(vec![Seg::new(Sem::Plain, "line")])]
    }
}

/// A handful of sessions the test drives directly — no fixture store, no
/// `classify`. Each session's liveness is `None` while it is gone from
/// every source.
#[derive(Clone)]
struct FakeDeck {
    sessions: Arc<Mutex<HashMap<String, Option<Liveness>>>>,
    /// Every `open_tailer` call, in order — proves which key(s) actually
    /// got (re)opened, and how many times.
    opens: Arc<Mutex<Vec<String>>>,
}

impl FakeDeck {
    fn new(keys: &[&str]) -> Self {
        let sessions = keys
            .iter()
            .map(|k| (k.to_string(), Some(Liveness::Live)))
            .collect();
        FakeDeck {
            sessions: Arc::new(Mutex::new(sessions)),
            opens: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn set(&self, key: &str, state: Liveness) {
        self.sessions
            .lock()
            .expect("sessions lock")
            .insert(key.to_string(), Some(state));
    }

    fn vanish(&self, key: &str) {
        self.sessions
            .lock()
            .expect("sessions lock")
            .insert(key.to_string(), None);
    }

    fn open_count(&self, key: &str) -> usize {
        self.opens
            .lock()
            .expect("opens lock")
            .iter()
            .filter(|k| k.as_str() == key)
            .count()
    }
}

impl Deck for FakeDeck {
    fn roster(&mut self, now: f64, _fresh_window: f64, _idle_timeout: f64) -> Vec<RosterRow> {
        self.sessions
            .lock()
            .expect("sessions lock")
            .iter()
            .filter_map(|(key, state)| {
                let state = (*state)?;
                Some(RosterRow {
                    id: format!("id-{key}"),
                    key: key.clone(),
                    state,
                    model: "m".to_string(),
                    last_tool: "-".to_string(),
                    last_line: String::new(),
                    in_tok: 0,
                    out_tok: 0,
                    cost: Some(0.0),
                    elapsed: None,
                    last_ts: now,
                    title: String::new(),
                    attn_elapsed: None,
                })
            })
            .collect()
    }

    fn open_tailer(
        &mut self,
        key: &str,
        _session_id: &str,
        _replay: Replay,
    ) -> Option<Box<dyn Tailer>> {
        self.opens.lock().expect("opens lock").push(key.to_string());
        Some(Box::new(StubTailer))
    }
}

/// Blocks until `pred` matches an event, or `timeout` elapses.
fn wait_for(rx: &mpsc::Receiver<Event>, timeout: Duration, pred: impl Fn(&Event) -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match rx.recv_timeout(remaining) {
            Ok(event) if pred(&event) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

fn wait_for_lifecycle(rx: &mpsc::Receiver<Event>, key: &str, change: Lifecycle) -> bool {
    wait_for(
        rx,
        RECV_TIMEOUT,
        |event| matches!(event, Event::Lifecycle { key: k, change: c } if k == key && *c == change),
    )
}

/// Drains whatever is already queued, without waiting for more.
fn drain(rx: &mpsc::Receiver<Event>) {
    for _ in rx.try_iter() {}
}

#[test]
fn full_walk_live_to_done_to_linger_to_closed_to_resurrected() {
    let clock = FakeClock::new(1_000.0);
    let deck = FakeDeck::new(&["a"]);
    let (tx, rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();
    let handle =
        Engine::spawn_with_clock(config(60.0, 8), deck.clone(), clock.as_clock(), tx, ui_rx);

    assert!(
        wait_for(&rx, RECV_TIMEOUT, |e| matches!(e,
            Event::Roster(rows) if rows.iter().any(|r| r.key == "a" && r.state == Liveness::Live)
        )),
        "session never showed up live"
    );
    ui_tx
        .send(UiCmd::OpenPane("a".to_string()))
        .expect("send OpenPane");
    assert!(
        wait_for(
            &rx,
            RECV_TIMEOUT,
            |e| matches!(e, Event::PaneLines { key, .. } if key == "a")
        ),
        "pane never streamed after OpenPane"
    );

    // Live -> Done.
    deck.set("a", Liveness::Done);
    assert!(
        wait_for_lifecycle(&rx, "a", Lifecycle::Finished(Cause::Clean)),
        "no Lifecycle::Finished on turning done"
    );

    // Still within the linger window: the pane keeps streaming.
    assert!(
        wait_for(
            &rx,
            RECV_TIMEOUT,
            |e| matches!(e, Event::PaneLines { key, .. } if key == "a")
        ),
        "pane stopped before its linger expired"
    );

    // Past the 60s linger: the engine closes the tailer on its own.
    clock.advance(61.0);
    thread::sleep(SETTLE);
    drain(&rx);
    thread::sleep(SETTLE);
    let stray: Vec<Event> = rx.try_iter().collect();
    assert!(
        !stray
            .iter()
            .any(|e| matches!(e, Event::PaneLines { key, .. } if key == "a")),
        "pane kept streaming past its linger: {stray:?}"
    );

    // Done -> Live: the engine reopens the pane on its own, no OpenPane
    // needed — the UI never gave the key back.
    deck.set("a", Liveness::Live);
    assert!(
        wait_for_lifecycle(&rx, "a", Lifecycle::Resumed),
        "no Lifecycle::Resumed on resurrection"
    );
    assert!(
        wait_for(
            &rx,
            RECV_TIMEOUT,
            |e| matches!(e, Event::PaneLines { key, .. } if key == "a")
        ),
        "pane never resumed streaming after resurrection"
    );
    assert_eq!(
        deck.open_count("a"),
        2,
        "opened once fresh, once on resurrection"
    );

    ui_tx.send(UiCmd::Shutdown).expect("send shutdown");
    handle.join().expect("engine thread panicked");
}

#[test]
fn linger_zero_keeps_a_finished_pane_open_forever() {
    let clock = FakeClock::new(1_000.0);
    let deck = FakeDeck::new(&["a"]);
    let (tx, rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();
    let handle =
        Engine::spawn_with_clock(config(0.0, 8), deck.clone(), clock.as_clock(), tx, ui_rx);

    assert!(wait_for(&rx, RECV_TIMEOUT, |e| matches!(
        e,
        Event::Roster(_)
    )));
    ui_tx
        .send(UiCmd::OpenPane("a".to_string()))
        .expect("send OpenPane");
    assert!(wait_for(
        &rx,
        RECV_TIMEOUT,
        |e| matches!(e, Event::PaneLines { key, .. } if key == "a")
    ));

    deck.set("a", Liveness::Done);
    assert!(wait_for_lifecycle(
        &rx,
        "a",
        Lifecycle::Finished(Cause::Clean)
    ));

    // A linger longer than any real session would ever wait.
    clock.advance(1_000_000.0);
    thread::sleep(SETTLE);
    drain(&rx);

    assert!(
        wait_for(
            &rx,
            RECV_TIMEOUT,
            |e| matches!(e, Event::PaneLines { key, .. } if key == "a")
        ),
        "linger=0 should keep the pane open no matter how much time passes"
    );

    ui_tx.send(UiCmd::Shutdown).expect("send shutdown");
    handle.join().expect("engine thread panicked");
}

#[test]
fn eviction_picks_the_oldest_finished_pane() {
    let clock = FakeClock::new(1_000.0);
    let deck = FakeDeck::new(&["a", "b", "c"]);
    let (tx, rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();
    // linger=0 isolates eviction from linger: a and b only ever lose their
    // slot by being evicted, never by aging out.
    let handle =
        Engine::spawn_with_clock(config(0.0, 2), deck.clone(), clock.as_clock(), tx, ui_rx);

    assert!(wait_for(&rx, RECV_TIMEOUT, |e| matches!(
        e,
        Event::Roster(_)
    )));
    ui_tx
        .send(UiCmd::OpenPane("a".to_string()))
        .expect("send OpenPane");
    assert!(wait_for(
        &rx,
        RECV_TIMEOUT,
        |e| matches!(e, Event::PaneLines { key, .. } if key == "a")
    ));
    ui_tx
        .send(UiCmd::OpenPane("b".to_string()))
        .expect("send OpenPane");
    assert!(wait_for(
        &rx,
        RECV_TIMEOUT,
        |e| matches!(e, Event::PaneLines { key, .. } if key == "b")
    ));

    // a finishes first, then b — a is the older finish.
    deck.set("a", Liveness::Done);
    assert!(wait_for_lifecycle(
        &rx,
        "a",
        Lifecycle::Finished(Cause::Clean)
    ));
    clock.advance(10.0);
    deck.set("b", Liveness::Done);
    assert!(wait_for_lifecycle(
        &rx,
        "b",
        Lifecycle::Finished(Cause::Clean)
    ));

    // Both slots are taken and full (max_panes=2); c wanting one evicts the
    // older finish, a, and leaves b's pane alone.
    ui_tx
        .send(UiCmd::OpenPane("c".to_string()))
        .expect("send OpenPane");
    assert!(
        wait_for_lifecycle(&rx, "a", Lifecycle::Evicted),
        "a (the older finish) should have been evicted"
    );
    assert!(
        wait_for(
            &rx,
            RECV_TIMEOUT,
            |e| matches!(e, Event::PaneLines { key, .. } if key == "c")
        ),
        "c never got its pane after eviction made room"
    );

    thread::sleep(SETTLE);
    drain(&rx);
    thread::sleep(SETTLE);
    let stray: Vec<Event> = rx.try_iter().collect();
    assert!(
        !stray
            .iter()
            .any(|e| matches!(e, Event::PaneLines { key, .. } if key == "a")),
        "evicted pane a kept streaming: {stray:?}"
    );
    assert!(
        stray
            .iter()
            .any(|e| matches!(e, Event::PaneLines { key, .. } if key == "b")),
        "b's pane should have survived the eviction untouched"
    );

    ui_tx.send(UiCmd::Shutdown).expect("send shutdown");
    handle.join().expect("engine thread panicked");
}

#[test]
fn an_all_live_fleet_at_the_cap_evicts_nobody() {
    let clock = FakeClock::new(1_000.0);
    let deck = FakeDeck::new(&["a", "b"]);
    let (tx, rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();
    let handle =
        Engine::spawn_with_clock(config(60.0, 1), deck.clone(), clock.as_clock(), tx, ui_rx);

    assert!(wait_for(&rx, RECV_TIMEOUT, |e| matches!(
        e,
        Event::Roster(_)
    )));
    ui_tx
        .send(UiCmd::OpenPane("a".to_string()))
        .expect("send OpenPane");
    assert!(wait_for(
        &rx,
        RECV_TIMEOUT,
        |e| matches!(e, Event::PaneLines { key, .. } if key == "a")
    ));

    // b is live too, and max_panes=1 is already spent on a: b has to wait,
    // and never bumps a — the roster still lists it either way.
    ui_tx
        .send(UiCmd::OpenPane("b".to_string()))
        .expect("send OpenPane");
    thread::sleep(SETTLE);
    drain(&rx);
    thread::sleep(SETTLE);
    let stray: Vec<Event> = rx.try_iter().collect();

    assert!(
        !stray.iter().any(|e| matches!(
            e,
            Event::Lifecycle {
                change: Lifecycle::Evicted,
                ..
            }
        )),
        "an all-live fleet must not evict anyone: {stray:?}"
    );
    assert!(
        !stray
            .iter()
            .any(|e| matches!(e, Event::PaneLines { key, .. } if key == "b")),
        "b should never have opened while a holds the only slot"
    );
    assert!(
        stray
            .iter()
            .any(|e| matches!(e, Event::Roster(rows) if rows.iter().any(|r| r.key == "b"))),
        "b must still be on the roster despite having no pane"
    );
    assert_eq!(
        deck.open_count("b"),
        0,
        "the deck was never even asked to tail b"
    );

    ui_tx.send(UiCmd::Shutdown).expect("send shutdown");
    handle.join().expect("engine thread panicked");
}

/// A session gone from every source (not merely turned done) is treated as
/// an implicit finish, then forgotten once both it is killed (its linger
/// expired) and still absent. Forgetting is only observable indirectly: the
/// same key reappearing afterwards reads as a brand-new [`Lifecycle::Started`]
/// rather than a [`Lifecycle::Resumed`], because nothing remembers it anymore.
#[test]
fn a_session_gone_from_every_source_is_forgotten_after_its_own_linger() {
    let clock = FakeClock::new(1_000.0);
    let deck = FakeDeck::new(&["a"]);
    let (tx, rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();
    let handle =
        Engine::spawn_with_clock(config(60.0, 8), deck.clone(), clock.as_clock(), tx, ui_rx);

    assert!(wait_for(&rx, RECV_TIMEOUT, |e| matches!(
        e,
        Event::Roster(_)
    )));
    ui_tx
        .send(UiCmd::OpenPane("a".to_string()))
        .expect("send OpenPane");
    assert!(wait_for(
        &rx,
        RECV_TIMEOUT,
        |e| matches!(e, Event::PaneLines { key, .. } if key == "a")
    ));

    // Gone from every source, with no explicit Done in between.
    deck.vanish("a");
    assert!(
        wait_for_lifecycle(&rx, "a", Lifecycle::Finished(Cause::Clean)),
        "a vanished session should read as an implicit finish"
    );

    clock.advance(61.0);
    thread::sleep(SETTLE);
    drain(&rx);
    thread::sleep(SETTLE);
    drain(&rx);

    // The same key returns; if it were merely lingering-killed this would
    // be a Resumed, but forgetting means it starts over as new.
    deck.set("a", Liveness::Live);
    assert!(
        wait_for_lifecycle(&rx, "a", Lifecycle::Started),
        "a forgotten key should read as freshly started, not resumed"
    );

    ui_tx.send(UiCmd::Shutdown).expect("send shutdown");
    handle.join().expect("engine thread panicked");
}
