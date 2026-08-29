//! The watcher loop: polls sources, tracks sessions, drives tmux panes.
//!
//! Port of the Python daemon loop's polling half (`hermon.py:1294
//! cmd_watch`) — the `while True: … time.sleep(args.interval)` body that
//! rebuilds the roster every tick — minus tmux, restructured as a thread
//! talking to the UI over two `mpsc` channels rather than driving panes
//! directly. Panes are tailed here too, on a faster tick than the scan;
//! eviction/linger (M4) is not here yet.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::EngineConfig;
use crate::render::StyledLine;
use crate::roster::{RosterRow, Sources, TICKER_LIMIT, api_call_ticker, build_roster};
use crate::source::{Liveness, Replay, Tailer};

/// How often open panes are polled for new transcript lines. Far quicker
/// than the roster scan: a pane should read like a terminal, not like a
/// dashboard refresh.
pub const PANE_TICK: Duration = Duration::from_millis(300);

/// Engine → UI. `Roster` is a full replacement of the deck each tick, not a
/// diff — the UI is expected to redraw from it wholesale.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Roster(Vec<RosterRow>),
    /// The newest Hermes API calls, refreshed with the roster.
    Ticker(Vec<StyledLine>),
    Lifecycle {
        key: String,
        change: Lifecycle,
    },
    /// Transcript lines that appeared in an open pane since the last fast
    /// tick — an append, not a replacement, and only ever sent for a pane
    /// the UI asked for with [`UiCmd::OpenPane`].
    PaneLines {
        key: String,
        lines: Vec<StyledLine>,
    },
    /// Placeholder for the attention-alert channel landing in M6.
    Alert,
}

/// A session's liveness crossing a boundary the UI should narrate, keyed by
/// [`RosterRow::key`] (`hermon.py:1352` `~/●/✓` log lines).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// First tick a session's key was seen.
    Started,
    /// Live or needing attention last tick, done this tick.
    Finished,
    /// Done last tick, live or needing attention again this tick.
    Resumed,
}

/// UI → engine. Pane commands follow the cursor: the UI keeps exactly the
/// selected session's pane open, and the engine only tails what is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCmd {
    Shutdown,
    /// Start tailing this [`RosterRow::key`]; its lines arrive as
    /// [`Event::PaneLines`]. Opening an already-open pane does nothing.
    OpenPane(String),
    /// Stop tailing it. The engine drops the tailer, so a later
    /// [`UiCmd::OpenPane`] replays history again from scratch.
    ClosePane(String),
}

/// What the loop needs from the stores: the deck on each scan tick, and a
/// tailer per pane the UI opens. [`Sources`] is the production
/// implementation; tests substitute a fake so the loop can be driven with
/// no real store on disk.
pub trait Deck {
    fn roster(&mut self, now: f64, fresh_window: f64, idle_timeout: f64) -> Vec<RosterRow>;
    fn open_tailer(
        &mut self,
        key: &str,
        session_id: &str,
        replay: Replay,
    ) -> Option<Box<dyn Tailer>>;
}

impl Deck for Sources {
    fn roster(&mut self, now: f64, fresh_window: f64, idle_timeout: f64) -> Vec<RosterRow> {
        build_roster(self, now, fresh_window, idle_timeout)
    }

    fn open_tailer(
        &mut self,
        key: &str,
        session_id: &str,
        replay: Replay,
    ) -> Option<Box<dyn Tailer>> {
        Sources::open_tailer(self, key, session_id, replay)
    }
}

pub struct Engine;

impl Engine {
    /// Spawns the poll loop on its own thread. Returns immediately; join the
    /// handle after sending [`UiCmd::Shutdown`] (or dropping `rx`'s sender)
    /// to wait for it to exit.
    pub fn spawn(config: EngineConfig, tx: Sender<Event>, rx: Receiver<UiCmd>) -> JoinHandle<()> {
        thread::spawn(move || {
            let mut deck = Sources::new(&config.claude_dir, &config.hermes_db, &config.opencode_db);
            run(&config, &mut deck, &tx, &rx);
        })
    }

    /// The same loop against a caller-supplied [`Deck`] — the seam tests use
    /// to drive panes without fixture stores.
    pub fn spawn_with<D: Deck + Send + 'static>(
        config: EngineConfig,
        mut deck: D,
        tx: Sender<Event>,
        rx: Receiver<UiCmd>,
    ) -> JoinHandle<()> {
        thread::spawn(move || run(&config, &mut deck, &tx, &rx))
    }
}

/// Two cadences on one thread: a slow scan tick rebuilding the roster every
/// `config.interval`, and a fast [`PANE_TICK`] polling open panes. The
/// `recv_timeout` wait is cut short to whichever falls due first, so
/// commands are still handled the moment they arrive.
fn run(config: &EngineConfig, deck: &mut dyn Deck, tx: &Sender<Event>, rx: &Receiver<UiCmd>) {
    let mut prev_liveness: HashMap<String, Liveness> = HashMap::new();
    // Full session ids by roster key, refreshed each scan: a key carries
    // only a shortened id, and opening a tailer needs the real one.
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut panes: HashMap<String, Box<dyn Tailer>> = HashMap::new();

    let mut next_scan = Instant::now();
    let mut next_pane_tick = Instant::now();

    loop {
        if Instant::now() >= next_scan {
            if scan(config, deck, tx, &mut prev_liveness, &mut ids).is_err() {
                return; // UI hung up.
            }
            next_scan = Instant::now() + config.interval;
        }
        if Instant::now() >= next_pane_tick {
            if pump_panes(&mut panes, tx).is_err() {
                return;
            }
            next_pane_tick = Instant::now() + PANE_TICK;
        }

        let deadline = next_scan.min(next_pane_tick);
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(UiCmd::Shutdown) => return,
            Ok(UiCmd::OpenPane(key)) => {
                if let Some(tailer) = open_pane(deck, &panes, &ids, &key) {
                    panes.insert(key, tailer);
                    // Replay should hit the screen now, not a tick from now.
                    next_pane_tick = Instant::now();
                }
            }
            Ok(UiCmd::ClosePane(key)) => {
                panes.remove(&key);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// One scan tick: rebuild the deck, send it with its lifecycle changes and
/// the API ticker. `Err` means the UI dropped its receiver.
fn scan(
    config: &EngineConfig,
    deck: &mut dyn Deck,
    tx: &Sender<Event>,
    prev_liveness: &mut HashMap<String, Liveness>,
    ids: &mut HashMap<String, String>,
) -> Result<(), ()> {
    let now = now_secs();
    let rows = deck.roster(now, config.fresh_window, config.idle_timeout);
    let liveness = lifecycle_events(prev_liveness, &rows);
    *prev_liveness = rows.iter().map(|r| (r.key.clone(), r.state)).collect();
    *ids = rows.iter().map(|r| (r.key.clone(), r.id.clone())).collect();

    let ticker = api_call_ticker(Path::new(&config.hermes_log), TICKER_LIMIT);

    tx.send(Event::Roster(rows)).map_err(|_| ())?;
    tx.send(Event::Ticker(ticker)).map_err(|_| ())?;
    for event in liveness {
        tx.send(event).map_err(|_| ())?;
    }
    Ok(())
}

/// One fast tick: every open pane polled once, silent panes sending
/// nothing. Closed panes are gone from the map, so they are never polled.
fn pump_panes(panes: &mut HashMap<String, Box<dyn Tailer>>, tx: &Sender<Event>) -> Result<(), ()> {
    for (key, tailer) in panes.iter_mut() {
        let lines = tailer.poll();
        if lines.is_empty() {
            continue;
        }
        tx.send(Event::PaneLines {
            key: key.clone(),
            lines,
        })
        .map_err(|_| ())?;
    }
    Ok(())
}

/// A tailer for a newly opened pane, or `None` if the pane is already open,
/// the key is not on the deck, or the source cannot tail that session.
fn open_pane(
    deck: &mut dyn Deck,
    panes: &HashMap<String, Box<dyn Tailer>>,
    ids: &HashMap<String, String>,
    key: &str,
) -> Option<Box<dyn Tailer>> {
    if panes.contains_key(key) {
        return None;
    }
    let session_id = ids.get(key)?;
    deck.open_tailer(key, session_id, Replay::DEFAULT)
}

/// Diffs this tick's rows against the last tick's liveness, in roster order,
/// so the UI's log line order matches the deck.
fn lifecycle_events(prev: &HashMap<String, Liveness>, rows: &[RosterRow]) -> Vec<Event> {
    rows.iter()
        .filter_map(|row| {
            let change = match prev.get(&row.key) {
                None => Lifecycle::Started,
                Some(Liveness::Done) if row.state != Liveness::Done => Lifecycle::Resumed,
                Some(prev_state)
                    if *prev_state != Liveness::Done && row.state == Liveness::Done =>
                {
                    Lifecycle::Finished
                }
                _ => return None,
            };
            Some(Event::Lifecycle {
                key: row.key.clone(),
                change,
            })
        })
        .collect()
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Attn;

    fn row(key: &str, state: Liveness) -> RosterRow {
        RosterRow {
            id: format!("id-{key}"),
            key: key.to_string(),
            state,
            model: "m".to_string(),
            last_tool: "-".to_string(),
            last_line: String::new(),
            in_tok: 0,
            out_tok: 0,
            cost: 0.0,
            elapsed: None,
            last_ts: 0.0,
            title: String::new(),
        }
    }

    #[test]
    fn a_new_key_is_started() {
        let prev = HashMap::new();
        let rows = vec![row("C:aaaaaa", Liveness::Live)];
        assert_eq!(
            lifecycle_events(&prev, &rows),
            vec![Event::Lifecycle {
                key: "C:aaaaaa".to_string(),
                change: Lifecycle::Started,
            }]
        );
    }

    #[test]
    fn live_to_done_is_finished() {
        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Live)]);
        let rows = vec![row("C:aaaaaa", Liveness::Done)];
        assert_eq!(
            lifecycle_events(&prev, &rows),
            vec![Event::Lifecycle {
                key: "C:aaaaaa".to_string(),
                change: Lifecycle::Finished,
            }]
        );
    }

    #[test]
    fn done_to_live_is_resumed() {
        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Done)]);
        let rows = vec![row("C:aaaaaa", Liveness::Live)];
        assert_eq!(
            lifecycle_events(&prev, &rows),
            vec![Event::Lifecycle {
                key: "C:aaaaaa".to_string(),
                change: Lifecycle::Resumed,
            }]
        );
    }

    #[test]
    fn attention_counts_as_live_for_lifecycle() {
        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Live)]);
        let rows = vec![row("C:aaaaaa", Liveness::Attention(Attn::Stuck))];
        assert!(lifecycle_events(&prev, &rows).is_empty());

        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Attention(Attn::PermWait))]);
        let rows = vec![row("C:aaaaaa", Liveness::Done)];
        assert_eq!(
            lifecycle_events(&prev, &rows),
            vec![Event::Lifecycle {
                key: "C:aaaaaa".to_string(),
                change: Lifecycle::Finished,
            }]
        );
    }

    #[test]
    fn unchanged_liveness_emits_nothing() {
        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Live)]);
        let rows = vec![row("C:aaaaaa", Liveness::Live)];
        assert!(lifecycle_events(&prev, &rows).is_empty());
    }
}
