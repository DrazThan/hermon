//! The host half of the remote wire protocol (#90): one remote agent,
//! wearing the same [`Source`] face as the three on-disk stores.
//!
//! A [`RemoteSource`] owns a supervisor thread that spawns the transport
//! child (`hermon agent` over docker/ssh/plain `Command` — #91 builds the
//! argv, this module never looks at it), a reader thread that demuxes the
//! agent's frames into a snapshot plus one queue per tail the host opened,
//! and a reconnect loop with capped backoff. Everything downstream —
//! [`crate::roster`], the views, notifications — sees only prefixed keys
//! (`job1/C:0f865f`) and ordinary [`SessionMeta`]s, and [`classify`] still
//! runs host-side on the agent's timestamps exactly as it does for a local
//! source.
//!
//! [`classify`]: crate::source::classify
//!
//! **The agent is adversarial.** A compromised container controls every
//! byte on this pipe, so the reader treats the stream as hostile input:
//! frames are read under a hard [`MAX_FRAME_BYTES`] cap (never an unbounded
//! `read_line`), snapshots are truncated at [`MAX_SNAP_SESSIONS`], `Tail`
//! frames naming keys the host never opened are dropped without allocating
//! demux state, and *every* string — titles, models, `last_line`, tail text,
//! the `Hello` hostname — is laundered through [`sanitize`] here on the
//! host. The agent-side sanitizer runs on hostile ground and cannot be
//! trusted.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::remote::proto::{
    AgentMsg, Decoded, HostCmd, PROTO_VERSION, decode_agent_msg, encode_host_cmd,
};
use crate::render::{Seg, Sem, StyledLine, sanitize};
use crate::source::{LastEvent, Replay, SessionMeta, Source, Tailer};

/// Longest wire frame the host will buffer. A hostile agent must not be
/// able to make us allocate without bound, so a longer line is discarded
/// (counted, and reported on the roster) instead of read.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Most sessions one `Snap` may contribute to the roster. In practice the
/// frame cap above rejects a snapshot this large long before this triggers;
/// it is the second line of defence, so a future framing change can't turn
/// into unbounded roster growth.
pub const MAX_SNAP_SESSIONS: usize = 10_000;

/// Lines held per open tail before the oldest are dropped. A pane shows a
/// screenful; a flood is the agent's problem, not the host's memory.
const MAX_TAIL_LINES: usize = 4_096;

/// Reconnect backoff floor and ceiling, doubling in between.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// A child that dies this fast never really connected — a bad image, a
/// missing binary, a container that exits on start.
const INSTANT_EXIT: Duration = Duration::from_secs(1);

/// Instant exits in a row before respawns are pinned at [`BACKOFF_MAX`] for
/// good: a remote that can't start must not become a respawn storm.
const INSTANT_EXIT_STRIKES: u32 = 3;

/// A connection that lasted this long counts as healthy, so the next
/// failure starts the backoff over rather than resuming where it left off.
const HEALTHY_UPTIME: Duration = Duration::from_secs(10);

/// How far a remote's timestamps may run ahead of the host clock before the
/// skew is worth a warning. `classify` compares the agent's `last_ts` to the
/// *host's* now, so a remote clock off by more than `idle_timeout` misreads
/// liveness outright; this sits below the 180s default so the warning
/// precedes the misclassification.
///
/// Measured on the first snapshot rather than on `Hello`, which carries no
/// timestamp (#88's frame is frozen), and only in the *future* direction: a
/// timestamp in the past is indistinguishable from a session that has
/// simply been idle a while.
const SKEW_WARN_SECS: f64 = 120.0;

/// How often the connection loop looks up from the command channel to
/// notice that the child died or the host is shutting down.
const CMD_TICK: Duration = Duration::from_millis(100);

/// Where a remote's link stands, as the roster narrates it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Link {
    /// No agent has ever said `Hello` on this transport.
    #[default]
    Connecting,
    /// A `Hello` at our protocol version arrived; frames are trusted.
    Up { hostname: String },
    /// Connected at least once, the child is gone, a respawn is pending.
    Down,
    /// The agent speaks a different protocol version. Sticky: reconnecting
    /// cannot fix it, only upgrading the agent can.
    ProtoMismatch { theirs: u32 },
}

/// One open tail's demux state. Created only when the *host* opens the tail,
/// which is what stops a hostile agent from growing host memory by inventing
/// keys.
#[derive(Debug, Default)]
struct TailQueue {
    /// Replayed again after a reconnect, so a pane refills itself instead of
    /// going permanently blank when the agent restarts under it.
    replay: Replay,
    lines: VecDeque<StyledLine>,
}

/// Everything the reader thread writes and the roster reads.
#[derive(Debug, Default)]
struct Shared {
    link: Link,
    /// The newest `Snap`, sanitized and capped. Kept across a disconnect so
    /// the sessions stay on the deck for their fresh window rather than
    /// vanishing the instant the pipe breaks.
    snapshot: Vec<SessionMeta>,
    tails: HashMap<String, TailQueue>,
    /// Frames refused for exceeding [`MAX_FRAME_BYTES`], this connection.
    oversized: u64,
    /// The newest `Snap` was truncated at [`MAX_SNAP_SESSIONS`].
    truncated: bool,
    /// Seconds the remote clock runs ahead of ours, warned once per
    /// connection.
    skew: Option<f64>,
    /// The transport itself could not be started (bad argv, missing docker).
    spawn_error: Option<String>,
}

/// A remote agent as the fourth [`Source`]. Constructing one starts the
/// transport immediately and keeps it running — sessions appear once the
/// agent's first `Snap` lands, and survive its restarts.
pub struct RemoteSource {
    name: String,
    shared: Arc<Mutex<Shared>>,
    cmds: Sender<HostCmd>,
    stop: Arc<AtomicBool>,
    supervisor: Option<JoinHandle<()>>,
}

impl RemoteSource {
    /// Starts `transport` and supervises it. The `Command` is prebuilt by
    /// the caller (#91's `cmd:`/`docker:`/`ssh:` argv); this module only
    /// spawns it, wires stdio, and respawns it when it dies.
    pub fn new(name: impl Into<String>, transport: Command) -> Self {
        let name = clean_name(&name.into());
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (cmds, cmd_rx) = mpsc::channel();

        let supervisor = {
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            let cmds = cmds.clone();
            thread::spawn(move || supervise(transport, &shared, &cmd_rx, &cmds, &stop))
        };

        RemoteSource {
            name,
            shared,
            cmds,
            stop,
            supervisor: Some(supervisor),
        }
    }

    /// The roster label this remote's keys are prefixed with.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn link(&self) -> Link {
        lock(&self.shared).link.clone()
    }

    /// The newest snapshot, with a dim disconnected marker folded into each
    /// row while the link is down — a session whose agent we can no longer
    /// hear is not the same as one that is quiet.
    pub fn sessions(&self) -> Vec<SessionMeta> {
        let shared = lock(&self.shared);
        let down = !matches!(shared.link, Link::Up { .. });
        shared
            .snapshot
            .iter()
            .cloned()
            .map(|s| if down { mark_disconnected(s) } else { s })
            .collect()
    }

    /// What this remote itself has to say on the deck: its link state, plus
    /// any hardening notice (a truncated snapshot, refused frames, a skewed
    /// clock). Empty for a healthy, quiet remote.
    pub fn notices(&self) -> Vec<String> {
        notices(&self.name, &lock(&self.shared))
    }
}

impl Source for RemoteSource {
    fn sessions(&mut self) -> Vec<SessionMeta> {
        RemoteSource::sessions(self)
    }

    fn last_tool(&mut self, session_id: &str) -> String {
        lock(&self.shared)
            .snapshot
            .iter()
            .find(|s| s.id == session_id)
            .map_or_else(|| "-".to_string(), |s| s.last_tool.clone())
    }

    /// `session_id` is the agent's own key for the session (`C:<uuid>`),
    /// which is what [`crate::roster::RosterRow::id`] carries for a remote
    /// row and what `OpenTail` names. Unknown ids get no tailer: the host
    /// only ever opens keys it has seen in a snapshot, so an invented one
    /// never reaches the demux.
    fn open_tailer(&self, session_id: &str, replay: Replay) -> Option<Box<dyn Tailer>> {
        let mut shared = lock(&self.shared);
        if !shared.snapshot.iter().any(|s| s.id == session_id) {
            return None;
        }
        shared.tails.insert(
            session_id.to_string(),
            TailQueue {
                replay,
                lines: VecDeque::new(),
            },
        );
        drop(shared);

        let _ = self.cmds.send(HostCmd::OpenTail {
            key: session_id.to_string(),
            replay,
        });
        Some(Box::new(RemoteTailer {
            key: session_id.to_string(),
            shared: Arc::clone(&self.shared),
            cmds: self.cmds.clone(),
            down_notified: false,
        }))
    }
}

impl Drop for RemoteSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.supervisor.take() {
            let _ = handle.join();
        }
    }
}

/// A pane onto one remote session, fed by the reader thread's demux.
pub struct RemoteTailer {
    key: String,
    shared: Arc<Mutex<Shared>>,
    cmds: Sender<HostCmd>,
    down_notified: bool,
}

impl Tailer for RemoteTailer {
    fn poll(&mut self) -> Vec<StyledLine> {
        let mut shared = lock(&self.shared);
        let down = !matches!(shared.link, Link::Up { .. });
        let mut out: Vec<StyledLine> = shared
            .tails
            .get_mut(&self.key)
            .map(|q| q.lines.drain(..).collect())
            .unwrap_or_default();
        drop(shared);

        // One notice per disconnect, per the Tailer contract — the pane
        // refills itself when the agent comes back.
        if down && !self.down_notified {
            out.push(StyledLine(vec![Seg::new(
                Sem::Dim,
                "⌁ disconnected — reconnecting…",
            )]));
        }
        self.down_notified = down;
        out
    }
}

impl Drop for RemoteTailer {
    fn drop(&mut self) {
        lock(&self.shared).tails.remove(&self.key);
        let _ = self.cmds.send(HostCmd::CloseTail {
            key: self.key.clone(),
        });
    }
}

// ------------------------------------------------------------ pure pieces

/// Reconnect scheduling, as a value: no child process, no clock, so the
/// policy can be unit-tested on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    next: Duration,
    strikes: u32,
    pinned: bool,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            next: BACKOFF_MIN,
            strikes: 0,
            pinned: false,
        }
    }
}

impl Backoff {
    pub fn new() -> Self {
        Backoff::default()
    }

    /// Records a connection that ended after `uptime` and returns how long
    /// to wait before respawning: 1s doubling to 30s, restarted by a
    /// connection that lasted, and pinned at 30s for good once the child has
    /// exited instantly [`INSTANT_EXIT_STRIKES`] times in a row.
    pub fn record_exit(&mut self, uptime: Duration) -> Duration {
        if uptime >= HEALTHY_UPTIME {
            self.strikes = 0;
            self.next = BACKOFF_MIN;
        }
        if uptime < INSTANT_EXIT {
            self.strikes = self.strikes.saturating_add(1);
            if self.strikes >= INSTANT_EXIT_STRIKES {
                self.pinned = true;
            }
        }
        let delay = if self.pinned { BACKOFF_MAX } else { self.next };
        self.next = (delay * 2).min(BACKOFF_MAX);
        delay
    }
}

/// `/` separates a remote's name from the source key it prefixes, so it
/// cannot appear inside the name itself; control bytes can't either.
fn clean_name(name: &str) -> String {
    sanitize(name).replace('/', "-")
}

/// The disconnected decoration: visible on the row, and reversible, since
/// it is applied to a copy of the pristine snapshot on every read rather
/// than written into it.
fn mark_disconnected(mut s: SessionMeta) -> SessionMeta {
    s.title = format!("⌁ disconnected · {}", s.title);
    s.last_line = format!("⌁ disconnected · {}", s.last_line);
    s
}

/// Every string a hostile agent controls, laundered through the render
/// boundary's sanitizer before anything on the host stores or draws it.
fn sanitize_meta(mut s: SessionMeta) -> SessionMeta {
    s.id = sanitize(&s.id);
    s.model = sanitize(&s.model);
    s.title = sanitize(&s.title);
    s.last_tool = sanitize(&s.last_tool);
    s.last_line = sanitize(&s.last_line);
    s.last_event = s.last_event.map(|e| match e {
        LastEvent::ToolUse(name) => LastEvent::ToolUse(sanitize(&name)),
        other => other,
    });
    s
}

/// `Seg::new` is the sanitizing constructor, so rebuilding the line through
/// it is the laundering — the wire's own segments are never stored as-is.
fn sanitize_line(line: StyledLine) -> StyledLine {
    StyledLine(
        line.0
            .into_iter()
            .map(|s| Seg::new(s.sem, s.text))
            .collect(),
    )
}

/// The remote's own roster lines: link state first, then whatever the
/// hardening caps have had to do.
fn notices(name: &str, shared: &Shared) -> Vec<String> {
    let mut out = Vec::new();
    match &shared.link {
        Link::Connecting => out.push(format!("⌁ {name} — connecting…")),
        Link::Down => out.push(format!("⌁ {name} — disconnected, reconnecting…")),
        Link::ProtoMismatch { theirs } => out.push(format!(
            "⌁ {name} — agent speaks proto v{theirs}, host speaks v{PROTO_VERSION}: \
             upgrade hermon inside the container"
        )),
        Link::Up { .. } => {}
    }
    if let Some(err) = &shared.spawn_error {
        out.push(format!("⌁ {name} — {err}"));
    }
    if shared.truncated {
        out.push(format!(
            "⚠ {name} — snapshot truncated at {MAX_SNAP_SESSIONS} sessions"
        ));
    }
    if shared.oversized > 0 {
        out.push(format!(
            "⚠ {name} — dropped {} frame(s) over {} MiB",
            shared.oversized,
            MAX_FRAME_BYTES / (1024 * 1024)
        ));
    }
    if let Some(skew) = shared.skew {
        out.push(format!(
            "⚠ {name} — remote clock runs {skew:.0}s ahead; liveness may misread"
        ));
    }
    out
}

/// What the connection loop must do after a frame was applied.
#[derive(Debug, PartialEq)]
enum Applied {
    Nothing,
    /// A fresh `Hello`: every tail the host holds open needs re-opening on
    /// the new child.
    Reopen(Vec<(String, Replay)>),
    /// Stop reading this connection — the agent said `Bye`, or it speaks a
    /// protocol we don't.
    Stop,
}

/// Folds one decoded frame into the shared state. Pure over `shared` and
/// `now` (no I/O, no clock of its own) so the demux rules — the version
/// gate, the caps, the unopened-key discard — are unit-testable without a
/// child process.
///
/// Frames are only trusted once `Hello` has put the link `Up`: anything an
/// agent sends before introducing itself is dropped.
fn apply_frame(shared: &mut Shared, msg: AgentMsg, now: f64) -> Applied {
    match msg {
        AgentMsg::Hello {
            proto_version,
            hostname,
            ..
        } => {
            if proto_version != PROTO_VERSION {
                shared.link = Link::ProtoMismatch {
                    theirs: proto_version,
                };
                return Applied::Stop;
            }
            shared.link = Link::Up {
                hostname: sanitize(&hostname),
            };
            // A fresh Hello resets everything that described the *previous*
            // connection; the snapshot stays, so the deck doesn't blink.
            shared.oversized = 0;
            shared.truncated = false;
            shared.skew = None;
            shared.spawn_error = None;
            Applied::Reopen(
                shared
                    .tails
                    .iter()
                    .map(|(key, q)| (key.clone(), q.replay))
                    .collect(),
            )
        }
        _ if !matches!(shared.link, Link::Up { .. }) => Applied::Nothing,
        AgentMsg::Snap { mut sessions } => {
            shared.truncated = sessions.len() > MAX_SNAP_SESSIONS;
            sessions.truncate(MAX_SNAP_SESSIONS);
            shared.snapshot = sessions.into_iter().map(sanitize_meta).collect();
            if shared.skew.is_none()
                && let Some(newest) = shared.snapshot.iter().map(|s| s.last_ts).reduce(f64::max)
                && newest - now > SKEW_WARN_SECS
            {
                shared.skew = Some(newest - now);
            }
            Applied::Nothing
        }
        AgentMsg::Tail { key, lines } => {
            // A key the host never opened has no queue, and gets none: the
            // lookup allocates nothing, so inventing keys costs the agent
            // everything and the host nothing.
            if let Some(queue) = shared.tails.get_mut(&key) {
                for line in lines {
                    if queue.lines.len() >= MAX_TAIL_LINES {
                        queue.lines.pop_front();
                    }
                    queue.lines.push_back(sanitize_line(line));
                }
            }
            Applied::Nothing
        }
        AgentMsg::Bye { .. } => Applied::Stop,
    }
}

/// One frame off the wire, or why there wasn't one.
#[derive(Debug, PartialEq)]
enum Frame {
    Line,
    /// Longer than [`MAX_FRAME_BYTES`]: discarded, never buffered.
    Oversized,
    Eof,
}

/// Reads one newline-terminated frame into `buf` under a hard size cap —
/// the reason this isn't `read_line`, which would let the agent choose how
/// much host memory to allocate.
fn read_frame<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> Frame {
    buf.clear();
    let limit = MAX_FRAME_BYTES as u64 + 1;
    match (&mut *reader).take(limit).read_until(b'\n', buf) {
        Ok(0) | Err(_) => Frame::Eof,
        Ok(_) if buf.last() == Some(&b'\n') => Frame::Line,
        // No newline within the cap: an oversized frame, whose tail we drop
        // without storing it. Short of the cap it's a truncated last line,
        // i.e. the stream ended.
        Ok(n) if n as u64 == limit => {
            discard_to_newline(reader);
            Frame::Oversized
        }
        Ok(_) => Frame::Eof,
    }
}

/// Drops bytes through the next newline using the reader's own buffer, so
/// an oversized frame costs no allocation at all.
fn discard_to_newline<R: BufRead>(reader: &mut R) {
    loop {
        let (found, used) = match reader.fill_buf() {
            Ok([]) | Err(_) => return,
            Ok(chunk) => match chunk.iter().position(|b| *b == b'\n') {
                Some(i) => (true, i + 1),
                None => (false, chunk.len()),
            },
        };
        reader.consume(used);
        if found {
            return;
        }
    }
}

// --------------------------------------------------------- process wiring

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    // A panicking reader thread must not take the roster down with it: the
    // state behind the lock is plain data, and stale data beats a crash.
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Spawn, run, mourn, wait, repeat — until the [`RemoteSource`] is dropped.
fn supervise(
    mut transport: Command,
    shared: &Arc<Mutex<Shared>>,
    cmd_rx: &Receiver<HostCmd>,
    cmds: &Sender<HostCmd>,
    stop: &Arc<AtomicBool>,
) {
    let mut backoff = Backoff::new();
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        match transport
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => {
                lock(shared).spawn_error = None;
                run_connection(child, shared, cmd_rx, cmds, stop);
            }
            Err(e) => {
                lock(shared).spawn_error = Some(format!("cannot start transport: {e}"));
            }
        }
        {
            // A remote that has never connected keeps saying "connecting…";
            // a version mismatch keeps saying that, since respawning cannot
            // resolve it.
            let mut shared = lock(shared);
            if matches!(shared.link, Link::Up { .. }) {
                shared.link = Link::Down;
            }
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        sleep_until(backoff.record_exit(started.elapsed()), stop);
    }
}

/// Drives one child: a reader thread on its stdout, host commands onto its
/// stdin, until either end goes away.
fn run_connection(
    mut child: Child,
    shared: &Arc<Mutex<Shared>>,
    cmd_rx: &Receiver<HostCmd>,
    cmds: &Sender<HostCmd>,
    stop: &Arc<AtomicBool>,
) {
    let reading = Arc::new(AtomicBool::new(true));
    let reader = child.stdout.take().map(|stdout| {
        let shared = Arc::clone(shared);
        let cmds = cmds.clone();
        let reading = Arc::clone(&reading);
        thread::spawn(move || {
            read_frames(BufReader::new(stdout), &shared, &cmds);
            reading.store(false, Ordering::Relaxed);
        })
    });

    let mut stdin = child.stdin.take();
    while reading.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
        let cmd = match cmd_rx.recv_timeout(CMD_TICK) {
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let Some(writer) = stdin.as_mut() else { break };
        let line = encode_host_cmd(&cmd);
        if writeln!(writer, "{line}").is_err() || writer.flush().is_err() {
            break; // the child stopped listening; the reader will notice too
        }
    }

    // Ask the agent to leave, then make sure it did: dropping stdin is the
    // EOF it exits on, and the kill covers an agent that ignores both.
    if let Some(writer) = stdin.as_mut() {
        let _ = writeln!(writer, "{}", encode_host_cmd(&HostCmd::Shutdown));
        let _ = writer.flush();
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    if let Some(reader) = reader {
        let _ = reader.join();
    }
}

/// The reader thread: frames in, shared state out. Never fails — a bad
/// frame is skipped, an oversized one is counted, and EOF just ends the
/// connection so the supervisor can respawn.
fn read_frames<R: BufRead>(mut reader: R, shared: &Arc<Mutex<Shared>>, cmds: &Sender<HostCmd>) {
    let mut buf = Vec::new();
    loop {
        match read_frame(&mut reader, &mut buf) {
            Frame::Eof => return,
            Frame::Oversized => {
                lock(shared).oversized += 1;
                continue;
            }
            Frame::Line => {}
        }
        let line = String::from_utf8_lossy(&buf);
        let Decoded::Msg(msg) = decode_agent_msg(line.trim_end()) else {
            continue;
        };
        let applied = apply_frame(&mut lock(shared), msg, now_secs());
        match applied {
            Applied::Nothing => {}
            Applied::Stop => return,
            Applied::Reopen(tails) => {
                for (key, replay) in tails {
                    let _ = cmds.send(HostCmd::OpenTail { key, replay });
                }
            }
        }
    }
}

/// Waits out the backoff in slices, so dropping the [`RemoteSource`] does
/// not have to wait out a 30s sleep.
fn sleep_until(delay: Duration, stop: &Arc<AtomicBool>) {
    let deadline = Instant::now() + delay;
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        thread::sleep(CMD_TICK.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn meta(id: &str) -> SessionMeta {
        SessionMeta {
            id: id.to_string(),
            started_at: 1.0,
            ended: false,
            model: "claude-sonnet-5".into(),
            title: "t".into(),
            in_tok: 1,
            out_tok: 2,
            cost: Some(0.5),
            last_ts: 100.0,
            turn_done: false,
            tool_pending: false,
            force_live: false,
            last_tool: "Bash".into(),
            last_line: "l".into(),
            last_event: Some(LastEvent::ToolUse("Bash".into())),
        }
    }

    fn hello(version: u32) -> AgentMsg {
        AgentMsg::Hello {
            proto_version: version,
            hostname: "box".into(),
            sources: vec!["claude".into()],
        }
    }

    /// A `Shared` with the link already up, i.e. past the `Hello` gate.
    fn connected() -> Shared {
        let mut shared = Shared::default();
        assert!(matches!(
            apply_frame(&mut shared, hello(PROTO_VERSION), 0.0),
            Applied::Reopen(_)
        ));
        shared
    }

    // ------------------------------------------------------------ backoff

    #[test]
    fn backoff_doubles_from_one_second_to_thirty() {
        let mut b = Backoff::new();
        let dying = Duration::from_secs(2); // neither instant nor healthy
        let delays: Vec<u64> = (0..8).map(|_| b.record_exit(dying).as_secs()).collect();
        assert_eq!(delays, vec![1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[test]
    fn a_connection_that_lasted_restarts_the_backoff() {
        let mut b = Backoff::new();
        b.record_exit(Duration::from_secs(2));
        b.record_exit(Duration::from_secs(2));
        assert_eq!(b.record_exit(HEALTHY_UPTIME).as_secs(), 1);
    }

    #[test]
    fn repeated_instant_exits_pin_the_backoff_at_the_ceiling_for_good() {
        let mut b = Backoff::new();
        let instant = Duration::ZERO;
        assert_eq!(b.record_exit(instant).as_secs(), 1);
        assert_eq!(b.record_exit(instant).as_secs(), 2);
        assert_eq!(b.record_exit(instant).as_secs(), 30, "third strike pins it");
        // Permanently: even a healthy run afterwards doesn't unpin it.
        assert_eq!(b.record_exit(Duration::from_secs(600)).as_secs(), 30);
        assert_eq!(b.record_exit(instant).as_secs(), 30);
    }

    #[test]
    fn a_healthy_run_clears_the_instant_exit_strikes() {
        let mut b = Backoff::new();
        b.record_exit(Duration::ZERO);
        b.record_exit(Duration::ZERO);
        assert_eq!(
            b.record_exit(Duration::from_secs(600)).as_secs(),
            1,
            "a connection that lasted restarts the backoff"
        );
        // …and the strike count with it: two more instant exits are not yet
        // the third strike.
        b.record_exit(Duration::ZERO);
        b.record_exit(Duration::ZERO);
        assert!(!b.pinned);
        assert_eq!(b.record_exit(Duration::ZERO).as_secs(), 30);
        assert!(b.pinned);
    }

    // -------------------------------------------------------- frame reader

    fn frames(input: &str) -> (Vec<Frame>, Vec<String>) {
        let mut reader = Cursor::new(input.as_bytes().to_vec());
        let mut buf = Vec::new();
        let (mut kinds, mut lines) = (Vec::new(), Vec::new());
        loop {
            let frame = read_frame(&mut reader, &mut buf);
            let done = frame == Frame::Eof;
            if frame == Frame::Line {
                lines.push(String::from_utf8_lossy(&buf).trim_end().to_string());
            }
            kinds.push(frame);
            if done {
                return (kinds, lines);
            }
        }
    }

    #[test]
    fn reads_one_frame_per_line() {
        let (kinds, lines) = frames("a\nbb\n");
        assert_eq!(kinds, vec![Frame::Line, Frame::Line, Frame::Eof]);
        assert_eq!(lines, vec!["a", "bb"]);
    }

    #[test]
    fn a_frame_over_the_cap_is_dropped_and_the_next_one_still_reads() {
        let huge = "x".repeat(MAX_FRAME_BYTES * 2);
        let (kinds, lines) = frames(&format!("{huge}\nsmall\n"));
        assert_eq!(kinds, vec![Frame::Oversized, Frame::Line, Frame::Eof]);
        assert_eq!(lines, vec!["small"], "no part of the huge frame is kept");
    }

    #[test]
    fn a_frame_exactly_at_the_cap_still_reads() {
        let line = "x".repeat(MAX_FRAME_BYTES - 1);
        let (kinds, lines) = frames(&format!("{line}\n"));
        assert_eq!(kinds, vec![Frame::Line, Frame::Eof]);
        assert_eq!(lines[0].len(), MAX_FRAME_BYTES - 1);
    }

    #[test]
    fn a_stream_ending_mid_line_is_eof_not_a_frame() {
        let (kinds, lines) = frames("a\nunterminated");
        assert_eq!(kinds, vec![Frame::Line, Frame::Eof]);
        assert_eq!(lines, vec!["a"]);
    }

    // ---------------------------------------------------------- demux rules

    #[test]
    fn frames_before_hello_are_ignored() {
        let mut shared = Shared::default();
        apply_frame(
            &mut shared,
            AgentMsg::Snap {
                sessions: vec![meta("C:1")],
            },
            0.0,
        );
        assert!(shared.snapshot.is_empty());
        assert_eq!(shared.link, Link::Connecting);
    }

    #[test]
    fn a_version_mismatch_stops_the_connection_and_names_both_versions() {
        let mut shared = Shared::default();
        assert_eq!(
            apply_frame(&mut shared, hello(PROTO_VERSION + 41), 0.0),
            Applied::Stop
        );
        assert_eq!(
            shared.link,
            Link::ProtoMismatch {
                theirs: PROTO_VERSION + 41
            }
        );
        let note = notices("job1", &shared).remove(0);
        assert!(note.contains(&format!("v{}", PROTO_VERSION + 41)), "{note}");
        assert!(note.contains(&format!("v{PROTO_VERSION}")), "{note}");
        assert!(
            note.contains("upgrade hermon inside the container"),
            "{note}"
        );
    }

    #[test]
    fn a_fresh_hello_resets_the_previous_connections_complaints() {
        let mut shared = connected();
        shared.oversized = 3;
        shared.truncated = true;
        shared.skew = Some(999.0);
        shared.snapshot = vec![meta("C:1")];

        apply_frame(&mut shared, hello(PROTO_VERSION), 0.0);
        assert_eq!(notices("job1", &shared), Vec::<String>::new());
        assert_eq!(
            shared.snapshot.len(),
            1,
            "the snapshot survives so the deck doesn't blink"
        );
    }

    #[test]
    fn a_hello_reopens_every_tail_the_host_holds() {
        let mut shared = connected();
        shared.tails.insert(
            "C:1".into(),
            TailQueue {
                replay: Replay { bytes: 7, rows: 3 },
                lines: VecDeque::new(),
            },
        );
        assert_eq!(
            apply_frame(&mut shared, hello(PROTO_VERSION), 0.0),
            Applied::Reopen(vec![("C:1".into(), Replay { bytes: 7, rows: 3 })])
        );
    }

    #[test]
    fn an_oversized_snapshot_is_truncated_with_a_visible_marker() {
        let mut shared = connected();
        let sessions = (0..MAX_SNAP_SESSIONS + 1)
            .map(|i| meta(&format!("C:{i}")))
            .collect();
        apply_frame(&mut shared, AgentMsg::Snap { sessions }, 0.0);
        assert_eq!(shared.snapshot.len(), MAX_SNAP_SESSIONS);
        assert!(
            notices("job1", &shared)
                .iter()
                .any(|n| n.contains("snapshot truncated")),
            "{:?}",
            notices("job1", &shared)
        );
    }

    #[test]
    fn a_snapshot_within_the_cap_carries_no_marker() {
        let mut shared = connected();
        apply_frame(
            &mut shared,
            AgentMsg::Snap {
                sessions: vec![meta("C:1")],
            },
            0.0,
        );
        assert!(!shared.truncated);
        assert_eq!(notices("job1", &shared), Vec::<String>::new());
    }

    #[test]
    fn tail_frames_for_keys_the_host_never_opened_are_discarded() {
        let mut shared = connected();
        apply_frame(
            &mut shared,
            AgentMsg::Tail {
                key: "C:never-opened".into(),
                lines: vec![StyledLine(vec![Seg::new(Sem::Plain, "hi")])],
            },
            0.0,
        );
        assert!(shared.tails.is_empty(), "no demux state for invented keys");
    }

    #[test]
    fn tail_frames_for_an_open_key_queue_up_bounded() {
        let mut shared = connected();
        shared.tails.insert("C:1".into(), TailQueue::default());
        for i in 0..MAX_TAIL_LINES + 10 {
            apply_frame(
                &mut shared,
                AgentMsg::Tail {
                    key: "C:1".into(),
                    lines: vec![StyledLine(vec![Seg::new(Sem::Plain, format!("{i}"))])],
                },
                0.0,
            );
        }
        let queue = &shared.tails["C:1"].lines;
        assert_eq!(queue.len(), MAX_TAIL_LINES, "oldest lines are dropped");
        assert_eq!(
            queue.back().map(StyledLine::to_plain),
            Some(format!("{}", MAX_TAIL_LINES + 9))
        );
    }

    #[test]
    fn bye_ends_the_connection() {
        let mut shared = connected();
        assert_eq!(
            apply_frame(&mut shared, AgentMsg::Bye { reason: "x".into() }, 0.0),
            Applied::Stop
        );
    }

    // ------------------------------------------------------------ hostility

    #[test]
    fn every_remote_string_is_sanitized_on_the_host() {
        let mut shared = Shared::default();
        apply_frame(
            &mut shared,
            AgentMsg::Hello {
                proto_version: PROTO_VERSION,
                hostname: "box\x1b]0;pwned\x07".into(),
                sources: vec![],
            },
            0.0,
        );
        let Link::Up { hostname } = &shared.link else {
            panic!("expected Up, got {:?}", shared.link);
        };
        assert_eq!(hostname, "box\u{FFFD}]0;pwned\u{FFFD}");

        let mut hostile = meta("C:\x1b[31m1");
        hostile.title = "t\x1b]0;x\x07".into();
        hostile.model = "m\x00".into();
        hostile.last_line = "l\x1b".into();
        hostile.last_tool = "Bash\x07".into();
        hostile.last_event = Some(LastEvent::ToolUse("Bash\x1b".into()));
        shared.tails.insert("C:1".into(), TailQueue::default());
        apply_frame(
            &mut shared,
            AgentMsg::Snap {
                sessions: vec![hostile],
            },
            0.0,
        );
        apply_frame(
            &mut shared,
            AgentMsg::Tail {
                key: "C:1".into(),
                lines: vec![StyledLine(vec![Seg {
                    sem: Sem::Plain,
                    text: "tail\x1b]0;pwned\x07".into(),
                }])],
            },
            0.0,
        );

        let stored = &shared.snapshot[0];
        for text in [
            &stored.id,
            &stored.title,
            &stored.model,
            &stored.last_line,
            &stored.last_tool,
            &shared.tails["C:1"].lines[0].to_plain(),
        ] {
            assert!(
                text.bytes().all(|b| b >= 0x20 || b == 0x0A),
                "control byte survived in {text:?}"
            );
        }
        assert_eq!(
            stored.last_event,
            Some(LastEvent::ToolUse("Bash\u{FFFD}".into()))
        );
    }

    #[test]
    fn a_name_can_never_break_the_key_prefix() {
        assert_eq!(clean_name("job1"), "job1");
        assert_eq!(clean_name("a/b"), "a-b");
        assert_eq!(clean_name("job\x1b1"), "job\u{FFFD}1");
    }

    // ----------------------------------------------------------- host state

    #[test]
    fn a_remote_clock_running_ahead_warns_once() {
        let mut shared = connected();
        let mut future = meta("C:1");
        future.last_ts = 10_000.0;
        apply_frame(
            &mut shared,
            AgentMsg::Snap {
                sessions: vec![future.clone()],
            },
            10_000.0 - SKEW_WARN_SECS - 60.0,
        );
        let warned = notices("job1", &shared);
        assert!(
            warned.iter().any(|n| n.contains("clock runs")),
            "{warned:?}"
        );

        // A second snapshot doesn't add a second warning.
        apply_frame(
            &mut shared,
            AgentMsg::Snap {
                sessions: vec![future],
            },
            10_000.0 - SKEW_WARN_SECS - 60.0,
        );
        assert_eq!(notices("job1", &shared).len(), warned.len());
    }

    #[test]
    fn timestamps_within_the_skew_tolerance_are_quiet() {
        let mut shared = connected();
        apply_frame(
            &mut shared,
            AgentMsg::Snap {
                sessions: vec![meta("C:1")],
            },
            100.0 - SKEW_WARN_SECS,
        );
        assert_eq!(shared.skew, None);
    }

    #[test]
    fn a_remote_that_never_connected_says_so() {
        assert_eq!(
            notices("job1", &Shared::default()),
            vec!["⌁ job1 — connecting…".to_string()]
        );
    }

    #[test]
    fn a_dropped_connection_marks_the_rows_it_left_behind() {
        let mut shared = connected();
        apply_frame(
            &mut shared,
            AgentMsg::Snap {
                sessions: vec![meta("C:1")],
            },
            0.0,
        );
        shared.link = Link::Down;
        assert_eq!(
            notices("job1", &shared),
            vec!["⌁ job1 — disconnected, reconnecting…".to_string()]
        );

        let marked = mark_disconnected(shared.snapshot[0].clone());
        assert_eq!(marked.title, "⌁ disconnected · t");
        assert_eq!(marked.last_line, "⌁ disconnected · l");
        assert_eq!(marked.last_ts, 100.0, "timestamps are untouched");
    }

    #[test]
    fn oversized_frames_are_counted_for_the_deck() {
        let mut shared = connected();
        shared.oversized = 2;
        assert!(
            notices("job1", &shared)
                .iter()
                .any(|n| n.contains("dropped 2 frame(s) over 1 MiB")),
            "{:?}",
            notices("job1", &shared)
        );
    }
}
