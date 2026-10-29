//! Remote bot context and qualified wire routing.
//!
//! These tests drive the current `hermon` binary and the host `RemoteSource`
//! linked into this crate. They do not run a saved historical v1 binary.
//! A v1 peer is a canned frame. Rejection of that peer is what a v2 host
//! must do; a peer version other than the host's is the same gate, checked
//! separately so the mismatch path stays covered on either side of the bump.
//!
//! New wire fields are `serde_json::Value` only. No future Rust variants.

mod common;

use std::fs;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use hermon::remote::source::{Link, MAX_FRAME_BYTES, RemoteSource};
use hermon::roster::{Sources, build_roster, roster_lines};
use hermon::source::Replay;

use common::hermes_profiles::*;

const VENDOR: &str = "11111111-2222-3333-4444-555555555555";

fn session(id: &str, title: &str) -> Value {
    json!({
        "id": id,
        "started_at": FIXTURE_TS,
        "ended": true,
        "model": "fixture-model",
        "title": title,
        "in_tok": 1,
        "out_tok": 1,
        "cost": 0.01,
        "last_ts": FIXTURE_TS,
        "turn_done": true,
        "tool_pending": false,
        "force_live": false,
        "last_tool": "-",
        "last_line": title,
        "last_event": null
    })
}

fn hello(version: u32) -> Value {
    json!({
        "type": "Hello",
        "proto_version": version,
        "hostname": "box",
        "sources": ["hermes"]
    })
}

fn snap(sessions: Vec<Value>) -> Value {
    json!({"type": "Snap", "sessions": sessions})
}

fn frames(values: &[Value]) -> String {
    let mut out = String::new();
    for value in values {
        out.push_str(&value.to_string());
        out.push('\n');
    }
    out
}

fn canned(dir: &std::path::Path, body: &str) -> Command {
    let path = dir.join("frames.jsonl");
    fs::write(&path, body).expect("frames");
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(format!("cat '{}'; cat > /dev/null", path.display()));
    cmd
}

fn local_sources() -> Sources {
    Sources::new(
        "/nonexistent/claude",
        "/nonexistent/state.db",
        "/nonexistent/opencode.db",
        "/nonexistent/codex",
        "/nonexistent/grok",
        "/nonexistent/gemini",
    )
}

fn wait_row(
    sources: &mut Sources,
    pred: impl Fn(&hermon::roster::RosterRow) -> bool,
) -> Option<hermon::roster::RosterRow> {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if let Some(row) = build_roster(sources, FIXTURE_TS, 1_000_000_000_000.0, 180.0)
            .into_iter()
            .find(&pred)
        {
            return Some(row);
        }
        thread::sleep(Duration::from_millis(25));
    }
    None
}

fn plain_roster(sources: &mut Sources) -> String {
    let rows = build_roster(sources, FIXTURE_TS, 1_000_000_000_000.0, 180.0);
    roster_lines(&rows, &[], FIXTURE_TS)
        .iter()
        .map(hermon::render::StyledLine::to_plain)
        .collect::<Vec<_>>()
        .join("\n")
}

fn agent_lines(home: &Home, extra: &[String]) -> (Vec<String>, String) {
    let agent = Agent::spawn(home.path(), extra);
    let lines = agent.recv_until(Duration::from_secs(8), |line| {
        line.contains("\"type\":\"Snap\"") || line.contains("\"type\": \"Snap\"")
    });
    let stderr = String::new();
    (lines, stderr)
}

#[test]
fn agent_hello_is_protocol_2() {
    let home = seed_main_and_worker();
    let (lines, _) = agent_lines(&home, &[]);
    let hello = lines.iter().find_map(|line| {
        let value: Value = serde_json::from_str(line).ok()?;
        (value.get("type")?.as_str() == Some("Hello")).then_some(value)
    });
    let hello = hello.unwrap_or_else(|| panic!("no Hello in {lines:?}"));
    assert_eq!(
        hello.get("proto_version").and_then(|v| v.as_u64()),
        Some(2),
        "agent must speak protocol 2: {hello}"
    );
}

#[test]
fn agent_emits_bot_context_before_a_full_snap() {
    let home = empty_home();
    seed_finished(&home.main_db(), "sess-main", "main-fixture", "main-body");
    seed_finished(
        &home.profile_db("worker"),
        "sess-worker",
        "worker-fixture",
        "worker-body",
    );
    create_db(&home.profile_db("emptybot"));
    let corrupt = home.profile_db("corrupt");
    fs::create_dir_all(corrupt.parent().unwrap()).unwrap();
    fs::write(&corrupt, b"not sqlite").unwrap();
    let (lines, _) = agent_lines(&home, &[]);
    let mut saw_context = false;
    let mut snap = None;
    for line in &lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(|t| t.as_str()) {
            Some("BotContext") if snap.is_none() => {
                saw_context = true;
                assert!(value.get("owners").and_then(|v| v.as_object()).is_some());
                assert!(value.get("bots").and_then(|v| v.as_array()).is_some());
                assert!(value.get("warnings").and_then(|v| v.as_array()).is_some());
                let text = value.to_string();
                assert!(text.contains("emptybot"), "empty store missing: {text}");
                assert!(
                    text.contains("corrupt") || text.contains("unavailable"),
                    "{text}"
                );
            }
            Some("Snap") => {
                snap = Some(value);
                break;
            }
            _ => {}
        }
    }
    let snap = snap.unwrap_or_else(|| panic!("no Snap in {lines:?}"));
    assert!(saw_context, "BotContext must precede Snap: {lines:?}");
    let sessions = snap["sessions"].as_array().expect("sessions");
    let ids: Vec<&str> = sessions
        .iter()
        .filter_map(|s| s.get("id").and_then(|id| id.as_str()))
        .collect();
    assert!(
        ids.iter().any(|id| id.contains("sess-main")
            || decode_wire_id(id).is_ok_and(|w| w.route.vendor_id == b"sess-main")),
        "full snap must include the main session despite no --bot filter: {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id.contains("sess-worker")
            || decode_wire_id(id).is_ok_and(|w| w.route.vendor_id == b"sess-worker")),
        "full snap must include the worker session: {ids:?}"
    );
}

#[test]
fn agent_wire_ids_are_canonical_and_not_double_encoded() {
    let home = seed_main_and_worker();
    let (lines, _) = agent_lines(&home, &[]);
    let snap = lines.iter().find_map(|line| {
        let value: Value = serde_json::from_str(line).ok()?;
        (value.get("type")?.as_str() == Some("Snap")).then_some(value)
    });
    let snap = snap.unwrap_or_else(|| panic!("no Snap in {lines:?}"));
    let mut stores = Vec::new();
    for session in snap["sessions"].as_array().unwrap_or(&Vec::new()) {
        let id = session["id"].as_str().unwrap_or("");
        let wire = decode_wire_id(id).unwrap_or_else(|e| panic!("wire id {id:?} rejected: {e}"));
        assert_eq!(wire.outer_source, wire.route.source);
        assert!(wire.route.origin.is_empty());
        assert!(!wire.route.store.is_empty());
        assert!(
            id.contains("hr1:"),
            "wire id must contain the printable hr1 envelope, not a second hex encoding: {id}"
        );
        stores.push(wire.route.store);
    }
    assert!(
        stores.len() >= 2 && stores[0] != stores[1],
        "two profiles must advertise distinct store components: {stores:?}"
    );
}

#[test]
fn agent_bot_flag_does_not_filter_the_snapshot() {
    let home = seed_main_and_worker();
    let (lines, _) = agent_lines(&home, &["--bot".into(), "worker".into()]);
    let snap = lines.iter().find_map(|line| {
        let value: Value = serde_json::from_str(line).ok()?;
        (value.get("type")?.as_str() == Some("Snap")).then_some(value)
    });
    let snap =
        snap.unwrap_or_else(|| panic!("--bot must not prevent a full snap; lines={lines:?}"));
    let text = snap.to_string();
    assert!(
        text.contains("main-fixture") && text.contains("worker-fixture"),
        "agent must ignore --bot and emit every session: {text}"
    );
}

#[test]
fn host_rejects_a_v1_peer() {
    let dir = scratch_temp();
    let body = frames(&[
        hello(1),
        snap(vec![session(&format!("C:{VENDOR}"), "v1-session")]),
    ]);
    let remote = RemoteSource::new("job1", canned(dir.path(), &body));
    let mut sources = local_sources().with_remote(remote);
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut saw = false;
    while Instant::now() < deadline {
        if matches!(sources.remotes[0].link(), Link::ProtoMismatch { theirs: 1 }) {
            saw = true;
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        saw,
        "v1 peer must be rejected: {:?}",
        sources.remotes[0].link()
    );
    let notes = sources.remotes[0].notices().join("\n");
    assert!(
        notes.contains("proto") && notes.contains("upgrade"),
        "mismatch must stay actionable: {notes}"
    );
    assert!(
        wait_row(&mut sources, |row| row.title.contains("v1-session")).is_none(),
        "a rejected v1 snap must not become roster sessions"
    );
}

#[test]
fn host_accepts_v2_legacy_snap_as_unknown_owner() {
    let dir = scratch_temp();
    let body = frames(&[
        hello(2),
        snap(vec![session(&format!("C:{VENDOR}"), "legacy-snap")]),
    ]);
    let remote = RemoteSource::new("job1", canned(dir.path(), &body));
    let mut sources = local_sources().with_remote(remote);
    let row = wait_row(&mut sources, |row| row.title.contains("legacy-snap"))
        .expect("v2 hello plus ownerless snap must be accepted");
    let route = require_hr1(&row.id, b"job1", "C", VENDOR.as_bytes());
    assert_eq!(
        route.store, b"legacy:C",
        "legacy snapshot uses the source-specific unknown-store namespace"
    );
    let text = plain_roster(&mut sources);
    let row_line = text
        .lines()
        .find(|line| line.contains("job1/C:"))
        .unwrap_or("");
    assert!(
        row_line.contains("owner unknown") || row_line.contains('—'),
        "missing context is unknown, not default: {row_line}\n{text}"
    );
    assert!(!row_line.to_lowercase().contains("default"));
}

#[test]
fn unrelated_protocol_mismatch_is_still_rejected() {
    let dir = scratch_temp();
    let body = frames(&[hello(99)]);
    let remote = RemoteSource::new("job1", canned(dir.path(), &body));
    let sources = local_sources().with_remote(remote);
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if matches!(
            sources.remotes[0].link(),
            Link::ProtoMismatch { theirs: 99 }
        ) {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "peer version 99 must still mismatch: {:?}",
        sources.remotes[0].link()
    );
}

#[test]
fn colliding_profiles_have_independent_wire_tails() {
    let home = empty_home();
    seed_finished(&home.main_db(), "same-vendor", "main-same", "sentinel-main");
    seed_finished(
        &home.profile_db("worker"),
        "same-vendor",
        "worker-same",
        "sentinel-worker",
    );
    let mut agent = Agent::spawn(home.path(), &[]);
    let lines = agent.recv_until(Duration::from_secs(8), |line| line.contains("worker-same"));
    let mut wires = Vec::new();
    for line in &lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("Snap") {
            continue;
        }
        for session in value["sessions"].as_array().unwrap_or(&Vec::new()) {
            let id = session["id"].as_str().unwrap_or("");
            if decode_wire_id(id).is_ok_and(|w| w.route.vendor_id == b"same-vendor") {
                wires.push((
                    id.to_string(),
                    session["title"].as_str().unwrap_or("").to_string(),
                ));
            }
        }
    }
    let mut unique = Vec::new();
    for (id, title) in wires {
        if !unique
            .iter()
            .any(|(seen, _): &(String, String)| seen == &id)
        {
            unique.push((id, title));
        }
    }
    assert!(
        unique.len() >= 2,
        "two stores with one vendor id must advertise two wire ids: {lines:?}"
    );
    use hermon::remote::proto::HostCmd;
    use hermon::remote::proto::encode_host_cmd;
    for (id, _) in &unique {
        agent.write_line(&encode_host_cmd(&HostCmd::OpenTail {
            key: id.clone(),
            replay: Replay::DEFAULT,
        }));
    }
    let mut hits: Vec<(String, String)> = Vec::new();
    let wanted = unique.clone();
    let _ = agent.recv_until(Duration::from_secs(8), |line| {
        if let Some(hit) = tail_frame(line) {
            hits.push(hit);
        }
        wanted.iter().all(|(id, title)| {
            let sentinel = sentinel_for(title);
            hits.iter()
                .any(|(key, text)| key == id && text.contains(sentinel))
        })
    });
    for (id, title) in &wanted {
        let sentinel = sentinel_for(title);
        let other = if sentinel == "sentinel-worker" {
            "sentinel-main"
        } else {
            "sentinel-worker"
        };
        let hit = hits
            .iter()
            .find(|(key, text)| key == id && text.contains(sentinel));
        let hit = hit.unwrap_or_else(|| panic!("no Tail echoing {id} with {sentinel}: {hits:?}"));
        assert!(
            !hit.1.contains(other),
            "{id} cross-delivered {other}: {}",
            hit.1
        );
    }
}

fn sentinel_for(title: &str) -> &'static str {
    if title.contains("worker") {
        "sentinel-worker"
    } else {
        "sentinel-main"
    }
}

fn tail_frame(line: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(|kind| kind.as_str()) != Some("Tail") {
        return None;
    }
    let key = value.get("key")?.as_str()?.to_string();
    Some((key, tail_text(value.get("lines"))))
}

fn tail_text(value: Option<&Value>) -> String {
    let mut out = String::new();
    if let Some(value) = value {
        walk_tail_text(value, &mut out);
    }
    out
}

fn walk_tail_text(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(text)) = map.get("text") {
                out.push_str(text);
            }
            for (key, child) in map {
                if key != "text" {
                    walk_tail_text(child, out);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                walk_tail_text(item, out);
            }
        }
        _ => {}
    }
}

#[test]
fn unadvertised_route_does_not_read_an_arbitrary_path() {
    let home = seed_main_and_worker();
    let mut agent = Agent::spawn(home.path(), &[]);
    let mut admitted = None;
    let _ = agent.recv_until(Duration::from_secs(8), |line| {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        if value.get("type").and_then(|kind| kind.as_str()) != Some("Snap") {
            return false;
        }
        for session in value["sessions"].as_array().unwrap_or(&Vec::new()) {
            if session.get("title").and_then(|t| t.as_str()) == Some("main-fixture")
                && let Some(id) = session.get("id").and_then(|id| id.as_str())
            {
                admitted = Some(id.to_string());
                return true;
            }
        }
        false
    });
    let admitted = admitted.expect("admitted main session was not advertised");
    use hermon::remote::proto::{HostCmd, encode_host_cmd};
    agent.write_line(&encode_host_cmd(&HostCmd::OpenTail {
        key: admitted.clone(),
        replay: Replay::DEFAULT,
    }));
    let mut saw_admitted = false;
    let _ = agent.recv_until(Duration::from_secs(6), |line| {
        if let Some((key, text)) = tail_frame(line)
            && key == admitted
            && text.contains("main-body")
        {
            saw_admitted = true;
            return true;
        }
        false
    });
    assert!(
        saw_admitted,
        "admitted route must tail before the unregistered-store negative"
    );

    let secret_db = home.path().join("unregistered").join("state.db");
    seed_finished(
        &secret_db,
        "canary-session",
        "canary-title",
        "canary-secret-sentinel",
    );
    let before = file_bytes(&secret_db);
    let forged = wire_session_id("H", &store_bytes(&secret_db), b"canary-session");
    agent.write_line(&encode_host_cmd(&HostCmd::OpenTail {
        key: forged.clone(),
        replay: Replay::DEFAULT,
    }));
    let mut leaked = false;
    let _ = agent.recv_until(Duration::from_secs(2), |line| {
        if let Some((key, text)) = tail_frame(line)
            && (text.contains("canary-secret-sentinel")
                || (key == forged && text.contains("canary")))
        {
            leaked = true;
            return true;
        }
        false
    });
    assert!(
        !leaked,
        "canonical unregistered hr1 route must not tail the canary session"
    );
    assert_eq!(
        file_bytes(&secret_db),
        before,
        "unregistered store bytes changed"
    );
}

#[test]
fn oversized_context_drops_attribution_but_keeps_the_snap() {
    let dir = scratch_temp();
    let marker = "HOSTILE-OWNER-MARKER";
    let mut huge = json!({
        "type": "BotContext",
        "owners": {"overflow": {"slug": marker, "label": marker}},
        "bots": [],
        "warnings": []
    })
    .to_string();
    huge.push_str(&marker.repeat(8));
    huge.push_str(&"x".repeat(MAX_FRAME_BYTES));
    let mut body = frames(&[hello(2)]);
    body.push_str(&huge);
    body.push('\n');
    body.push_str(&frames(&[snap(vec![session(
        &format!("H:{VENDOR}"),
        "kept-session",
    )])]));
    let remote = RemoteSource::new("job1", canned(dir.path(), &body));
    let mut sources = local_sources().with_remote(remote);
    let row = wait_row(&mut sources, |row| row.title.contains("kept-session"))
        .expect("an oversized BotContext must not drop the following valid Snap");
    assert!(!row.title.contains(marker));
    let notes = sources.remotes[0].notices().join("\n");
    assert!(
        notes.contains("frame") || notes.contains("ownership") || notes.contains("omitted"),
        "host must surface a bounded cap warning: {notes}"
    );
    assert!(notes.len() < 500, "warning is not bounded: {notes}");
    assert!(!notes.contains(marker), "oversized payload leaked: {notes}");
    let text = plain_roster(&mut sources);
    assert!(
        !text.contains(marker),
        "stale oversized owner must not be reused: {text}"
    );
}

#[test]
fn reconnect_does_not_reuse_the_previous_context() {
    let dir = scratch_temp();
    let first = dir.path().join("first.jsonl");
    let second = dir.path().join("second.jsonl");
    let count = dir.path().join("count");
    let wire = format!("C:{VENDOR}");
    let mut context = json!({
        "type": "BotContext",
        "owners": {wire.clone(): {"slug": "evil", "label": "HOSTILE-LABEL-ONE\u{1b}[31m"}},
        "bots": [{"slug": "evil", "label": "HOSTILE-LABEL-ONE", "store": "x", "available": true}],
        "warnings": []
    })
    .to_string();
    context.push('\n');
    fs::write(
        &first,
        format!("{}{}{}", hello(2), "\n", {
            let mut s = context;
            s.push_str(&snap(vec![session(&wire, "kept-session")]).to_string());
            s.push('\n');
            s
        }),
    )
    .unwrap();
    fs::write(
        &second,
        frames(&[hello(2), snap(vec![session(&wire, "kept-session")])]),
    )
    .unwrap();
    let script = dir.path().join("transport.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nn=0\n[ -f '{count}' ] && n=$(cat '{count}')\necho $((n+1)) > '{count}'\nif [ \"$n\" = 0 ]; then\n  cat '{first}'\n  sleep 2\nelse\n  cat '{second}'\n  cat > /dev/null\nfi\n",
            count = count.display(),
            first = first.display(),
            second = second.display(),
        ),
    )
    .unwrap();
    let mut cmd = Command::new("sh");
    cmd.arg(&script);
    let remote = RemoteSource::new("job1", cmd);
    let mut sources = local_sources().with_remote(remote);
    let row = wait_row(&mut sources, |row| row.title.contains("kept-session"))
        .expect("first v2 snap must be accepted");
    let _ = row;
    thread::sleep(Duration::from_secs(4));
    let text = plain_roster(&mut sources);
    let notes = sources.remotes[0].notices().join("\n");
    assert!(
        !text.contains("HOSTILE-LABEL-ONE") && !notes.contains("HOSTILE-LABEL-ONE"),
        "reconnect must not reuse the previous context\n{text}\n{notes}"
    );
    assert!(
        !text.contains('\u{1b}') && !notes.contains('\u{1b}'),
        "hostile label escaped into a surface"
    );
}
