//! The watcher loop: polls sources, tracks sessions, drives tmux panes.
//!
//! Port of the Python daemon loop's polling half (`hermon.py:1294
//! cmd_watch`) — the `while True: … time.sleep(args.interval)` body that
//! rebuilds the roster every tick — minus tmux, restructured as a thread
//! talking to the UI over two `mpsc` channels rather than driving panes
//! directly. Pane management (M3) and eviction/linger (M4) are not here yet.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::EngineConfig;
use crate::roster::{RosterRow, Sources, build_roster};
use crate::source::Liveness;

/// Engine → UI. `Roster` is a full replacement of the deck each tick, not a
/// diff — the UI is expected to redraw from it wholesale.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Roster(Vec<RosterRow>),
    Lifecycle {
        key: String,
        change: Lifecycle,
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

/// UI → engine. `Shutdown` is the only command for M2; the rest arrive with
/// their features (M3 tailing, M5 pane commands).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCmd {
    Shutdown,
}

pub struct Engine;

impl Engine {
    /// Spawns the poll loop on its own thread. Returns immediately; join the
    /// handle after sending [`UiCmd::Shutdown`] (or dropping `rx`'s sender)
    /// to wait for it to exit.
    pub fn spawn(config: EngineConfig, tx: Sender<Event>, rx: Receiver<UiCmd>) -> JoinHandle<()> {
        thread::spawn(move || run(config, &tx, &rx))
    }
}

fn run(config: EngineConfig, tx: &Sender<Event>, rx: &Receiver<UiCmd>) {
    let mut sources = Sources::new(&config.claude_dir, &config.hermes_db, &config.opencode_db);
    let mut prev_liveness: HashMap<String, Liveness> = HashMap::new();

    loop {
        let now = now_secs();
        let rows = build_roster(&mut sources, now, config.fresh_window, config.idle_timeout);
        let liveness = lifecycle_events(&prev_liveness, &rows);
        prev_liveness = rows.iter().map(|r| (r.key.clone(), r.state)).collect();

        if tx.send(Event::Roster(rows)).is_err() {
            return; // UI hung up.
        }
        for event in liveness {
            if tx.send(event).is_err() {
                return;
            }
        }

        match rx.recv_timeout(config.interval) {
            Ok(UiCmd::Shutdown) => return,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
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
            key: key.to_string(),
            state,
            model: "m".to_string(),
            last_tool: "-".to_string(),
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
