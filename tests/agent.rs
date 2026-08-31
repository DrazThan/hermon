//! `hermon agent` acceptance tests (#89): the stdio pipe test the spec
//! calls for — `Hello` then `Snap` frames, `OpenTail` producing `Tail`
//! frames, a prompt exit on stdin EOF, and no non-frame bytes on stdout —
//! run against a real fixture store, no docker. This is the shape #90's
//! e2e (`cmd:`-style transport) reuses.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command as Proc, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono::DateTime;
use tempfile::TempDir;

use hermon::remote::proto::{AgentMsg, Decoded, HostCmd, decode_agent_msg, encode_host_cmd};
use hermon::source::Replay;

/// A Claude transcript root with one session fresh enough (mtime, real
/// wall clock — the binary has no injectable `now`) for `ClaudeSource` to
/// surface it, same trick `tests/roster.rs`'s binary tests use.
fn claude_dir() -> TempDir {
    let dir = TempDir::new().expect("create temp dir");
    let project = dir.path().join("-Users-taloz-code-hermon");
    fs::create_dir_all(&project).expect("create project dir");
    let now = wall_clock_now();
    let body = format!("{}{}", user_line(now - 30.0), assistant_line(now - 5.0));
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

fn assistant_line(ts: f64) -> String {
    format!(
        concat!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","#,
            r#""model":"claude-sonnet-5","content":[{{"type":"text","text":"hello"}}],"#,
            r#""usage":{{"input_tokens":5,"output_tokens":5}}}},"costUSD":0.01}}"#,
            "\n",
        ),
        iso(ts)
    )
}

fn iso(ts: f64) -> String {
    DateTime::from_timestamp(ts as i64, 0)
        .expect("valid timestamp")
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn wall_clock_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_secs_f64()
}

/// Spawns `hermon agent --interval 1` against a claude fixture and
/// nonexistent hermes/opencode stores (both degrade to "no sessions", same
/// as every other source-reading command), piping both stdin and stdout.
fn spawn_agent(claude: &TempDir) -> std::process::Child {
    Proc::new(env!("CARGO_BIN_EXE_hermon"))
        .arg("agent")
        .arg("--interval")
        .arg("1")
        .arg("--claude-dir")
        .arg(claude.path())
        .arg("--hermes-db")
        .arg("/nonexistent/state.db")
        .arg("--opencode-db")
        .arg("/nonexistent/opencode.db")
        .arg("--hermes-log")
        .arg("/nonexistent/agent.log")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hermon agent")
}

/// Reads stdout lines on a background thread into a channel, so the test
/// can interleave writing to stdin without either direction deadlocking.
fn spawn_line_reader(stdout: std::process::ChildStdout) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line.trim_end().to_string()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

#[test]
fn agent_streams_hello_snap_tail_and_exits_promptly_on_stdin_eof() {
    let claude = claude_dir();
    let mut child = spawn_agent(&claude);
    let mut stdin = child.stdin.take().expect("piped stdin");
    let lines = spawn_line_reader(child.stdout.take().expect("piped stdout"));

    // Hello arrives immediately, naming all three sources.
    let hello = lines
        .recv_timeout(Duration::from_secs(2))
        .expect("Hello frame");
    match decode_agent_msg(&hello) {
        Decoded::Msg(AgentMsg::Hello { sources, .. }) => {
            assert_eq!(sources, vec!["claude", "hermes", "opencode"]);
        }
        other => panic!("expected Hello, got {other:?}"),
    }

    // A Snap follows within the 1s interval and names the fixture session.
    let key = (0..5)
        .find_map(|_| {
            let line = lines.recv_timeout(Duration::from_secs(2)).ok()?;
            match decode_agent_msg(&line) {
                Decoded::Msg(AgentMsg::Snap { sessions }) => {
                    sessions.iter().find(|s| s.id.starts_with("C:")).cloned()
                }
                _ => None,
            }
        })
        .expect("Snap named the fixture claude session")
        .id;

    // Ask the agent to tail it; the replayed lines come back as a Tail frame.
    let open = encode_host_cmd(&HostCmd::OpenTail {
        key: key.clone(),
        replay: Replay::DEFAULT,
    });
    writeln!(stdin, "{open}").expect("write OpenTail");
    stdin.flush().expect("flush OpenTail");

    let saw_tail = (0..10).any(|_| {
        let Ok(line) = lines.recv_timeout(Duration::from_secs(2)) else {
            return false;
        };
        matches!(
            decode_agent_msg(&line),
            Decoded::Msg(AgentMsg::Tail { key: ref k, ref lines }) if *k == key && !lines.is_empty()
        )
    });
    assert!(saw_tail, "expected a Tail frame after OpenTail");

    // Closing stdin (EOF) exits the process within one interval.
    drop(stdin);
    let (exit_tx, exit_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = exit_tx.send(child.wait());
    });
    let status = exit_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("hermon agent exits promptly on stdin EOF")
        .expect("wait on child");
    assert!(status.success(), "{status:?}");
}

#[test]
fn agent_stdout_carries_only_frames() {
    let claude = claude_dir();
    let mut child = spawn_agent(&claude);
    let stdin = child.stdin.take().expect("piped stdin");
    let mut reader = BufReader::new(child.stdout.take().expect("piped stdout"));

    let mut seen = Vec::new();
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => seen.push(line.trim_end().to_string()),
        }
    }
    drop(stdin);
    let _ = child.wait();

    assert!(!seen.is_empty(), "expected at least Hello + Snap");
    for line in &seen {
        assert!(
            matches!(decode_agent_msg(line), Decoded::Msg(_)),
            "non-frame bytes on stdout: {line:?}"
        );
    }
}
