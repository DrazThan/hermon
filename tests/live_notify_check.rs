//! Manual live-delivery smoke tests for #44 — NOT part of the regular
//! `cargo test` gate (every test here is `#[ignore]`). Run explicitly with:
//!
//!   cargo test --test live_notify_check -- --ignored --nocapture
//!
//! These drive the real [`Engine`] (real `notify::probe()`/`deliver()`, so a
//! real banner fires on whatever machine runs them) against a [`FakeDeck`]
//! instead of a real Claude/Hermes/OpenCode store, so the liveness
//! transitions that trigger each alert kind can be forced directly rather
//! than waiting on a real permission prompt or a real `sleep 600`. See
//! `NOTES-44.md` for what this does and does not substitute for.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hermon::config::EngineConfig;
use hermon::engine::{Clock, Deck, Engine, Event, UiCmd};
use hermon::notify::NotifyCfg;
use hermon::roster::RosterRow;
use hermon::source::{Attn, Liveness, Replay, Tailer};

const TICK: Duration = Duration::from_millis(20);
const RECV_TIMEOUT: Duration = Duration::from_secs(2);

fn config() -> EngineConfig {
    EngineConfig {
        claude_dir: "/nonexistent/claude/projects".to_string(),
        hermes_db: "/nonexistent/state.db".to_string(),
        opencode_db: "/nonexistent/opencode.db".to_string(),
        hermes_log: "/nonexistent/agent.log".to_string(),
        idle_timeout: 180.0,
        fresh_window: 1_000_000.0,
        interval: TICK,
        linger: 60.0,
        max_panes: 8,
        notify: NotifyCfg::default(),
    }
}

#[derive(Clone)]
struct FakeClock(Arc<Mutex<f64>>);

impl FakeClock {
    fn new(start: f64) -> Self {
        FakeClock(Arc::new(Mutex::new(start)))
    }
    fn advance(&self, dt: f64) {
        *self.0.lock().unwrap() += dt;
    }
    fn as_clock(&self) -> Clock {
        let inner = self.0.clone();
        Arc::new(move || *inner.lock().unwrap())
    }
}

#[derive(Clone)]
struct FakeDeck(Arc<Mutex<HashMap<String, Liveness>>>);

impl FakeDeck {
    fn new(key: &str, state: Liveness) -> Self {
        FakeDeck(Arc::new(Mutex::new(HashMap::from([(
            key.to_string(),
            state,
        )]))))
    }
    fn set(&self, key: &str, state: Liveness) {
        self.0.lock().unwrap().insert(key.to_string(), state);
    }
}

impl Deck for FakeDeck {
    fn roster(&mut self, now: f64, _fresh_window: f64, _idle_timeout: f64) -> Vec<RosterRow> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(key, &state)| RosterRow {
                id: format!("id-{key}"),
                key: key.clone(),
                state,
                model: "m".to_string(),
                last_tool: "bash".to_string(),
                last_line: String::new(),
                in_tok: 0,
                out_tok: 0,
                cost: 0.11,
                elapsed: Some(20.0),
                last_ts: now,
                title: String::new(),
                attn_elapsed: None,
            })
            .collect()
    }

    fn open_tailer(
        &mut self,
        _key: &str,
        _session_id: &str,
        _replay: Replay,
    ) -> Option<Box<dyn Tailer>> {
        None
    }
}

/// Drains events until an `Event::Alert` shows up or `ticks` scans pass
/// without one. Bounded by wall-clock deadlines per tick, not "wait for
/// real silence" — the fake deck keeps producing Roster/Ticker events every
/// tick forever, so the channel is never actually quiet in the no-alert
/// case.
fn wait_for_alert(rx: &mpsc::Receiver<Event>, clock: &FakeClock, ticks: usize) -> Option<Event> {
    for _ in 0..ticks {
        clock.advance(1.0);
        std::thread::sleep(TICK * 2);
        let deadline = Instant::now() + Duration::from_millis(200);
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(remaining) {
                Ok(Event::Alert(alert)) => return Some(Event::Alert(alert)),
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    }
    None
}

#[test]
#[ignore]
fn live_check_perm_wait_fires_a_real_banner() {
    let deck = FakeDeck::new("C:livepw", Liveness::Live);
    let clock = FakeClock::new(1_000.0);
    let (tx, rx) = mpsc::channel();
    let (_ui_tx, ui_rx) = mpsc::channel();
    let handle = Engine::spawn_with_clock(config(), deck.clone(), clock.as_clock(), tx, ui_rx);

    // First tick: startup grace, no alert possible yet.
    rx.recv_timeout(RECV_TIMEOUT).expect("roster");

    deck.set("C:livepw", Liveness::Attention(Attn::PermWait));
    let alert = wait_for_alert(&rx, &clock, 10);
    println!("perm-wait alert: {alert:?}");
    assert!(alert.is_some(), "expected a PermWait alert to fire");

    _ui_tx.send(UiCmd::Shutdown).ok();
    handle.join().ok();
}

#[test]
#[ignore]
fn live_check_stuck_fires_a_real_banner_after_the_ceiling() {
    let deck = FakeDeck::new("C:livestk", Liveness::Live);
    let clock = FakeClock::new(2_000.0);
    let (tx, rx) = mpsc::channel();
    let (_ui_tx, ui_rx) = mpsc::channel();
    let handle = Engine::spawn_with_clock(config(), deck.clone(), clock.as_clock(), tx, ui_rx);

    rx.recv_timeout(RECV_TIMEOUT).expect("roster");

    deck.set("C:livestk", Liveness::Attention(Attn::Stuck));
    let alert = wait_for_alert(&rx, &clock, 10);
    println!("stuck alert: {alert:?}");
    assert!(alert.is_some(), "expected a Stuck alert to fire");

    _ui_tx.send(UiCmd::Shutdown).ok();
    handle.join().ok();
}

/// A fleet that is already mid-attention on hermon's very first tick — the
/// "restarted hermon while a session was waiting" scenario — must raise
/// zero alerts on that first tick (the startup-grace latch).
#[test]
#[ignore]
fn live_check_restart_mid_fleet_raises_no_banner() {
    let deck = FakeDeck::new("C:restart", Liveness::Attention(Attn::PermWait));
    let clock = FakeClock::new(3_000.0);
    let (tx, rx) = mpsc::channel();
    let (_ui_tx, ui_rx) = mpsc::channel();
    let handle = Engine::spawn_with_clock(config(), deck.clone(), clock.as_clock(), tx, ui_rx);

    let alert = wait_for_alert(&rx, &clock, 5);
    println!("post-restart alert (expect None): {alert:?}");
    assert!(
        alert.is_none(),
        "a restart must not re-alert on old attention state"
    );

    _ui_tx.send(UiCmd::Shutdown).ok();
    handle.join().ok();
}
