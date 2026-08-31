//! The watcher loop: polls sources, tracks sessions, drives the TUI panes.
//!
//! Port of the Python daemon loop's polling half (`hermon.py:1294
//! cmd_watch`) — the `while True: … time.sleep(args.interval)` body that
//! rebuilds the roster every tick — minus tmux, restructured as a thread
//! talking to the UI over two `mpsc` channels rather than driving panes
//! directly. Panes are tailed here too, on a faster tick than the scan.
//!
//! Lifecycle (`hermon.py:1352` the tracked-dict loop, `hermon.py:1389
//! self_evict`) lives here too: [`Tracked`] remembers, per key, when a
//! session last finished and whether its tailer has since been closed —
//! by [`linger_expire`] aging it out, or by [`try_open`] evicting it to
//! make room under `--max-panes`. A key survives in [`Tracked`] past its
//! own disappearance from the roster until [`forget`] can drop it, so a
//! session that vanishes mid-linger still ages out on schedule rather than
//! being forgotten (and its slot silently freed) the instant its source
//! goes quiet.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::EngineConfig;
use crate::notify::{self, Alert, AlertHistory, DoneCause, LifecycleTransition, Notifier};
use crate::remote::discover::{self, Discovered};
use crate::remote::source::RemoteSource;
use crate::remote::spec::{docker_spec, to_command};
use crate::render::{Sem, StyledLine};
use crate::roster::{RosterRow, Sources, TICKER_LIMIT, api_call_ticker, build_roster};
use crate::source::{Attn, Liveness, Replay, Tailer};

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
    /// One notification [`notify::decide_alerts`] fired this tick.
    /// Delivery to the desktop already happened by the time this lands — the
    /// UI just gets to know about it too (a future history view, say).
    Alert(Alert),
}

/// A session's liveness crossing a boundary the UI should narrate, keyed by
/// [`RosterRow::key`] (`hermon.py:1352` `~/●/✓` log lines).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// First tick a session's key was seen.
    Started,
    /// Live or needing attention last tick, done this tick. Carries why, so
    /// M6's `decide_alerts` can tell a session that timed out while stuck
    /// apart from one that just finished its turn cleanly.
    Finished(Cause),
    /// Done last tick, live or needing attention again this tick.
    Resumed,
    /// A finished session's pane was closed to make room for a new live one
    /// under `--max-panes` (`hermon.py:1389 self_evict`), not because its
    /// own linger expired.
    Evicted,
    /// Live (or a resumed Done) crossed into [`Liveness::Attention`], or
    /// switched from one attention reason to the other without ever
    /// clearing. Carries which.
    Attention(Attn),
    /// Was needing attention, is plain live again — a permission prompt
    /// answered, a wedged tool producing output again.
    Cleared,
}

/// Why a [`Lifecycle::Finished`] fired: the session either stopped cleanly
/// (a turn closed, or the source reported it ended) or was still needing
/// attention when it aged out past `fresh_window` — the same silence a
/// human would read as "gave up waiting", not "done".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Stopped mid-attention: [`Attn::PermWait`] or [`Attn::Stuck`] never
    /// resolved before the session aged out of the roster.
    Timeout,
    /// Stopped without ever needing attention.
    Clean,
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
    /// Sets the global mute flag `[m]` toggles — mirrors the UI's own copy,
    /// which flips instantly rather than waiting on a round trip.
    SetMuted(bool),
    /// Replaces the whole pinned set, keyed by [`RosterRow::key`]. Eviction
    /// (`--max-panes`, [`try_open`]) never picks a key in this set as its
    /// victim — the UI ticket's pinning composes with M4 eviction this way,
    /// without the engine ever needing to know why a key is pinned.
    Pinned(HashSet<String>),
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

    /// Applies one `--docker-auto` discovery tick's decisions: spawns each
    /// newly (or re-)discovered container's remote, tears down each one
    /// that disappeared. Default no-op — only [`Sources`], the production
    /// deck, has remotes to manage; the engine's test fakes never enable
    /// `--docker-auto`, so they never need an override.
    fn sync_docker_auto(
        &mut self,
        _spawn: Vec<Discovered>,
        _remove: Vec<String>,
        _remote_flags: &[String],
    ) {
    }
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

    fn sync_docker_auto(
        &mut self,
        spawn: Vec<Discovered>,
        remove: Vec<String>,
        remote_flags: &[String],
    ) {
        for name in remove {
            self.remove_remote(&name);
        }
        for d in spawn {
            let spec = docker_spec(d.container, d.name);
            let cmd = to_command(&spec, remote_flags);
            self.add_remote(RemoteSource::new(spec.name.clone(), cmd));
        }
    }
}

/// A wall clock the engine reads instead of calling [`SystemTime::now`]
/// directly, so tests can drive linger/eviction deterministically without
/// sleeping real seconds. `Arc<dyn Fn…>` rather than `FnMut` because it is
/// shared between the run loop and the command handlers it calls into.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// What a tracked key remembers between ticks: when it last finished, and
/// whether its pane has since been closed by policy rather than by the UI.
/// Ported from `hermon.py:1352`'s per-session `Tracked` record, minus the
/// tmux pane id — [`Engine`]'s `panes` map is the pane now.
#[derive(Debug, Clone, Copy)]
struct Tracked {
    liveness: Liveness,
    /// Set the instant `liveness` last became [`Liveness::Done`]; cleared on
    /// resumption. The linger clock and eviction's oldest-first pick both
    /// read this.
    finished_at: Option<f64>,
    /// The pane was closed by [`linger_expire`] or by [`try_open`] evicting
    /// it, not by an explicit [`UiCmd::ClosePane`]. A killed-and-still-done
    /// key is not retried until it resumes; a killed key gone from the
    /// roster is what [`forget`] prunes.
    killed: bool,
    /// Set the instant `liveness` last became the [`Liveness::Attention`]
    /// variant it currently is; cleared the instant it leaves attention
    /// (cleared to live, or finished). Reset — not preserved — on
    /// re-entry, and on switching from one attention reason to the other,
    /// so elapsed-in-state always measures the *current* reason.
    attn_since: Option<f64>,
}

/// Cross-tick bookkeeping for `--docker-auto`, threaded through [`run`]'s
/// loop the same way `tracked`/`ids` are: which containers are currently
/// spawned (by container id, to the name they're running under, so a
/// rename can be told apart from a plain disappear-then-reappear), whether
/// `docker` has already proven unusable this run, and which warnings have
/// already been printed once so a standing collision doesn't spam stderr
/// every tick.
#[derive(Debug, Default)]
struct DockerAutoState {
    managed: HashMap<String, String>,
    disabled: bool,
    warned: HashSet<String>,
}

/// Runs `docker ps --filter label=<AGENT_LABEL> --format {{json .}}` for
/// one discovery tick. `Err` names why it failed — `docker` missing from
/// `PATH`, or a nonzero exit (daemon unreachable, say) — so the caller can
/// warn once and turn the feature off rather than retrying a broken
/// `docker` every tick forever.
fn run_docker_ps() -> Result<String, String> {
    let output = std::process::Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("label={}", discover::AGENT_LABEL),
            "--format",
            "{{json .}}",
        ])
        .output()
        .map_err(|e| format!("docker ps: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "docker ps: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// One `--docker-auto` discovery tick: runs `docker ps`, reconciles it
/// against last tick's bookkeeping, and applies the result to `deck`. A
/// no-op once `docker` has proven unusable ([`DockerAutoState::disabled`])
/// — the "bounded" half of the issue's "poll, bounded; docker absent -> one
/// warning, feature off".
fn sync_docker_auto(config: &EngineConfig, deck: &mut dyn Deck, state: &mut DockerAutoState) {
    if !config.docker_auto || state.disabled {
        return;
    }
    let stdout = match run_docker_ps() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hermon: --docker-auto: {e}, disabling for this run");
            state.disabled = true;
            return;
        }
    };
    let containers = discover::parse_ps(&stdout);
    let explicit: HashSet<String> = config.remotes.iter().map(|r| r.name.clone()).collect();
    let (sync, next) = discover::reconcile(&containers, &explicit, &state.managed);
    for warning in &sync.warnings {
        if state.warned.insert(warning.clone()) {
            eprintln!("hermon: {warning}");
        }
    }
    state.managed = next;
    if !sync.spawn.is_empty() || !sync.remove.is_empty() {
        deck.sync_docker_auto(sync.spawn, sync.remove, &config.remote_flags);
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
            for spec in &config.remotes {
                let cmd = to_command(spec, &config.remote_flags);
                deck = deck.with_remote(RemoteSource::new(spec.name.clone(), cmd));
            }
            let clock: Clock = Arc::new(now_secs);
            run(&config, &mut deck, &clock, &tx, &rx);
        })
    }

    /// The same loop against a caller-supplied [`Deck`], on the real wall
    /// clock — the seam tests use to drive panes without fixture stores.
    pub fn spawn_with<D: Deck + Send + 'static>(
        config: EngineConfig,
        deck: D,
        tx: Sender<Event>,
        rx: Receiver<UiCmd>,
    ) -> JoinHandle<()> {
        Self::spawn_with_clock(config, deck, Arc::new(now_secs), tx, rx)
    }

    /// [`Engine::spawn_with`] plus an injectable [`Clock`] — the seam
    /// lifecycle tests use to walk linger and eviction deterministically
    /// without sleeping real seconds.
    pub fn spawn_with_clock<D: Deck + Send + 'static>(
        config: EngineConfig,
        mut deck: D,
        clock: Clock,
        tx: Sender<Event>,
        rx: Receiver<UiCmd>,
    ) -> JoinHandle<()> {
        thread::spawn(move || run(&config, &mut deck, &clock, &tx, &rx))
    }
}

/// Two cadences on one thread: a slow scan tick rebuilding the roster every
/// `config.interval`, and a fast [`PANE_TICK`] polling open panes. The
/// `recv_timeout` wait is cut short to whichever falls due first, so
/// commands are still handled the moment they arrive.
fn run(
    config: &EngineConfig,
    deck: &mut dyn Deck,
    clock: &Clock,
    tx: &Sender<Event>,
    rx: &Receiver<UiCmd>,
) {
    let mut tracked: HashMap<String, Tracked> = HashMap::new();
    // Full session ids by roster key, refreshed each scan: a key carries
    // only a shortened id, and opening a tailer needs the real one.
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut panes: HashMap<String, Box<dyn Tailer>> = HashMap::new();
    // Keys the UI has asked to be tailed. Usually equal to `panes.keys()`;
    // lifecycle policy can make `panes` lag behind it (a pane lingered or
    // evicted away) or catch back up on its own (a resumption reopening
    // one), all without the UI ever sending another command.
    let mut wanted: HashSet<String> = HashSet::new();
    // Sem::Error lines seen on open panes since the last scan drained them,
    // keyed by roster key — the only source `decide_alerts` has for
    // [`LifecycleTransition::error_line`], since only tailed sessions have
    // any transcript content to inspect at all.
    let mut errors: HashMap<String, String> = HashMap::new();
    let mut hist = AlertHistory::new();
    // Probed once, not per tick: it's a filesystem walk, and the answer
    // doesn't change while hermon is running.
    let notifier = notify::probe();
    // Keys the UI has pinned, replaced wholesale on every `UiCmd::Pinned`.
    // Eviction never picks a victim from this set (#42).
    let mut pinned: HashSet<String> = HashSet::new();
    // `--docker-auto` (#92) bookkeeping; a no-op tick when the flag is off.
    let mut docker_auto = DockerAutoState::default();

    let mut next_scan = Instant::now();
    let mut next_pane_tick = Instant::now();

    loop {
        if Instant::now() >= next_scan {
            if scan(
                config,
                deck,
                clock,
                tx,
                &mut tracked,
                &mut ids,
                &mut panes,
                &wanted,
                &mut errors,
                &mut hist,
                &notifier,
                &pinned,
                &mut docker_auto,
            )
            .is_err()
            {
                return; // UI hung up.
            }
            next_scan = Instant::now() + config.interval;
        }
        if Instant::now() >= next_pane_tick {
            if pump_panes(&mut panes, tx, &mut errors).is_err() {
                return;
            }
            next_pane_tick = Instant::now() + PANE_TICK;
        }

        let deadline = next_scan.min(next_pane_tick);
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(UiCmd::Shutdown) => return,
            Ok(UiCmd::SetMuted(muted)) => hist.set_muted(muted),
            Ok(UiCmd::OpenPane(key)) => {
                wanted.insert(key.clone());
                if !panes.contains_key(&key) && !killed_and_done(&tracked, &key) {
                    match try_open(
                        deck,
                        &mut panes,
                        &mut tracked,
                        &ids,
                        &key,
                        config.max_panes,
                        &pinned,
                        config.replay,
                    ) {
                        OpenOutcome::Skipped => {}
                        OpenOutcome::Opened => next_pane_tick = Instant::now(),
                        OpenOutcome::Evicted(victim) => {
                            next_pane_tick = Instant::now();
                            if tx.send(evicted_event(victim)).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            Ok(UiCmd::ClosePane(key)) => {
                wanted.remove(&key);
                panes.remove(&key);
            }
            Ok(UiCmd::Pinned(keys)) => pinned = keys,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn evicted_event(key: String) -> Event {
    Event::Lifecycle {
        key,
        change: Lifecycle::Evicted,
    }
}

fn killed_and_done(tracked: &HashMap<String, Tracked>, key: &str) -> bool {
    tracked
        .get(key)
        .is_some_and(|t| t.killed && t.liveness == Liveness::Done)
}

/// One scan tick: rebuild the deck, apply the lifecycle state machine over
/// it, and send the roster with its lifecycle changes and the API ticker.
/// `Err` means the UI dropped its receiver.
#[allow(clippy::too_many_arguments)]
fn scan(
    config: &EngineConfig,
    deck: &mut dyn Deck,
    clock: &Clock,
    tx: &Sender<Event>,
    tracked: &mut HashMap<String, Tracked>,
    ids: &mut HashMap<String, String>,
    panes: &mut HashMap<String, Box<dyn Tailer>>,
    wanted: &HashSet<String>,
    errors: &mut HashMap<String, String>,
    hist: &mut AlertHistory,
    notifier: &Notifier,
    pinned: &HashSet<String>,
    docker_auto: &mut DockerAutoState,
) -> Result<(), ()> {
    sync_docker_auto(config, deck, docker_auto);
    let now = clock();
    let mut rows = deck.roster(now, config.fresh_window, config.idle_timeout);

    let prev_liveness: HashMap<String, Liveness> = tracked
        .iter()
        .map(|(k, t)| (k.clone(), t.liveness))
        .collect();
    let mut events = lifecycle_events(&prev_liveness, &rows);
    events.extend(attention_events(&prev_liveness, &rows));
    events.extend(vanished_events(tracked, &rows));

    apply_lifecycle(tracked, &events, &rows, now);
    refresh_liveness(tracked, &rows);
    attach_attn_elapsed(tracked, &mut rows, now);
    linger_expire(tracked, panes, config.linger, now);
    forget(tracked, panes, &rows);
    *ids = rows.iter().map(|r| (r.key.clone(), r.id.clone())).collect();

    // Decide and fire this tick's alerts, once liveness/attn-elapsed are
    // final. A vanished session (finished with no row surviving this tick)
    // gets no transition — there is no roster data left to describe it —
    // so it can only ever raise TurnDone/Stuck/PermWait while its row is
    // still on the deck.
    let transitions = build_transitions(&prev_liveness, &rows, now, errors);
    for alert in notify::decide_alerts(&transitions, now, &config.notify, hist) {
        notify::deliver(notifier, "hermon", &alert.label, &alert.detail);
        events.push(Event::Alert(alert));
    }

    // Satisfy whatever the UI still wants but doesn't have: newly wanted
    // keys the last scan couldn't reach yet, and resumptions reopening a
    // pane linger or eviction closed without the UI ever asking again.
    let pending: Vec<String> = wanted
        .iter()
        .filter(|k| !panes.contains_key(*k) && !killed_and_done(tracked, k))
        .cloned()
        .collect();
    for key in pending {
        if let OpenOutcome::Evicted(victim) = try_open(
            deck,
            panes,
            tracked,
            ids,
            &key,
            config.max_panes,
            pinned,
            config.replay,
        ) {
            events.push(evicted_event(victim));
        }
    }

    let ticker = api_call_ticker(Path::new(&config.hermes_log), TICKER_LIMIT);

    tx.send(Event::Roster(rows)).map_err(|_| ())?;
    tx.send(Event::Ticker(ticker)).map_err(|_| ())?;
    for event in events {
        tx.send(event).map_err(|_| ())?;
    }
    Ok(())
}

/// One fast tick: every open pane polled once, silent panes sending
/// nothing. Closed panes are gone from the map, so they are never polled.
/// Any `Sem::Error` line seen is stashed in `errors`, keyed by roster key —
/// the only source [`notify::decide_alerts`] has for
/// [`LifecycleTransition::error_line`], since only tailed sessions have any
/// transcript content to inspect at all. Overwritten by a later error the
/// same tick; drained by the next scan tick's [`build_transitions`].
fn pump_panes(
    panes: &mut HashMap<String, Box<dyn Tailer>>,
    tx: &Sender<Event>,
    errors: &mut HashMap<String, String>,
) -> Result<(), ()> {
    for (key, tailer) in panes.iter_mut() {
        let lines = tailer.poll();
        if lines.is_empty() {
            continue;
        }
        if let Some(line) = lines
            .iter()
            .rev()
            .find(|line| line.0.iter().any(|seg| seg.sem == Sem::Error))
        {
            errors.insert(key.clone(), line.to_plain());
        }
        tx.send(Event::PaneLines {
            key: key.clone(),
            lines,
        })
        .map_err(|_| ())?;
    }
    Ok(())
}

/// Builds one [`LifecycleTransition`] per current row for
/// [`notify::decide_alerts`], reading `from` off the pre-tick liveness
/// snapshot and draining any error line [`pump_panes`] recorded for that key
/// since the last scan. `started_at`/`state_since` are derived from the
/// row's own `elapsed`/`attn_elapsed` rather than kept separately in
/// [`Tracked`] — both already carry exactly this, computed the same tick.
fn build_transitions(
    prev: &HashMap<String, Liveness>,
    rows: &[RosterRow],
    now: f64,
    errors: &mut HashMap<String, String>,
) -> Vec<LifecycleTransition> {
    rows.iter()
        .map(|row| {
            let from = prev.get(&row.key).copied().unwrap_or(row.state);
            let done_cause =
                (row.state == Liveness::Done && from != Liveness::Done).then(
                    || match finish_cause(from) {
                        Cause::Clean => DoneCause::TurnDone,
                        Cause::Timeout => DoneCause::Timeout,
                    },
                );
            LifecycleTransition {
                key: row.key.clone(),
                label: row.key.clone(),
                from,
                to: row.state,
                done_cause,
                started_at: row.last_ts - row.elapsed.unwrap_or(0.0),
                state_since: now - row.attn_elapsed.unwrap_or(0.0),
                cost: row.cost,
                last_tool: row.last_tool.clone(),
                error_line: errors.remove(&row.key),
            }
        })
        .collect()
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
                    Lifecycle::Finished(finish_cause(*prev_state))
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

/// A session that was mid-attention when it stopped timed out waiting for a
/// human or a wedged tool; anything else stopped on its own.
fn finish_cause(prev_state: Liveness) -> Cause {
    if matches!(prev_state, Liveness::Attention(_)) {
        Cause::Timeout
    } else {
        Cause::Clean
    }
}

/// Diffs this tick's rows against the last tick's liveness for the
/// live-side boundary `lifecycle_events` doesn't cover: crossing into or out
/// of [`Liveness::Attention`] without touching [`Liveness::Done`] at all.
/// Firing on a brand-new key that starts already needing attention is
/// deliberate — [`Lifecycle::Started`] narrates the session appearing,
/// this narrates that it needs eyes on it right away — and a resumption
/// landing straight in `Attention` fires both `Resumed` and this, since
/// both are true.
fn attention_events(prev: &HashMap<String, Liveness>, rows: &[RosterRow]) -> Vec<Event> {
    rows.iter()
        .filter_map(|row| {
            let prev_state = prev.get(&row.key).copied();
            let change = match row.state {
                Liveness::Attention(attn) if prev_state != Some(row.state) => {
                    Lifecycle::Attention(attn)
                }
                Liveness::Live if matches!(prev_state, Some(Liveness::Attention(_))) => {
                    Lifecycle::Cleared
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

/// A tracked key that drops out of the roster entirely, without first
/// reading as done, is an implicit finish — every source stopped reporting
/// it before `classify` ever got the chance to. Mirrors Python's
/// `state = snap.state if snap else "done"` (`hermon.py:1352`): the linger
/// clock starts the moment a session disappears, not never.
fn vanished_events(tracked: &HashMap<String, Tracked>, rows: &[RosterRow]) -> Vec<Event> {
    tracked
        .iter()
        .filter(|(_, t)| t.liveness != Liveness::Done)
        .filter(|(key, _)| !rows.iter().any(|r| &r.key == *key))
        .map(|(key, t)| Event::Lifecycle {
            key: key.clone(),
            change: Lifecycle::Finished(finish_cause(t.liveness)),
        })
        .collect()
}

/// Folds this tick's [`Lifecycle`] events into [`Tracked`]: starts or clears
/// the linger clock, clears `killed` so a resumed session is eligible for a
/// pane again, and starts or clears the attention clock `Attention`/`Cleared`
/// carry. `events` always orders a key's [`Lifecycle::Started`] before any
/// [`Lifecycle::Attention`] for the same tick (`lifecycle_events`'s output
/// precedes `attention_events`'s in `scan`), so the branches below can
/// assume the tracked entry already exists by the time attention fires.
fn apply_lifecycle(
    tracked: &mut HashMap<String, Tracked>,
    events: &[Event],
    rows: &[RosterRow],
    now: f64,
) {
    for event in events {
        let Event::Lifecycle { key, change } = event else {
            continue;
        };
        match change {
            Lifecycle::Started => {
                let liveness = rows
                    .iter()
                    .find(|r| &r.key == key)
                    .map_or(Liveness::Done, |r| r.state);
                tracked.insert(
                    key.clone(),
                    Tracked {
                        liveness,
                        finished_at: (liveness == Liveness::Done).then_some(now),
                        killed: false,
                        attn_since: None,
                    },
                );
            }
            Lifecycle::Finished(_) => match tracked.get_mut(key) {
                Some(t) => {
                    t.liveness = Liveness::Done;
                    t.finished_at = Some(now);
                    t.killed = false;
                    t.attn_since = None;
                }
                None => {
                    tracked.insert(
                        key.clone(),
                        Tracked {
                            liveness: Liveness::Done,
                            finished_at: Some(now),
                            killed: false,
                            attn_since: None,
                        },
                    );
                }
            },
            Lifecycle::Resumed => {
                let liveness = rows
                    .iter()
                    .find(|r| &r.key == key)
                    .map_or(Liveness::Live, |r| r.state);
                match tracked.get_mut(key) {
                    Some(t) => {
                        t.liveness = liveness;
                        t.finished_at = None;
                        t.killed = false;
                    }
                    None => {
                        tracked.insert(
                            key.clone(),
                            Tracked {
                                liveness,
                                finished_at: None,
                                killed: false,
                                attn_since: None,
                            },
                        );
                    }
                }
            }
            Lifecycle::Attention(attn) => match tracked.get_mut(key) {
                Some(t) => {
                    t.liveness = Liveness::Attention(*attn);
                    t.attn_since = Some(now);
                }
                None => {
                    tracked.insert(
                        key.clone(),
                        Tracked {
                            liveness: Liveness::Attention(*attn),
                            finished_at: None,
                            killed: false,
                            attn_since: Some(now),
                        },
                    );
                }
            },
            Lifecycle::Cleared => {
                if let Some(t) = tracked.get_mut(key) {
                    t.liveness = Liveness::Live;
                    t.attn_since = None;
                }
            }
            Lifecycle::Evicted => {}
        }
    }
}

/// Syncs every tracked key still on the roster to its row's exact liveness.
/// [`apply_lifecycle`] only reacts to the live/done boundary the UI narrates
/// (`Started`/`Finished`/`Resumed`); a change within the "not done" side of
/// it — say `Live` to `Attention(Stuck)` — fires no event but still has to
/// land here so later ticks compare against the truth, not a stale value.
fn refresh_liveness(tracked: &mut HashMap<String, Tracked>, rows: &[RosterRow]) {
    for row in rows {
        if let Some(t) = tracked.get_mut(&row.key) {
            t.liveness = row.state;
        }
    }
}

/// Stamps each row with how long its session has held its current
/// [`Liveness::Attention`] state, read from the `attn_since` [`apply_lifecycle`]
/// just settled — `None` outside attention. This is the only place
/// [`RosterRow::attn_elapsed`] is ever set: [`build_roster`] itself has no
/// memory of a previous tick to compute it from.
fn attach_attn_elapsed(tracked: &HashMap<String, Tracked>, rows: &mut [RosterRow], now: f64) {
    for row in rows.iter_mut() {
        row.attn_elapsed = tracked
            .get(&row.key)
            .and_then(|t| t.attn_since)
            .map(|since| now - since);
    }
}

/// Closes the pane of every tracked key that has been done longer than
/// `linger` seconds, freeing its slot for `try_open` to hand to someone
/// else. `linger <= 0.0` means "forever" — Python's `args.linger` `0`
/// (`hermon.py:1294`) — so nothing here ever kills on age alone; only
/// [`try_open`]'s eviction can still reclaim the slot.
fn linger_expire(
    tracked: &mut HashMap<String, Tracked>,
    panes: &mut HashMap<String, Box<dyn Tailer>>,
    linger: f64,
    now: f64,
) {
    if linger <= 0.0 {
        return;
    }
    for (key, t) in tracked.iter_mut() {
        if t.liveness == Liveness::Done
            && !t.killed
            && t.finished_at.is_some_and(|f| now - f >= linger)
        {
            panes.remove(key);
            t.killed = true;
        }
    }
}

/// Drops a tracked key once it is both killed (its pane closed by linger or
/// eviction, never by the UI) and gone from every source — the roster no
/// longer lists it at all. Port of `hermon.py:1352`'s
/// `if t.killed and snap is None: del tracked[key]`.
fn forget(
    tracked: &mut HashMap<String, Tracked>,
    panes: &mut HashMap<String, Box<dyn Tailer>>,
    rows: &[RosterRow],
) {
    let present: HashSet<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    tracked.retain(|key, t| !(t.killed && !present.contains(key.as_str())));
    panes.retain(|key, _| tracked.contains_key(key));
}

/// What [`try_open`] managed to do for the key it was asked to open.
enum OpenOutcome {
    /// Opened with a free slot to spare.
    Opened,
    /// Opened by closing a finished pane to make room; the evicted key's
    /// [`Lifecycle::Evicted`] event is the caller's to send.
    Evicted(String),
    /// Not opened: the key isn't on the deck, its source can't tail it, or
    /// every occupied slot belongs to a still-live session — Python's
    /// `self_evict` returning `false` and the caller moving on
    /// (`hermon.py:1389`).
    Skipped,
}

/// Opens `key`'s tailer, first evicting the oldest-finished, unpinned pane
/// if `max_panes` is already full. Eviction only ever takes a *finished,
/// unpinned* pane's slot — a fleet that is entirely live or pinned never
/// loses one, and `key` simply waits for a slot on a later tick, without the
/// deck ever being asked to open it. The chosen victim isn't actually
/// removed until `key`'s own tailer opens successfully, so a source that
/// can't tail `key` costs nobody their slot.
#[allow(clippy::too_many_arguments)]
fn try_open(
    deck: &mut dyn Deck,
    panes: &mut HashMap<String, Box<dyn Tailer>>,
    tracked: &mut HashMap<String, Tracked>,
    ids: &HashMap<String, String>,
    key: &str,
    max_panes: usize,
    pinned: &HashSet<String>,
    replay: Replay,
) -> OpenOutcome {
    let Some(session_id) = ids.get(key) else {
        return OpenOutcome::Skipped;
    };

    let mut victim = None;
    if panes.len() >= max_panes {
        victim = panes
            .keys()
            .filter(|k| {
                !pinned.contains(k.as_str())
                    && tracked
                        .get(k.as_str())
                        .is_some_and(|t| t.liveness == Liveness::Done)
            })
            .min_by(|a, b| {
                let at = tracked[a.as_str()].finished_at.unwrap_or(0.0);
                let bt = tracked[b.as_str()].finished_at.unwrap_or(0.0);
                at.total_cmp(&bt)
            })
            .cloned();
        if victim.is_none() {
            return OpenOutcome::Skipped;
        }
    }

    let Some(tailer) = deck.open_tailer(key, session_id, replay) else {
        return OpenOutcome::Skipped;
    };

    if let Some(victim) = &victim {
        panes.remove(victim);
        if let Some(t) = tracked.get_mut(victim) {
            t.killed = true;
        }
    }
    panes.insert(key.to_string(), tailer);
    match victim {
        Some(victim) => OpenOutcome::Evicted(victim),
        None => OpenOutcome::Opened,
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Seg;
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
            cost: Some(0.0),
            elapsed: None,
            last_ts: 0.0,
            title: String::new(),
            attn_elapsed: None,
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
    fn live_to_done_is_finished_cleanly() {
        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Live)]);
        let rows = vec![row("C:aaaaaa", Liveness::Done)];
        assert_eq!(
            lifecycle_events(&prev, &rows),
            vec![Event::Lifecycle {
                key: "C:aaaaaa".to_string(),
                change: Lifecycle::Finished(Cause::Clean),
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
    fn attention_counts_as_live_for_the_done_boundary() {
        // lifecycle_events only narrates the live/done boundary; entering
        // Attention fires nothing on this side (attention_events covers it).
        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Live)]);
        let rows = vec![row("C:aaaaaa", Liveness::Attention(Attn::Stuck))];
        assert!(lifecycle_events(&prev, &rows).is_empty());
    }

    #[test]
    fn finishing_from_attention_is_a_timeout() {
        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Attention(Attn::PermWait))]);
        let rows = vec![row("C:aaaaaa", Liveness::Done)];
        assert_eq!(
            lifecycle_events(&prev, &rows),
            vec![Event::Lifecycle {
                key: "C:aaaaaa".to_string(),
                change: Lifecycle::Finished(Cause::Timeout),
            }]
        );
    }

    // ------------------------------------------------------- attention events

    #[test]
    fn live_to_attention_fires_the_reason() {
        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Live)]);
        let rows = vec![row("C:aaaaaa", Liveness::Attention(Attn::PermWait))];
        assert_eq!(
            attention_events(&prev, &rows),
            vec![Event::Lifecycle {
                key: "C:aaaaaa".to_string(),
                change: Lifecycle::Attention(Attn::PermWait),
            }]
        );
    }

    #[test]
    fn attention_to_live_is_cleared() {
        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Attention(Attn::Stuck))]);
        let rows = vec![row("C:aaaaaa", Liveness::Live)];
        assert_eq!(
            attention_events(&prev, &rows),
            vec![Event::Lifecycle {
                key: "C:aaaaaa".to_string(),
                change: Lifecycle::Cleared,
            }]
        );
    }

    #[test]
    fn switching_attention_reason_fires_again() {
        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Attention(Attn::PermWait))]);
        let rows = vec![row("C:aaaaaa", Liveness::Attention(Attn::Stuck))];
        assert_eq!(
            attention_events(&prev, &rows),
            vec![Event::Lifecycle {
                key: "C:aaaaaa".to_string(),
                change: Lifecycle::Attention(Attn::Stuck),
            }]
        );
    }

    #[test]
    fn a_brand_new_key_starting_in_attention_fires_it_too() {
        let prev = HashMap::new();
        let rows = vec![row("C:aaaaaa", Liveness::Attention(Attn::Stuck))];
        assert_eq!(
            attention_events(&prev, &rows),
            vec![Event::Lifecycle {
                key: "C:aaaaaa".to_string(),
                change: Lifecycle::Attention(Attn::Stuck),
            }]
        );
    }

    #[test]
    fn unchanged_attention_emits_nothing() {
        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Attention(Attn::Stuck))]);
        let rows = vec![row("C:aaaaaa", Liveness::Attention(Attn::Stuck))];
        assert!(attention_events(&prev, &rows).is_empty());
    }

    #[test]
    fn unchanged_liveness_emits_nothing() {
        let prev = HashMap::from([("C:aaaaaa".to_string(), Liveness::Live)]);
        let rows = vec![row("C:aaaaaa", Liveness::Live)];
        assert!(lifecycle_events(&prev, &rows).is_empty());
    }

    // -------------------------------------------------- lifecycle helpers

    fn done(finished_at: Option<f64>, killed: bool) -> Tracked {
        Tracked {
            liveness: Liveness::Done,
            finished_at,
            killed,
            attn_since: None,
        }
    }

    fn live() -> Tracked {
        Tracked {
            liveness: Liveness::Live,
            finished_at: None,
            killed: false,
            attn_since: None,
        }
    }

    fn attention(attn: Attn, attn_since: f64) -> Tracked {
        Tracked {
            liveness: Liveness::Attention(attn),
            finished_at: None,
            killed: false,
            attn_since: Some(attn_since),
        }
    }

    #[test]
    fn a_key_missing_from_the_roster_is_a_vanished_finish() {
        let tracked = HashMap::from([("C:aaaaaa".to_string(), live())]);
        assert_eq!(
            vanished_events(&tracked, &[]),
            vec![Event::Lifecycle {
                key: "C:aaaaaa".to_string(),
                change: Lifecycle::Finished(Cause::Clean),
            }]
        );
    }

    #[test]
    fn a_vanished_attention_key_finishes_as_a_timeout() {
        let tracked = HashMap::from([("C:aaaaaa".to_string(), attention(Attn::Stuck, 0.0))]);
        assert_eq!(
            vanished_events(&tracked, &[]),
            vec![Event::Lifecycle {
                key: "C:aaaaaa".to_string(),
                change: Lifecycle::Finished(Cause::Timeout),
            }]
        );
    }

    #[test]
    fn an_already_done_vanished_key_fires_nothing_again() {
        let tracked = HashMap::from([("C:aaaaaa".to_string(), done(Some(1.0), false))]);
        assert!(vanished_events(&tracked, &[]).is_empty());
    }

    #[test]
    fn a_key_still_on_the_roster_never_vanishes() {
        let tracked = HashMap::from([("C:aaaaaa".to_string(), live())]);
        let rows = vec![row("C:aaaaaa", Liveness::Live)];
        assert!(vanished_events(&tracked, &rows).is_empty());
    }

    #[test]
    fn apply_lifecycle_starts_a_fresh_key_at_its_row_state() {
        let mut tracked = HashMap::new();
        let rows = vec![row("C:aaaaaa", Liveness::Live)];
        let events = vec![Event::Lifecycle {
            key: "C:aaaaaa".to_string(),
            change: Lifecycle::Started,
        }];
        apply_lifecycle(&mut tracked, &events, &rows, 100.0);
        let t = tracked["C:aaaaaa"];
        assert_eq!(t.liveness, Liveness::Live);
        assert_eq!(t.finished_at, None);
        assert!(!t.killed);
    }

    #[test]
    fn apply_lifecycle_starts_a_key_already_done_with_the_linger_clock_running() {
        let mut tracked = HashMap::new();
        let rows = vec![row("C:aaaaaa", Liveness::Done)];
        let events = vec![Event::Lifecycle {
            key: "C:aaaaaa".to_string(),
            change: Lifecycle::Started,
        }];
        apply_lifecycle(&mut tracked, &events, &rows, 100.0);
        assert_eq!(tracked["C:aaaaaa"].finished_at, Some(100.0));
    }

    #[test]
    fn apply_lifecycle_finish_starts_the_linger_clock_and_clears_killed() {
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), live())]);
        let events = vec![Event::Lifecycle {
            key: "C:aaaaaa".to_string(),
            change: Lifecycle::Finished(Cause::Clean),
        }];
        apply_lifecycle(&mut tracked, &events, &[], 200.0);
        let t = tracked["C:aaaaaa"];
        assert_eq!(t.liveness, Liveness::Done);
        assert_eq!(t.finished_at, Some(200.0));
        assert!(!t.killed);
    }

    #[test]
    fn apply_lifecycle_finish_clears_the_attention_clock() {
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), attention(Attn::Stuck, 10.0))]);
        let events = vec![Event::Lifecycle {
            key: "C:aaaaaa".to_string(),
            change: Lifecycle::Finished(Cause::Timeout),
        }];
        apply_lifecycle(&mut tracked, &events, &[], 200.0);
        assert_eq!(tracked["C:aaaaaa"].attn_since, None);
    }

    #[test]
    fn apply_lifecycle_attention_starts_the_clock() {
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), live())]);
        let events = vec![Event::Lifecycle {
            key: "C:aaaaaa".to_string(),
            change: Lifecycle::Attention(Attn::PermWait),
        }];
        apply_lifecycle(&mut tracked, &events, &[], 50.0);
        let t = tracked["C:aaaaaa"];
        assert_eq!(t.liveness, Liveness::Attention(Attn::PermWait));
        assert_eq!(t.attn_since, Some(50.0));
    }

    #[test]
    fn apply_lifecycle_attention_resets_the_clock_on_re_entry() {
        let mut tracked =
            HashMap::from([("C:aaaaaa".to_string(), attention(Attn::PermWait, 10.0))]);
        let events = vec![Event::Lifecycle {
            key: "C:aaaaaa".to_string(),
            change: Lifecycle::Attention(Attn::PermWait),
        }];
        apply_lifecycle(&mut tracked, &events, &[], 999.0);
        assert_eq!(
            tracked["C:aaaaaa"].attn_since,
            Some(999.0),
            "re-entering resets elapsed-in-state rather than preserving the old clock"
        );
    }

    #[test]
    fn apply_lifecycle_cleared_stops_the_clock() {
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), attention(Attn::Stuck, 10.0))]);
        let events = vec![Event::Lifecycle {
            key: "C:aaaaaa".to_string(),
            change: Lifecycle::Cleared,
        }];
        apply_lifecycle(&mut tracked, &events, &[], 50.0);
        let t = tracked["C:aaaaaa"];
        assert_eq!(t.liveness, Liveness::Live);
        assert_eq!(t.attn_since, None);
    }

    #[test]
    fn attn_elapsed_reflects_time_since_the_tracked_clock_started() {
        let tracked = HashMap::from([("C:aaaaaa".to_string(), attention(Attn::Stuck, 100.0))]);
        let mut rows = vec![row("C:aaaaaa", Liveness::Attention(Attn::Stuck))];
        attach_attn_elapsed(&tracked, &mut rows, 145.0);
        assert_eq!(rows[0].attn_elapsed, Some(45.0));
    }

    #[test]
    fn attn_elapsed_is_none_outside_attention() {
        let tracked = HashMap::from([("C:aaaaaa".to_string(), live())]);
        let mut rows = vec![row("C:aaaaaa", Liveness::Live)];
        attach_attn_elapsed(&tracked, &mut rows, 145.0);
        assert_eq!(rows[0].attn_elapsed, None);
    }

    /// End-to-end through `apply_lifecycle` + `attach_attn_elapsed`, as
    /// `scan` runs them: entering PermWait starts the clock, a tick later
    /// elapsed has grown, clearing to Live stops it, and re-entering PermWait
    /// starts the clock over rather than picking the old one back up.
    #[test]
    fn elapsed_in_state_resets_on_re_entry_end_to_end() {
        let mut tracked = HashMap::new();

        let enter = vec![Event::Lifecycle {
            key: "C:aaaaaa".to_string(),
            change: Lifecycle::Attention(Attn::PermWait),
        }];
        apply_lifecycle(&mut tracked, &enter, &[], 0.0);
        let mut rows = vec![row("C:aaaaaa", Liveness::Attention(Attn::PermWait))];
        attach_attn_elapsed(&tracked, &mut rows, 45.0);
        assert_eq!(rows[0].attn_elapsed, Some(45.0));

        let clear = vec![Event::Lifecycle {
            key: "C:aaaaaa".to_string(),
            change: Lifecycle::Cleared,
        }];
        apply_lifecycle(&mut tracked, &clear, &[], 46.0);

        let reenter = vec![Event::Lifecycle {
            key: "C:aaaaaa".to_string(),
            change: Lifecycle::Attention(Attn::PermWait),
        }];
        apply_lifecycle(&mut tracked, &reenter, &[], 100.0);
        let mut rows = vec![row("C:aaaaaa", Liveness::Attention(Attn::PermWait))];
        attach_attn_elapsed(&tracked, &mut rows, 105.0);
        assert_eq!(
            rows[0].attn_elapsed,
            Some(5.0),
            "re-entry should measure from the new since, not the original one"
        );
    }

    #[test]
    fn apply_lifecycle_resume_clears_the_linger_clock_and_killed() {
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), done(Some(50.0), true))]);
        let rows = vec![row("C:aaaaaa", Liveness::Live)];
        let events = vec![Event::Lifecycle {
            key: "C:aaaaaa".to_string(),
            change: Lifecycle::Resumed,
        }];
        apply_lifecycle(&mut tracked, &events, &rows, 300.0);
        let t = tracked["C:aaaaaa"];
        assert_eq!(t.liveness, Liveness::Live);
        assert_eq!(t.finished_at, None);
        assert!(!t.killed);
    }

    #[test]
    fn refresh_liveness_syncs_a_state_change_that_fired_no_event() {
        // Live -> Attention(Stuck) fires no Lifecycle event (neither side is
        // Done), but the stored liveness still has to track it so a later
        // Attention -> Done comparison sees a real transition.
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), live())]);
        let rows = vec![row("C:aaaaaa", Liveness::Attention(Attn::Stuck))];
        refresh_liveness(&mut tracked, &rows);
        assert_eq!(
            tracked["C:aaaaaa"].liveness,
            Liveness::Attention(Attn::Stuck)
        );
    }

    #[test]
    fn refresh_liveness_ignores_keys_no_longer_on_the_roster() {
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), done(Some(1.0), false))]);
        refresh_liveness(&mut tracked, &[]);
        assert_eq!(tracked["C:aaaaaa"].liveness, Liveness::Done, "unchanged");
    }

    fn panes_with(keys: &[&str]) -> HashMap<String, Box<dyn Tailer>> {
        struct Mute;
        impl Tailer for Mute {
            fn poll(&mut self) -> Vec<StyledLine> {
                Vec::new()
            }
        }
        keys.iter()
            .map(|k| (k.to_string(), Box::new(Mute) as Box<dyn Tailer>))
            .collect()
    }

    #[test]
    fn linger_expire_closes_a_pane_once_it_has_been_done_long_enough() {
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), done(Some(0.0), false))]);
        let mut panes = panes_with(&["C:aaaaaa"]);
        linger_expire(&mut tracked, &mut panes, 60.0, 60.0);
        assert!(panes.is_empty(), "pane should have closed");
        assert!(tracked["C:aaaaaa"].killed);
    }

    #[test]
    fn linger_expire_leaves_a_pane_that_has_not_yet_aged_out() {
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), done(Some(0.0), false))]);
        let mut panes = panes_with(&["C:aaaaaa"]);
        linger_expire(&mut tracked, &mut panes, 60.0, 59.999);
        assert!(!panes.is_empty());
        assert!(!tracked["C:aaaaaa"].killed);
    }

    #[test]
    fn linger_zero_never_expires_a_pane() {
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), done(Some(0.0), false))]);
        let mut panes = panes_with(&["C:aaaaaa"]);
        linger_expire(&mut tracked, &mut panes, 0.0, 1_000_000.0);
        assert!(!panes.is_empty(), "linger=0 keeps the pane forever");
        assert!(!tracked["C:aaaaaa"].killed);
    }

    #[test]
    fn linger_expire_never_touches_a_live_session() {
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), live())]);
        let mut panes = panes_with(&["C:aaaaaa"]);
        linger_expire(&mut tracked, &mut panes, 60.0, 1_000_000.0);
        assert!(!panes.is_empty());
    }

    #[test]
    fn forget_drops_a_killed_key_once_it_leaves_the_roster() {
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), done(Some(0.0), true))]);
        let mut panes = HashMap::new();
        forget(&mut tracked, &mut panes, &[]);
        assert!(tracked.is_empty());
    }

    #[test]
    fn forget_keeps_a_killed_key_still_on_the_roster() {
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), done(Some(0.0), true))]);
        let mut panes = HashMap::new();
        let rows = vec![row("C:aaaaaa", Liveness::Done)];
        forget(&mut tracked, &mut panes, &rows);
        assert!(tracked.contains_key("C:aaaaaa"));
    }

    #[test]
    fn forget_keeps_a_gone_key_that_was_never_killed() {
        // linger=0: the session lingers forever, so disappearing from every
        // source still must not forget it.
        let mut tracked = HashMap::from([("C:aaaaaa".to_string(), done(Some(0.0), false))]);
        let mut panes = HashMap::new();
        forget(&mut tracked, &mut panes, &[]);
        assert!(
            tracked.contains_key("C:aaaaaa"),
            "linger=0 sessions are never forgotten just for vanishing"
        );
    }

    // ------------------------------------------------------------ try_open

    struct StubTailer;
    impl Tailer for StubTailer {
        fn poll(&mut self) -> Vec<StyledLine> {
            Vec::new()
        }
    }

    /// A [`Deck`] whose roster is irrelevant to `try_open` — only
    /// `open_tailer` matters here — configurable to refuse a chosen key,
    /// standing in for a source that can't tail that session.
    struct StubDeck {
        refuse: HashSet<String>,
    }

    impl Deck for StubDeck {
        fn roster(&mut self, _now: f64, _fresh_window: f64, _idle_timeout: f64) -> Vec<RosterRow> {
            Vec::new()
        }

        fn open_tailer(
            &mut self,
            key: &str,
            _session_id: &str,
            _replay: Replay,
        ) -> Option<Box<dyn Tailer>> {
            if self.refuse.contains(key) {
                None
            } else {
                Some(Box::new(StubTailer))
            }
        }
    }

    fn ids(keys: &[&str]) -> HashMap<String, String> {
        keys.iter()
            .map(|k| (k.to_string(), format!("id-{k}")))
            .collect()
    }

    fn no_pins() -> HashSet<String> {
        HashSet::new()
    }

    fn pins(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn try_open_opens_directly_under_the_cap() {
        let mut deck = StubDeck {
            refuse: HashSet::new(),
        };
        let mut panes = HashMap::new();
        let mut tracked = HashMap::new();
        let ids = ids(&["a"]);
        let outcome = try_open(
            &mut deck,
            &mut panes,
            &mut tracked,
            &ids,
            "a",
            2,
            &no_pins(),
            Replay::DEFAULT,
        );
        assert!(matches!(outcome, OpenOutcome::Opened));
        assert!(panes.contains_key("a"));
    }

    #[test]
    fn try_open_evicts_the_oldest_finished_pane_when_full() {
        let mut deck = StubDeck {
            refuse: HashSet::new(),
        };
        let mut panes = panes_with(&["old", "new"]);
        let mut tracked = HashMap::from([
            ("old".to_string(), done(Some(1.0), false)),
            ("new".to_string(), done(Some(2.0), false)),
        ]);
        let ids = ids(&["fresh"]);
        let outcome = try_open(
            &mut deck,
            &mut panes,
            &mut tracked,
            &ids,
            "fresh",
            2,
            &no_pins(),
            Replay::DEFAULT,
        );
        assert!(matches!(outcome, OpenOutcome::Evicted(ref v) if v == "old"));
        assert!(
            !panes.contains_key("old"),
            "the oldest finish should be evicted"
        );
        assert!(panes.contains_key("new"), "the newer finish should survive");
        assert!(panes.contains_key("fresh"));
        assert!(tracked["old"].killed);
    }

    /// #42: a pinned finished pane is never picked as the eviction victim,
    /// even when it is the oldest finish on the deck — the newer, unpinned
    /// finish gets evicted in its place.
    #[test]
    fn try_open_never_evicts_a_pinned_key_even_when_it_is_oldest() {
        let mut deck = StubDeck {
            refuse: HashSet::new(),
        };
        let mut panes = panes_with(&["old-pinned", "new"]);
        let mut tracked = HashMap::from([
            ("old-pinned".to_string(), done(Some(1.0), false)),
            ("new".to_string(), done(Some(2.0), false)),
        ]);
        let ids = ids(&["fresh"]);
        let outcome = try_open(
            &mut deck,
            &mut panes,
            &mut tracked,
            &ids,
            "fresh",
            2,
            &pins(&["old-pinned"]),
            Replay::DEFAULT,
        );
        assert!(matches!(outcome, OpenOutcome::Evicted(ref v) if v == "new"));
        assert!(panes.contains_key("old-pinned"), "the pin held its slot");
        assert!(!panes.contains_key("new"));
        assert!(!tracked["old-pinned"].killed);
    }

    /// #42: when every finished slot is pinned, eviction has nowhere to
    /// take a slot from, so the new key simply waits — same as an all-live
    /// fleet.
    #[test]
    fn try_open_skips_when_every_finished_slot_is_pinned() {
        let mut deck = StubDeck {
            refuse: HashSet::new(),
        };
        let mut panes = panes_with(&["a", "b"]);
        let mut tracked = HashMap::from([
            ("a".to_string(), done(Some(1.0), false)),
            ("b".to_string(), done(Some(2.0), false)),
        ]);
        let ids = ids(&["fresh"]);
        let outcome = try_open(
            &mut deck,
            &mut panes,
            &mut tracked,
            &ids,
            "fresh",
            2,
            &pins(&["a", "b"]),
            Replay::DEFAULT,
        );
        assert!(matches!(outcome, OpenOutcome::Skipped));
        assert!(panes.contains_key("a"));
        assert!(panes.contains_key("b"));
        assert!(!panes.contains_key("fresh"));
    }

    #[test]
    fn try_open_skips_when_every_occupied_slot_is_live() {
        let mut deck = StubDeck {
            refuse: HashSet::new(),
        };
        let mut panes = panes_with(&["a"]);
        let mut tracked = HashMap::from([("a".to_string(), live())]);
        let ids = ids(&["b"]);
        let outcome = try_open(
            &mut deck,
            &mut panes,
            &mut tracked,
            &ids,
            "b",
            1,
            &no_pins(),
            Replay::DEFAULT,
        );
        assert!(matches!(outcome, OpenOutcome::Skipped));
        assert!(!panes.contains_key("b"));
        assert!(panes.contains_key("a"), "an all-live fleet loses no slot");
    }

    #[test]
    fn try_open_skips_an_unknown_key_without_touching_the_deck() {
        let mut deck = StubDeck {
            refuse: HashSet::new(),
        };
        let mut panes = HashMap::new();
        let mut tracked = HashMap::new();
        let outcome = try_open(
            &mut deck,
            &mut panes,
            &mut tracked,
            &HashMap::new(),
            "ghost",
            2,
            &no_pins(),
            Replay::DEFAULT,
        );
        assert!(matches!(outcome, OpenOutcome::Skipped));
    }

    #[test]
    fn try_open_evicts_nobody_when_the_source_refuses_the_new_key() {
        let mut deck = StubDeck {
            refuse: HashSet::from(["fresh".to_string()]),
        };
        let mut panes = panes_with(&["old"]);
        let mut tracked = HashMap::from([("old".to_string(), done(Some(1.0), false))]);
        let ids = ids(&["fresh"]);
        let outcome = try_open(
            &mut deck,
            &mut panes,
            &mut tracked,
            &ids,
            "fresh",
            1,
            &no_pins(),
            Replay::DEFAULT,
        );
        assert!(matches!(outcome, OpenOutcome::Skipped));
        assert!(
            panes.contains_key("old"),
            "a failed open must not cost the victim its slot"
        );
        assert!(!tracked["old"].killed);
    }

    // ------------------------------------------------------ alert wiring

    #[test]
    fn build_transitions_covers_every_row_using_prev_liveness_as_from() {
        let prev = HashMap::from([("a".to_string(), Liveness::Live)]);
        let rows = vec![row("a", Liveness::Attention(Attn::Stuck))];
        let mut errors = HashMap::new();
        let transitions = build_transitions(&prev, &rows, 100.0, &mut errors);
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].from, Liveness::Live);
        assert_eq!(transitions[0].to, Liveness::Attention(Attn::Stuck));
        assert_eq!(transitions[0].label, "a");
    }

    #[test]
    fn build_transitions_defaults_from_to_the_current_state_for_a_brand_new_key() {
        // No prior tick's liveness for this key: from == to, so a session
        // that shows up already Done never reads as a fresh finish.
        let rows = vec![row("a", Liveness::Done)];
        let mut errors = HashMap::new();
        let transitions = build_transitions(&HashMap::new(), &rows, 0.0, &mut errors);
        assert_eq!(transitions[0].from, transitions[0].to);
        assert_eq!(transitions[0].done_cause, None);
    }

    #[test]
    fn build_transitions_maps_clean_and_timeout_finishes_to_done_cause() {
        let prev = HashMap::from([
            ("clean".to_string(), Liveness::Live),
            ("timeout".to_string(), Liveness::Attention(Attn::PermWait)),
        ]);
        let rows = vec![row("clean", Liveness::Done), row("timeout", Liveness::Done)];
        let mut errors = HashMap::new();
        let transitions = build_transitions(&prev, &rows, 0.0, &mut errors);
        assert_eq!(transitions[0].done_cause, Some(DoneCause::TurnDone));
        assert_eq!(transitions[1].done_cause, Some(DoneCause::Timeout));
    }

    #[test]
    fn build_transitions_leaves_done_cause_none_without_a_done_boundary() {
        let prev = HashMap::from([("a".to_string(), Liveness::Live)]);
        let rows = vec![row("a", Liveness::Live)];
        let mut errors = HashMap::new();
        let transitions = build_transitions(&prev, &rows, 0.0, &mut errors);
        assert_eq!(transitions[0].done_cause, None);
    }

    #[test]
    fn build_transitions_derives_started_at_and_state_since_from_the_row() {
        let mut r = row("a", Liveness::Attention(Attn::Stuck));
        r.last_ts = 1000.0;
        r.elapsed = Some(30.0);
        r.attn_elapsed = Some(15.0);
        let prev = HashMap::from([("a".to_string(), Liveness::Live)]);
        let mut errors = HashMap::new();
        let transitions = build_transitions(&prev, &[r], 1000.0, &mut errors);
        assert_eq!(transitions[0].started_at, 970.0);
        assert_eq!(transitions[0].state_since, 985.0);
    }

    #[test]
    fn build_transitions_drains_a_recorded_error_line() {
        let rows = vec![row("a", Liveness::Live)];
        let mut errors = HashMap::from([("a".to_string(), "boom".to_string())]);
        let transitions = build_transitions(&HashMap::new(), &rows, 0.0, &mut errors);
        assert_eq!(transitions[0].error_line, Some("boom".to_string()));
        assert!(
            errors.is_empty(),
            "the error line should be drained, not just read"
        );
    }

    #[test]
    fn pump_panes_records_the_latest_error_line_per_key() {
        struct ErrTailer(Vec<StyledLine>);
        impl Tailer for ErrTailer {
            fn poll(&mut self) -> Vec<StyledLine> {
                std::mem::take(&mut self.0)
            }
        }
        let lines = vec![
            StyledLine(vec![Seg::new(Sem::Plain, "ok line")]),
            StyledLine(vec![Seg::new(Sem::Error, "first error")]),
            StyledLine(vec![Seg::new(Sem::Plain, "more output")]),
            StyledLine(vec![Seg::new(Sem::Error, "second error")]),
        ];
        let mut panes: HashMap<String, Box<dyn Tailer>> = HashMap::new();
        panes.insert("a".to_string(), Box::new(ErrTailer(lines)));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut errors = HashMap::new();

        pump_panes(&mut panes, &tx, &mut errors).unwrap();

        assert_eq!(
            errors.get("a"),
            Some(&"second error".to_string()),
            "the most recent error line this tick wins"
        );
        match rx.recv().unwrap() {
            Event::PaneLines { lines, .. } => {
                assert_eq!(lines.len(), 4, "every polled line is still forwarded")
            }
            other => panic!("expected PaneLines, got {other:?}"),
        }
    }

    #[test]
    fn pump_panes_leaves_errors_untouched_with_no_error_line_this_tick() {
        struct QuietTailer(Vec<StyledLine>);
        impl Tailer for QuietTailer {
            fn poll(&mut self) -> Vec<StyledLine> {
                std::mem::take(&mut self.0)
            }
        }
        let mut panes: HashMap<String, Box<dyn Tailer>> = HashMap::new();
        panes.insert(
            "a".to_string(),
            Box::new(QuietTailer(vec![StyledLine(vec![Seg::new(
                Sem::Plain,
                "all clear",
            )])])),
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let mut errors = HashMap::new();

        pump_panes(&mut panes, &tx, &mut errors).unwrap();

        assert!(errors.is_empty());
        assert!(rx.try_recv().is_ok());
    }
}
