//! `hermon agent`: the in-container half of the remote wire protocol.
//!
//! Deliberately dumb (#89): read the same three stores `watch`/`ls` read —
//! inside a container they're local files again, `lsof` liveness included
//! — and print [`AgentMsg`] frames on stdout instead of drawing a UI. No
//! notifications, no TUI, no roster classification (that's the host's job);
//! stdout carries only frames, stderr carries diagnostics.
//!
//! Two threads: [`run`]'s caller thread drives the scan/tail loop and owns
//! every [`Tailer`] (which need not be `Send`), while a second thread reads
//! [`HostCmd`] lines off stdin and a third owns the actual stdout writes so
//! a stalled reader can't wedge the scan loop (see [`spawn_writer`]).

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::remote::proto::{
    AgentMsg, Decoded, HostCmd, PROTO_VERSION, decode_host_cmd, encode_agent_msg,
};
use crate::roster::Sources;
use crate::source::{Replay, SessionMeta, Source, Tailer};

/// How often open tails are polled for new lines — the same cadence
/// `engine::PANE_TICK` polls TUI panes at, since a tailed session should
/// read live either way.
const TAIL_TICK: Duration = Duration::from_millis(300);

/// Frames queued for the writer thread before a stalled reader starts
/// costing `Tail` frames (see [`spawn_writer`]). Generous enough to absorb
/// a burst without dropping anything under normal conditions.
const WRITER_QUEUE_CAP: usize = 256;

/// Store locations and the `Snap` cadence — the agent's share of
/// [`crate::cli::SourceArgs`]; everything else there (notify flags,
/// `--max-panes`, `--linger`, …) belongs to UI modes this one doesn't have.
pub struct AgentConfig {
    pub claude_dir: String,
    pub hermes_db: String,
    pub opencode_db: String,
    pub idle_timeout: f64,
    pub fresh_window: f64,
    pub interval: Duration,
}

/// A [`HostCmd`], decoded and handed from the stdin thread to the main
/// loop. `Shutdown` doubles as "stdin hit EOF" — the transport died either
/// way, and an orphaned agent in a container must not linger.
enum Cmd {
    OpenTail { key: String, replay: Replay },
    CloseTail { key: String },
    Shutdown,
}

/// Runs until `HostCmd::Shutdown` arrives or stdin closes. Never returns an
/// `Err` for anything a malformed frame or a stalled pipe could cause —
/// only a genuinely unrecoverable setup failure would, and there isn't one
/// here.
pub fn run(config: AgentConfig) -> anyhow::Result<()> {
    let (writer_tx, writer) = spawn_writer();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    thread::spawn(move || read_stdin(cmd_tx));

    send(
        &writer_tx,
        AgentMsg::Hello {
            proto_version: PROTO_VERSION,
            hostname: hostname(),
            sources: vec!["claude".into(), "hermes".into(), "opencode".into()],
        },
    );

    let mut sources = Sources::new(&config.claude_dir, &config.hermes_db, &config.opencode_db);
    let mut tails: HashMap<String, Box<dyn Tailer>> = HashMap::new();

    let mut next_snap = Instant::now();
    let mut next_tail_tick = Instant::now();
    let reason = loop {
        if Instant::now() >= next_snap {
            let sessions = snapshot(&mut sources, now_secs(), &config);
            send(&writer_tx, AgentMsg::Snap { sessions });
            next_snap = Instant::now() + config.interval;
        }
        if Instant::now() >= next_tail_tick {
            pump_tails(&writer_tx, &mut tails);
            next_tail_tick = Instant::now() + TAIL_TICK;
        }

        let deadline = next_snap.min(next_tail_tick);
        match cmd_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Cmd::Shutdown) => break "shutdown",
            Ok(Cmd::OpenTail { key, replay }) => {
                let session_id = key.split_once(':').map_or(key.as_str(), |(_, id)| id);
                if let Some(tailer) = sources.open_tailer(&key, session_id, replay) {
                    tails.insert(key, tailer);
                    next_tail_tick = Instant::now(); // surface the replay promptly
                }
            }
            Ok(Cmd::CloseTail { key }) => {
                tails.remove(&key);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break "stdin closed",
        }
    };

    send(
        &writer_tx,
        AgentMsg::Bye {
            reason: reason.to_string(),
        },
    );
    drop(writer_tx); // closes the writer thread's channel so it can drain and exit
    let _ = writer.join();
    Ok(())
}

/// Every session across all three sources, each tagged with its source
/// prefix so a later `HostCmd::OpenTail` can name it back. The wire
/// `SessionMeta` (unlike `roster::RosterRow`) carries no separate key
/// field, so the prefix `Sources::open_tailer` dispatches on rides in `id`
/// itself as `"C:<real id>"` rather than beside it — the id round-trips
/// through the host unchanged and `run`'s `OpenTail` arm splits it back
/// apart the same way it was built here.
///
/// Unfiltered and unclassified, deliberately: this is a full snapshot, not
/// a diff, and liveness classification is the host's job, not the agent's.
fn snapshot(sources: &mut Sources, now: f64, config: &AgentConfig) -> Vec<SessionMeta> {
    let mut out = Vec::new();
    for s in sources.claude.sessions(now, config.idle_timeout) {
        out.push(keyed("C", s));
    }
    for s in sources.hermes.sessions() {
        out.push(keyed("H", s));
    }
    for s in sources.opencode.sessions(now - config.fresh_window) {
        out.push(keyed("O", s));
    }
    out
}

fn keyed(prefix: &str, mut s: SessionMeta) -> SessionMeta {
    s.id = format!("{prefix}:{}", s.id);
    s
}

/// Polls every open tail and forwards new lines as `Tail` frames, dropping
/// (never blocking on) a tail whose frame can't be queued right now — a
/// stalled reader must cost tail lines, not wedge the scan loop that also
/// has to get the next `Snap` out.
fn pump_tails(writer_tx: &SyncSender<AgentMsg>, tails: &mut HashMap<String, Box<dyn Tailer>>) {
    for (key, tailer) in tails.iter_mut() {
        let lines = tailer.poll();
        if lines.is_empty() {
            continue;
        }
        let msg = AgentMsg::Tail {
            key: key.clone(),
            lines,
        };
        match writer_tx.try_send(msg) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {} // reader is stalled; drop this tail's frame
        }
    }
}

/// Sends a frame the delivery-guaranteed way (`Hello`, `Snap`, `Bye`):
/// blocks until the writer thread has room, rather than risking the loss
/// of a snapshot the way a dropped `Tail` frame is tolerated.
fn send(writer_tx: &SyncSender<AgentMsg>, msg: AgentMsg) {
    let _ = writer_tx.send(msg);
}

/// Owns the only handle on stdout: every frame is line-delimited JSON,
/// unbuffered (flushed per line) so a reader tailing the pipe sees each
/// frame as soon as it's written. Runs until its sender is dropped or a
/// write fails (reader gone) — either way there is nothing left to do but
/// stop.
fn spawn_writer() -> (SyncSender<AgentMsg>, JoinHandle<()>) {
    let (tx, rx): (SyncSender<AgentMsg>, Receiver<AgentMsg>) = mpsc::sync_channel(WRITER_QUEUE_CAP);
    let handle = thread::spawn(move || {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        while let Ok(msg) = rx.recv() {
            let line = encode_agent_msg(&msg);
            if writeln!(out, "{line}").is_err() || out.flush().is_err() {
                break;
            }
        }
    });
    (tx, handle)
}

/// Reads `HostCmd` lines off stdin until EOF, handing each to the main
/// loop as a [`Cmd`]. A malformed line is `ParseSkip` per #88's decoder: it
/// gets one stderr diagnostic and the loop keeps running — garbage on
/// stdin is never a reason to exit. EOF (or a read error) sends `Shutdown`
/// itself, since that's the only way the main loop learns the transport
/// died.
fn read_stdin(cmd_tx: Sender<Cmd>) {
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        match decode_host_cmd(&line) {
            Decoded::Msg(HostCmd::OpenTail { key, replay }) => {
                let _ = cmd_tx.send(Cmd::OpenTail { key, replay });
            }
            Decoded::Msg(HostCmd::CloseTail { key }) => {
                let _ = cmd_tx.send(Cmd::CloseTail { key });
            }
            Decoded::Msg(HostCmd::Shutdown) => {
                let _ = cmd_tx.send(Cmd::Shutdown);
                return;
            }
            Decoded::ParseSkip => {
                eprintln!("hermon agent: skipping malformed stdin line");
            }
        }
    }
    let _ = cmd_tx.send(Cmd::Shutdown); // stdin EOF: the transport died
}

/// Best-effort hostname for `Hello`, with no extra dependency: containers
/// almost always carry one at `/etc/hostname` (set by the runtime); `$HOSTNAME`
/// and a fixed fallback cover everywhere that file doesn't exist (e.g. local
/// dev/test on macOS). Cosmetic only — nothing downstream keys behavior off
/// this value.
fn hostname() -> String {
    if let Ok(name) = std::fs::read_to_string("/etc/hostname") {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    if let Ok(name) = std::env::var("HOSTNAME")
        && !name.is_empty()
    {
        return name;
    }
    "unknown-host".to_string()
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}
