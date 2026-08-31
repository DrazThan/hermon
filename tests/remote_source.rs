//! `RemoteSource` acceptance tests (#90): the host half against a real
//! child process, no docker anywhere — the `cmd:`-style transport #91
//! formalizes, spawning `hermon agent` locally against a fixture store.
//!
//! The first test is the protocol's end-to-end proof: sessions appear on
//! the roster under a `job1/` prefix, a tail opens through the demux,
//! killing the agent shows the disconnected state, and the supervisor
//! reconnects on its own. The rest stand a hostile agent up in front of the
//! host — a bad `Hello`, a frame far over the cap — using a canned frame
//! file instead of a real agent.

use std::fs;
use std::path::Path;
use std::process::Command as Proc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::DateTime;
use tempfile::TempDir;

use hermon::remote::proto::{AgentMsg, PROTO_VERSION, encode_agent_msg};
use hermon::remote::source::RemoteSource;
use hermon::roster::{RosterRow, Sources, build_roster};
use hermon::source::{Replay, SessionMeta};

const IDLE_TIMEOUT: f64 = 180.0;
const FRESH_WINDOW: f64 = 300.0;

// ------------------------------------------------------------- e2e helpers

/// A Claude transcript root with one session fresh enough (mtime, real wall
/// clock — the agent binary has no injectable `now`) for the agent to
/// surface it, same trick `tests/agent.rs` uses.
fn claude_dir() -> TempDir {
    let dir = TempDir::new().expect("create temp dir");
    let project = dir.path().join("-Users-taloz-code-hermon");
    fs::create_dir_all(&project).expect("create project dir");
    let now = now_secs();
    let body = format!(
        "{}{}",
        user_line(now - 30.0),
        assistant_line(now - 5.0, "hello from the container")
    );
    fs::write(
        project.join("11111111-2222-3333-4444-555555555555.jsonl"),
        body,
    )
    .expect("write transcript");
    dir
}

fn user_line(ts: f64) -> String {
    format!(
        "{{\"type\":\"user\",\"timestamp\":\"{}\",\
         \"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n",
        iso(ts)
    )
}

fn assistant_line(ts: f64, text: &str) -> String {
    format!(
        concat!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","#,
            r#""model":"claude-sonnet-5","content":[{{"type":"text","text":"{}"}}],"#,
            r#""usage":{{"input_tokens":5,"output_tokens":5}}}},"costUSD":0.01}}"#,
            "\n",
        ),
        iso(ts),
        text
    )
}

fn iso(ts: f64) -> String {
    DateTime::from_timestamp(ts as i64, 0)
        .expect("valid timestamp")
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_secs_f64()
}

/// A transport that runs `hermon agent` and leaves its pid where the test
/// can find it, so the test can kill the agent out from under the host the
/// way a dying container would. The wrapper shell exits when the agent
/// does, which is exactly the child-exit the supervisor reconnects from.
///
/// The `exec 3<&0` / `<&3` dance is load-bearing: a non-interactive shell
/// gives a background job `/dev/null` for stdin unless it is redirected
/// explicitly, and an agent whose stdin is at EOF exits immediately.
fn agent_transport(claude: &Path, workdir: &Path) -> Proc {
    let script = workdir.join("transport.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             exec 3<&0\n\
             '{bin}' agent --interval 1 --claude-dir '{claude}' \
             --hermes-db /nonexistent/state.db --opencode-db /nonexistent/opencode.db \
             --hermes-log /nonexistent/agent.log <&3 &\n\
             echo $! > '{pid}'\n\
             wait $!\n",
            bin = env!("CARGO_BIN_EXE_hermon"),
            claude = claude.display(),
            pid = workdir.join("agent.pid").display(),
        ),
    )
    .expect("write transport script");

    let mut cmd = Proc::new("sh");
    cmd.arg(script);
    cmd
}

/// A transport that replays a canned frame stream and then holds the pipe
/// open on stdin — a hostile agent that says exactly what the test wants it
/// to say.
fn canned_transport(workdir: &Path, frames: &str) -> Proc {
    let path = workdir.join("frames.jsonl");
    fs::write(&path, frames).expect("write canned frames");
    let mut cmd = Proc::new("sh");
    cmd.arg("-c")
        .arg(format!("cat '{}'; cat > /dev/null", path.display()));
    cmd
}

fn kill_agent(workdir: &Path) {
    let pid = fs::read_to_string(workdir.join("agent.pid")).expect("agent pid file");
    let status = Proc::new("kill")
        .arg(pid.trim())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill {}: {status:?}", pid.trim());
}

/// Polls `f` until it yields a value or the deadline passes — every remote
/// assertion is about state that arrives on another thread.
fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn local_sources() -> Sources {
    Sources::new(
        "/nonexistent/claude",
        "/nonexistent/state.db",
        "/nonexistent/opencode.db",
    )
}

fn roster(sources: &mut Sources) -> Vec<RosterRow> {
    build_roster(sources, now_secs(), FRESH_WINDOW, IDLE_TIMEOUT)
}

// ------------------------------------------------------------------- e2e

#[test]
fn remote_sessions_tail_disconnect_and_reconnect_through_a_real_agent() {
    let claude = claude_dir();
    let workdir = TempDir::new().expect("create temp dir");
    let remote = RemoteSource::new("job1", agent_transport(claude.path(), workdir.path()));
    let mut sources = local_sources().with_remote(remote);

    // The agent's first Snap reaches the roster as an ordinary row, keyed
    // by where it lives: job1/C:555555.
    let row = wait_for(Duration::from_secs(20), || {
        roster(&mut sources)
            .into_iter()
            .find(|r| r.key.starts_with("job1/C:"))
    })
    .expect("the remote's claude session reaches the roster");
    assert_eq!(row.key, "job1/C:555555");
    assert_eq!(row.id, "C:11111111-2222-3333-4444-555555555555");
    assert_eq!(row.model, "claude-sonnet-5");
    assert!(
        !row.title.contains("⌁ disconnected"),
        "a connected remote's rows carry no marker: {:?}",
        row.title
    );
    assert!(
        !roster(&mut sources).iter().any(|r| r.key == "job1"),
        "a connected remote has nothing to say on its own line"
    );

    // A tail opens end to end: OpenTail out, Tail frames back, demuxed onto
    // this key's queue.
    let mut tailer = sources
        .open_tailer(&row.key, &row.id, Replay::DEFAULT)
        .expect("a remote tailer for a session the snapshot named");
    let lines = wait_for(Duration::from_secs(15), || {
        let lines = tailer.poll();
        (!lines.is_empty()).then_some(lines)
    })
    .expect("replayed transcript lines arrive over the wire");
    let text: String = lines.iter().map(|l| l.to_plain()).collect();
    assert!(text.contains("hello from the container"), "{text:?}");
    assert!(
        text.bytes().all(|b| b >= 0x20 || b == 0x0A),
        "remote tail text is sanitized on the host: {text:?}"
    );

    // Kill the agent: the sessions stay on the deck for their fresh window,
    // marked disconnected rather than vanishing.
    kill_agent(workdir.path());
    let marked = wait_for(Duration::from_secs(10), || {
        roster(&mut sources)
            .into_iter()
            .find(|r| r.key.starts_with("job1/C:") && r.title.contains("⌁ disconnected"))
    })
    .expect("the killed agent's sessions are marked disconnected");
    assert_eq!(marked.id, row.id, "same session, still on the deck");
    assert!(
        roster(&mut sources)
            .iter()
            .any(|r| r.key == "job1" && r.title.contains("disconnected")),
        "the remote itself says the link is down"
    );
    assert!(
        wait_for(Duration::from_secs(5), || {
            tailer
                .poll()
                .iter()
                .any(|l| l.to_plain().contains("⌁ disconnected"))
                .then_some(())
        })
        .is_some(),
        "the open pane says so too, rather than going silently blank"
    );

    // The supervisor respawns the transport on its own, and the fresh Hello
    // re-opens the tail the host still holds.
    let back = wait_for(Duration::from_secs(30), || {
        roster(&mut sources)
            .into_iter()
            .find(|r| r.key.starts_with("job1/C:") && !r.title.contains("⌁ disconnected"))
    })
    .expect("the remote reconnects without being asked");
    assert_eq!(back.id, row.id);
    assert!(
        wait_for(Duration::from_secs(15), || {
            let lines = tailer.poll();
            lines
                .iter()
                .any(|l| l.to_plain().contains("hello from the container"))
                .then_some(())
        })
        .is_some(),
        "the pane refills itself after the reconnect"
    );
}

// -------------------------------------------------------- hostile agents

#[test]
fn a_version_mismatch_shows_the_upgrade_message_on_the_roster() {
    let workdir = TempDir::new().expect("create temp dir");
    let frames = format!(
        "{}\n",
        encode_agent_msg(&AgentMsg::Hello {
            proto_version: PROTO_VERSION + 41,
            hostname: "box".into(),
            sources: vec!["claude".into()],
        })
    );
    let remote = RemoteSource::new("job1", canned_transport(workdir.path(), &frames));
    let mut sources = local_sources().with_remote(remote);

    let note = wait_for(Duration::from_secs(10), || {
        roster(&mut sources)
            .into_iter()
            .find(|r| r.key == "job1" && r.title.contains("proto"))
            .map(|r| r.title)
    })
    .expect("the version mismatch reaches the roster");
    assert!(note.contains(&format!("v{}", PROTO_VERSION + 41)), "{note}");
    assert!(note.contains(&format!("v{PROTO_VERSION}")), "{note}");
    assert!(
        note.contains("upgrade hermon inside the container"),
        "{note}"
    );
}

#[test]
fn a_remote_that_never_connects_shows_one_connecting_line() {
    let remote = RemoteSource::new("job1", Proc::new("/nonexistent/transport"));
    let mut sources = local_sources().with_remote(remote);

    let rows = wait_for(Duration::from_secs(5), || {
        let rows = roster(&mut sources);
        rows.iter()
            .any(|r| r.title.contains("cannot start transport"))
            .then_some(rows)
    })
    .expect("a transport that cannot start says why");
    assert!(rows.iter().all(|r| r.key == "job1"), "{rows:?}");
    assert_eq!(
        rows[0].title, "⌁ job1 — connecting…",
        "a remote that has never connected is still connecting, not disconnected"
    );
}

#[test]
fn a_missing_binary_in_the_container_names_the_fix_instead_of_going_silent() {
    // The child a `docker exec`/`ssh` transport would spawn when `hermon`
    // isn't on the remote's PATH: it writes the shell's own "not found" and
    // exits immediately, over and over as the supervisor keeps respawning.
    let mut cmd = Proc::new("sh");
    cmd.arg("-c").arg("echo 'hermon: not found' >&2; exit 127");
    let remote = RemoteSource::new("job1", cmd);
    let mut sources = local_sources().with_remote(remote);

    let rows = wait_for(Duration::from_secs(10), || {
        let rows = roster(&mut sources);
        rows.iter()
            .any(|r| r.title.contains("not found in container"))
            .then_some(rows)
    })
    .expect("a missing binary is diagnosed rather than surfacing as silence");
    assert!(rows.iter().all(|r| r.key == "job1"), "{rows:?}");
    let note = rows
        .iter()
        .find(|r| r.title.contains("not found in container"))
        .expect("the missing-binary row");
    assert_eq!(
        note.title,
        "⌁ job1 — 'hermon' not found in container: copy the binary in or add to the image",
        "names the fix, not just the symptom"
    );

    // The supervisor keeps respawning a binary that will never appear; that
    // must not spin the host or crash it. A few more polls over the
    // reconnect loop should still show the same one line, not a pile of
    // duplicates.
    thread::sleep(Duration::from_secs(2));
    let rows = roster(&mut sources);
    assert_eq!(
        rows.iter()
            .filter(|r| r.key == "job1" && r.title.contains("not found in container"))
            .count(),
        1,
        "one line, not spammed per respawn: {:?}",
        rows.iter().map(|r| &r.title).collect::<Vec<_>>()
    );
}

#[test]
fn an_oversized_frame_is_dropped_and_the_stream_resyncs() {
    let workdir = TempDir::new().expect("create temp dir");
    let mut hostile = session("C:11111111-2222-3333-4444-555555555555");
    hostile.title = "title\x1b]0;pwned\x07".into();
    let frames = format!(
        "{}\n{}\n{}\n",
        encode_agent_msg(&AgentMsg::Hello {
            proto_version: PROTO_VERSION,
            hostname: "box".into(),
            sources: vec!["claude".into()],
        }),
        "x".repeat(2 * 1024 * 1024),
        encode_agent_msg(&AgentMsg::Snap {
            sessions: vec![hostile],
        }),
    );
    let remote = RemoteSource::new("job1", canned_transport(workdir.path(), &frames));
    let mut sources = local_sources().with_remote(remote);

    let rows = wait_for(Duration::from_secs(15), || {
        let rows = roster(&mut sources);
        rows.iter()
            .any(|r| r.key.starts_with("job1/C:"))
            .then_some(rows)
    })
    .expect("the frame after the oversized one still decodes");
    assert!(
        rows.iter()
            .any(|r| r.key == "job1" && r.title.contains("dropped 1 frame(s) over 1 MiB")),
        "the drop is counted where a human sees it: {:?}",
        rows.iter().map(|r| &r.title).collect::<Vec<_>>()
    );

    let session = rows
        .iter()
        .find(|r| r.key.starts_with("job1/C:"))
        .expect("the session row");
    assert_eq!(session.title, "title\u{FFFD}]0;pwned\u{FFFD}");
}

/// A minimal live session the roster will keep — the agent's own key form
/// (`C:<id>`) in `id`, timestamped now so `classify` reads it as live.
fn session(id: &str) -> SessionMeta {
    let now = now_secs();
    SessionMeta {
        id: id.to_string(),
        started_at: now - 60.0,
        ended: false,
        model: "claude-sonnet-5".into(),
        title: "t".into(),
        in_tok: 1,
        out_tok: 2,
        cost: Some(0.25),
        last_ts: now,
        turn_done: false,
        tool_pending: false,
        force_live: false,
        last_tool: "Bash".into(),
        last_line: "▶ Bash ls".into(),
        last_event: None,
    }
}
