//! Independent worker-facing block-kind guidance acceptance.
#![cfg(unix)]
use hermon_flow::{
    control::{RunRecord, save},
    hub::{Event, Run},
    intake::hash,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    process::Command,
    thread,
    time::{Duration, Instant},
};

#[test]
fn generated_task_enumerates_every_legal_block_kind_for_the_worker() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let repo = root.join("fixture-repository");
    fs::create_dir(&repo).unwrap();
    let worker = root.join("fixture-worker");
    fs::write(&worker, r#"#!/bin/sh
set -eu
/bin/cp "$1" "${1%/*}/worker-task.md"
printf '%s' '{"schema":1,"status":"done","summary":"fixture read its task","artifacts":[],"block":null,"verdict":null,"questions":[]}' > "${1%/*}/result.json"
"#).unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o755)).unwrap();
    let project = root.join("project.json");
    fs::write(&project,json!({
        "repo":repo,"worktree_root":root.join("unused-worktrees"),"branch_prefix":"fixture","base":"main","start":"work",
        "env":{"PATH":"/usr/bin:/bin"},"bots":{"fixture-worker":{"Command":{
            "start":[worker,"{task}"],
            "resume":null,"task_stdin":false,"protocol":"ResultFile","family":"fixture"}}},
        "commands":{},"paths":{},"tracker":{"endpoint":"http://127.0.0.1:1/graphql","token_env":"HERMON_BLOCK_CONTRACT_FIXTURE_TOKEN","owner_ids":["fixture-owner"],"owner_mention":"@fixture-owner"},
        "code_host":{"repo":"fixture/project","remote":"origin"}
    }).to_string()).unwrap();
    let data = root.join("data");
    let run_dir = data.join("runs/FIXTURE-BLOCK-CONTRACT");
    fs::create_dir_all(&run_dir).unwrap();
    let workflow =
        "version: 1\nname: fixture-block-contract\nsteps:\n  work:\n    bot: fixture-worker\n";
    let mut run = Run::new("FIXTURE-BLOCK-CONTRACT", "work");
    run.events.push(Event {
        kind: "intake".into(),
        step: "work".into(),
        reason: "synthetic queued fixture".into(),
    });
    let record = RunRecord {
        schema: 1,
        ticket: "FIXTURE-BLOCK".into(),
        workflow_name: "fixture-block-contract".into(),
        workflow_hash: hash(workflow),
        workflow_source: workflow.into(),
        template_hashes: BTreeMap::new(),
        templates: BTreeMap::new(),
        project,
        worktree: repo,
        base: "main".into(),
        base_commit: "fixture-base".into(),
        branch: "fixture/block-contract".into(),
        paused: false,
        cancelled: false,
        run,
        notices: vec![],
        started: true,
    };
    save(&run_dir, &record).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hermon"))
        .args(["hub", "--once", "--no-notify", "--data-dir"])
        .arg(&data)
        .env(
            "HERMON_BLOCK_CONTRACT_FIXTURE_TOKEN",
            "synthetic-fixture-token",
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture host: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let attempt = run_dir.join("steps/work/1");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let finished = fs::read(attempt.join("process.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|value| value["finished"] == true);
        if finished {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "finite fixture worker did not settle"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let generated = fs::read_to_string(attempt.join("task.md")).unwrap();
    let delivered = fs::read_to_string(attempt.join("worker-task.md")).unwrap();
    assert_eq!(
        delivered, generated,
        "worker must receive the exact generated contract"
    );
    for kind in [
        "owner_question",
        "test_defect",
        "capability",
        "transient",
        "scope_change",
        "unknown",
    ] {
        assert!(
            delivered.contains(kind),
            "generated worker contract omits legal block kind {kind}"
        );
    }
}
