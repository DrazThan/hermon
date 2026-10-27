mod common;
#[path = "common/new_stores.rs"]
mod new_stores;
use hermon::{
    remote::source::RemoteSource,
    roster::{Sources, build_roster},
    source::{LastEvent, Replay},
    view::{ViewState, apply},
};
use new_stores::{ID, Stores};
use std::{
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn empty_sources() -> Sources {
    Sources::new(
        "/nonexistent/c",
        "/nonexistent/h",
        "/nonexistent/o",
        "/nonexistent/x",
        "/nonexistent/g",
        "/nonexistent/gm",
    )
}
fn now() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1000.0
}

#[test]
fn built_agent_routes_full_ids_and_live_updates_without_cross_delivery() {
    let stores = Stores::new();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hermon"));
    cmd.args([
        "agent",
        "--interval",
        "0.1",
        "--claude-dir",
        "/nonexistent/c",
        "--hermes-db",
        "/nonexistent/h",
        "--opencode-db",
        "/nonexistent/o",
    ])
    .args(stores.args());
    let mut sources = empty_sources().with_remote(RemoteSource::new("job1", cmd));
    let deadline = Instant::now() + Duration::from_secs(10);
    let rows = loop {
        let rows = build_roster(&mut sources, now(), 3600.0, 180.0);
        if rows.iter().filter(|r| !r.id.is_empty()).count() == 3 {
            break rows;
        }
        assert!(Instant::now() < deadline, "missing snapshots: {rows:?}");
        thread::sleep(Duration::from_millis(25));
    };
    let mut tails = Vec::new();
    for (prefix, word) in [
        ("X", "unique-codex"),
        ("G", "unique-grok"),
        ("Gm", "unique-gemini"),
    ] {
        let row = rows
            .iter()
            .find(|r| r.key == format!("job1/{prefix}:123456"))
            .unwrap_or_else(|| panic!("no display alias job1/{prefix}:123456 in {rows:?}"));
        common::hermes_profiles::require_hr1(&row.id, b"job1", prefix, ID.as_bytes());
        assert_eq!(row.key, format!("job1/{prefix}:123456"));
        tails.push((
            word,
            sources
                .open_tailer(&row.key, &row.id, Replay::DEFAULT)
                .unwrap(),
            String::new(),
        ));
    }
    let mut view = ViewState::default();
    view.set_filter("key=job1/Gm:*").unwrap();
    assert_eq!(apply(&rows, &view).rows.len(), 1);
    // Wait for every OpenTail to reach the actual agent before appending.
    let deadline = Instant::now() + Duration::from_secs(5);
    while tails.iter().any(|(_, _, text)| text.is_empty()) {
        for (_, tail, text) in &mut tails {
            for l in tail.poll() {
                text.push_str(&l.to_plain());
            }
        }
        assert!(Instant::now() < deadline, "missing replay");
        thread::sleep(Duration::from_millis(25));
    }
    for (_, _, text) in &mut tails {
        text.clear();
    }
    stores.update();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        for (_, tail, text) in &mut tails {
            for l in tail.poll() {
                text.push_str(&l.to_plain());
            }
        }
        if tails.iter().all(|(word, _, text)| text.contains(*word)) {
            break;
        }
        assert!(Instant::now() < deadline, "missing appended text");
        thread::sleep(Duration::from_millis(25));
    }
    thread::sleep(Duration::from_millis(700));
    for (word, tail, text) in &mut tails {
        for l in tail.poll() {
            text.push_str(&l.to_plain());
        }
        assert_eq!(text.matches(*word).count(), 1, "{text}");
        assert_eq!(
            text.matches("unique-").count(),
            1,
            "cross-source delivery: {text}"
        );
    }
    let sessions = sources.remotes[0].sessions();
    let codex = sessions
        .iter()
        .find(|s| {
            common::hermes_profiles::decode_wire_id(&s.id)
                .is_ok_and(|w| w.outer_source == "X" && w.route.vendor_id == ID.as_bytes())
        })
        .unwrap_or_else(|| {
            panic!(
                "no canonical X wire session in {:?}",
                sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>()
            )
        });
    assert!(codex.turn_done);
    assert_eq!(codex.last_event, Some(LastEvent::AssistantText));
    for s in &sessions {
        assert_eq!(s.last_event, Some(LastEvent::AssistantText));
    }
}

#[test]
fn render_opens_each_new_source_using_ls_keys() {
    use std::{
        io::{BufRead, BufReader},
        process::Stdio,
        sync::mpsc,
    };
    let stores = Stores::new();
    stores.update();
    for (prefix, expected) in [
        ("X", "unique-codex"),
        ("G", "unique-grok"),
        ("Gm", "unique-gemini"),
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_hermon"))
            .args([
                "render",
                &format!("{prefix}:123456"),
                "--claude-dir",
                "/nonexistent/c",
                "--hermes-db",
                "/nonexistent/h",
                "--opencode-db",
                "/nonexistent/o",
            ])
            .args(stores.args())
            .env("NO_COLOR", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = String::new();
        while let Ok(line) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            output.push_str(&line);
            if output.contains(expected) {
                break;
            }
        }
        let _ = child.kill();
        child.wait().unwrap();
        reader.join().unwrap();
        assert!(output.contains(expected), "{prefix}: {output}");
        assert!(!output.contains('\u{1b}'));
    }
}
