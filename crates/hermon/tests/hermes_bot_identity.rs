//! Pre-feature identity behavior through public Engine, TUI, and notification
//! seams only. Synthetic hr1 ids may share a human alias. GUI private
//! sync_panes / toggle_pin are not called; visible GUI checks are smoke.
//!
//! Recognition these tests enforce: `row.id.starts_with("hr1:")` routes as
//! `row.id`, otherwise as `row.key`. A malformed hr1 prefix is not a legacy
//! alias. Fake decks record calls; they do not admit filesystem routes.

use std::collections::HashSet;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hermon::config::EngineConfig;
use hermon::engine::{Clock, Deck, Engine, Event, Lifecycle, UiCmd};
use hermon::notify::{AlertHistory, AlertKind, LifecycleTransition, NotifyCfg, decide_alerts};
use hermon::render::{Seg, Sem, StyledLine};
use hermon::roster::RosterRow;
use hermon::source::{Liveness, Replay, Tailer};
use hermon::ui::{App, ViewMode};

const NOW: f64 = 10_000.0;
const ALIAS: &str = "H:abcdef";

fn hr1(store: &str, vendor: &str) -> String {
    fn hex(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::new();
        for byte in bytes {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
    format!(
        "hr1::H:{}:{}",
        hex(store.as_bytes()),
        hex(vendor.as_bytes())
    )
}

fn row(id: &str, key: &str, state: Liveness) -> RosterRow {
    RosterRow {
        id: id.to_string(),
        key: key.to_string(),
        state,
        model: "m".to_string(),
        last_tool: "-".to_string(),
        last_line: String::new(),
        in_tok: 0,
        out_tok: 0,
        cost: Some(1.0),
        elapsed: Some(60.0),
        last_ts: NOW,
        title: id.to_string(),
        attn_elapsed: None,
    }
}

fn colliding() -> (String, String, Vec<RosterRow>) {
    let a = hr1("store-a", "session-abcdef");
    let b = hr1("store-b", "session-abcdef");
    let rows = vec![
        row(&a, ALIAS, Liveness::Live),
        row(&b, ALIAS, Liveness::Live),
    ];
    (a, b, rows)
}

fn line(text: &str) -> StyledLine {
    StyledLine(vec![Seg::new(Sem::Plain, text)])
}

fn press(app: &mut App, c: char) {
    app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

fn open_panes(cmds: &[UiCmd]) -> Vec<String> {
    cmds.iter()
        .filter_map(|cmd| match cmd {
            UiCmd::OpenPane(key) => Some(key.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn legacy_colon_id_still_commands_as_row_key() {
    let mut app = App::default();
    let rows = vec![row("C:aaa-id", "C:aaa", Liveness::Live)];
    app.selected_id = Some("C:aaa-id".to_string());
    app.apply_event(Event::Roster(rows));
    let cmds = app.take_commands();
    assert_eq!(
        open_panes(&cmds),
        vec!["C:aaa".to_string()],
        "unqualified C:aaa-id must keep commanding as C:aaa, not as the raw id"
    );
    press(&mut app, 'p');
    let pinned = app
        .take_commands()
        .into_iter()
        .find_map(|cmd| match cmd {
            UiCmd::Pinned(set) => Some(set),
            _ => None,
        })
        .expect("pin command");
    assert_eq!(pinned, HashSet::from(["C:aaa".to_string()]));
}

#[test]
fn qualified_list_and_grid_panes_are_independent_of_a_shared_alias() {
    let (a, b, rows) = colliding();
    let mut app = App::default();
    app.selected_id = Some(a.clone());
    app.apply_event(Event::Roster(rows.clone()));
    let listed = open_panes(&app.take_commands());
    assert_eq!(
        listed,
        vec![a.clone()],
        "list mode must open the hr1 id, not the shared alias {ALIAS}"
    );

    let mut grid = App::default();
    grid.mode = ViewMode::Grid;
    grid.apply_event(Event::Roster(rows));
    let opened: HashSet<String> = open_panes(&grid.take_commands()).into_iter().collect();
    assert!(
        opened.contains(&a) && opened.contains(&b),
        "grid/zoom slots must use each hr1 id, got {opened:?}"
    );
    assert!(
        !opened.contains(ALIAS),
        "shared alias must not be the pane command, got {opened:?}"
    );
}

#[test]
fn qualified_pin_and_buffers_do_not_merge_on_the_alias() {
    let (a, b, rows) = colliding();
    let mut app = App::default();
    app.mode = ViewMode::Grid;
    app.selected_id = Some(a.clone());
    app.apply_event(Event::Roster(rows));
    let _ = app.take_commands();
    press(&mut app, 'p');
    let pinned = app
        .take_commands()
        .into_iter()
        .find_map(|cmd| match cmd {
            UiCmd::Pinned(set) => Some(set),
            _ => None,
        })
        .expect("pin command");
    assert_eq!(
        pinned,
        HashSet::from([a.clone()]),
        "pin translation must use the selected hr1 id, not {ALIAS}"
    );

    app.apply_event(Event::PaneLines {
        key: a.clone(),
        lines: vec![line("alpha-sentinel")],
    });
    app.apply_event(Event::PaneLines {
        key: b.clone(),
        lines: vec![line("beta-sentinel")],
    });
    let text = |key: &str| {
        app.panes
            .get(key)
            .map(|buf| buf.iter().map(StyledLine::to_plain).collect::<String>())
            .unwrap_or_default()
    };
    assert!(
        text(&a).contains("alpha-sentinel"),
        "pane buffer for {a} missing alpha: {:?}",
        app.panes.keys().collect::<Vec<_>>()
    );
    assert!(
        text(&b).contains("beta-sentinel") && !text(&b).contains("alpha-sentinel"),
        "colliding alias must not share a pane buffer"
    );
    assert!(!text(&a).contains("beta-sentinel"));
}

#[test]
fn malformed_hr1_is_not_routed_as_the_legacy_alias() {
    let mut app = App::default();
    let id = "hr1:NOT-CANONICAL";
    app.selected_id = Some(id.to_string());
    app.apply_event(Event::Roster(vec![row(id, ALIAS, Liveness::Live)]));
    let opened = open_panes(&app.take_commands());
    assert_eq!(
        opened,
        vec![id.to_string()],
        "a malformed hr1 id is a refused qualified attempt, not fallback to {ALIAS}"
    );
}

struct IdentityDeck {
    rows: Arc<Mutex<Vec<RosterRow>>>,
    opens: Arc<Mutex<Vec<(String, String)>>>,
}

impl IdentityDeck {
    fn new(rows: Vec<RosterRow>) -> Self {
        IdentityDeck {
            rows: Arc::new(Mutex::new(rows)),
            opens: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn opens(&self) -> Vec<(String, String)> {
        self.opens.lock().expect("opens").clone()
    }
}

impl Clone for IdentityDeck {
    fn clone(&self) -> Self {
        IdentityDeck {
            rows: Arc::clone(&self.rows),
            opens: Arc::clone(&self.opens),
        }
    }
}

struct OnceTailer {
    sent: bool,
}

impl Tailer for OnceTailer {
    fn poll(&mut self) -> Vec<StyledLine> {
        if self.sent {
            return Vec::new();
        }
        self.sent = true;
        vec![StyledLine(vec![Seg::new(Sem::Error, "boom")])]
    }
}

impl Deck for IdentityDeck {
    fn roster(&mut self, _now: f64, _fresh_window: f64, _idle_timeout: f64) -> Vec<RosterRow> {
        self.rows.lock().expect("rows").clone()
    }

    fn open_tailer(
        &mut self,
        key: &str,
        session_id: &str,
        _replay: Replay,
    ) -> Option<Box<dyn Tailer>> {
        self.opens
            .lock()
            .expect("opens")
            .push((key.to_string(), session_id.to_string()));
        Some(Box::new(OnceTailer { sent: false }))
    }
}

fn engine_config() -> EngineConfig {
    EngineConfig {
        claude_dir: "/nonexistent/claude".into(),
        hermes_db: "/nonexistent/state.db".into(),
        opencode_db: "/nonexistent/opencode.db".into(),
        codex_dir: "/nonexistent/codex".into(),
        grok_dir: "/nonexistent/grok".into(),
        gemini_dir: "/nonexistent/gemini".into(),
        hermes_log: "/nonexistent/agent.log".into(),
        idle_timeout: 180.0,
        replay: Replay::DEFAULT,
        fresh_window: 1_000_000.0,
        interval: Duration::from_millis(15),
        linger: 60.0,
        max_panes: 8,
        notify: NotifyCfg::default().silenced(),
        remotes: Vec::new(),
        remote_flags: Vec::new(),
        docker_auto: false,
    }
}

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn spawn_engine(deck: IdentityDeck) -> (JoinHandle<()>, Sender<UiCmd>, mpsc::Receiver<Event>) {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let clock: Clock = Arc::new(|| NOW);
    let (tx, rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();
    let handle = Engine::spawn_with_clock(engine_config(), deck, clock, tx, ui_rx);
    (handle, ui_tx, rx)
}

fn shutdown(handle: JoinHandle<()>, ui_tx: &Sender<UiCmd>) {
    let _ = ui_tx.send(UiCmd::Shutdown);
    let _ = handle.join();
}

#[test]
fn legacy_engine_open_uses_alias_and_raw_id() {
    let deck = IdentityDeck::new(vec![row("C:aaa-id", "C:aaa", Liveness::Live)]);
    let (handle, ui_tx, _rx) = spawn_engine(deck.clone());
    ui_tx.send(UiCmd::OpenPane("C:aaa".into())).expect("send");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while deck.opens().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(15));
    }
    shutdown(handle, &ui_tx);
    assert_eq!(
        deck.opens(),
        vec![("C:aaa".into(), "C:aaa-id".into())],
        "legacy pair stays alias/raw id"
    );
}

#[test]
fn qualified_engine_opens_lifecycle_and_cooldowns_are_independent() {
    let (a, b, rows) = colliding();
    let deck = IdentityDeck::new(rows);
    let (handle, ui_tx, rx) = spawn_engine(deck.clone());
    ui_tx.send(UiCmd::OpenPane(a.clone())).expect("open a");
    ui_tx.send(UiCmd::OpenPane(b.clone())).expect("open b");
    ui_tx
        .send(UiCmd::OpenPane(ALIAS.into()))
        .expect("alias must not open a qualified row");

    let mut started = Vec::new();
    let mut panes = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline && (started.len() < 2 || panes.len() < 2) {
        if let Ok(event) = rx.recv_timeout(Duration::from_millis(30)) {
            match event {
                Event::Lifecycle {
                    key,
                    change: Lifecycle::Started,
                } => started.push(key),
                Event::PaneLines { key, .. } => panes.push(key),
                _ => {}
            }
        }
    }
    shutdown(handle, &ui_tx);

    let started_set: HashSet<_> = started.iter().cloned().collect();
    assert!(
        started_set.contains(&a) && started_set.contains(&b),
        "lifecycle keys must be the hr1 ids, got {started:?}"
    );
    let opens = deck.opens();
    assert!(
        opens.contains(&(a.clone(), a.clone())) && opens.contains(&(b.clone(), b.clone())),
        "open_tailer must be called as hr1/hr1, got {opens:?}"
    );
    assert!(
        !opens.iter().any(|(key, _)| key == ALIAS),
        "alias OpenPane must not open a qualified row, got {opens:?}"
    );
    let pane_set: HashSet<_> = panes.iter().cloned().collect();
    assert!(
        pane_set.contains(&a) && pane_set.contains(&b),
        "pane and error-buffer keys must not merge on {ALIAS}, got {panes:?}"
    );
}

#[test]
fn notification_seam_cools_down_per_key_without_delivery() {
    let mut hist = AlertHistory::new();
    let cfg = NotifyCfg::default();
    let transition = |key: &str| LifecycleTransition {
        key: key.to_string(),
        label: key.to_string(),
        from: Liveness::Live,
        to: Liveness::Done,
        done_cause: Some(hermon::notify::DoneCause::TurnDone),
        started_at: NOW - 60.0,
        state_since: NOW,
        cost: Some(1.0),
        last_tool: "-".into(),
        error_line: None,
    };
    let _ = decide_alerts(&[transition("boot")], NOW, &cfg, &mut hist);
    let (a, b, _) = colliding();
    let first = decide_alerts(
        &[transition(&a), transition(&b)],
        NOW + 1.0,
        &cfg,
        &mut hist,
    );
    let keys: HashSet<_> = first.iter().map(|alert| alert.key.as_str()).collect();
    assert_eq!(keys.len(), 2, "distinct hr1 keys both alert: {first:?}");
    assert!(first.iter().all(|alert| alert.kind == AlertKind::TurnDone));
    let merged = decide_alerts(
        &[transition(ALIAS), transition(ALIAS)],
        NOW + 2.0,
        &cfg,
        &mut hist,
    );
    assert_eq!(
        merged.len(),
        1,
        "a shared alias key must cool the second alert down: {merged:?}"
    );
}
