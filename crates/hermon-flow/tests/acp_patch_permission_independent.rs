//! Independent narrow artifact-patch permission acceptance.
#![cfg(unix)]
use hermon_flow::{
    agent::{BotConfig, ProcessAgent},
    hub::{Agent, Poll, StartRequest},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
const WORKER: &str = env!("CARGO_BIN_EXE_hermon-flow-worker");
const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/hermes-acp-patch-permission.py"
);
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    work: PathBuf,
    attempt: PathBuf,
    log: PathBuf,
    env: BTreeMap<String, String>,
    config: BotConfig,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs");
        let work = temp.path().join("work");
        let bin = temp.path().join("bin");
        let attempt = root.join("FIXTURE-PATCH/steps/work/1");
        for p in [&work, &bin, &attempt] {
            fs::create_dir_all(p).unwrap();
        }
        fs::create_dir(attempt.join("artifacts")).unwrap();
        for p in [
            attempt.join("plan.md"),
            attempt.join("artifacts/notes.md"),
            attempt.join(".hidden.md"),
            attempt.join("artifact.lock"),
            work.join("outside.md"),
        ] {
            fs::write(p, "old\n").unwrap();
        }
        let sibling = root.join("FIXTURE-PATCH/steps/work/2");
        fs::create_dir(&sibling).unwrap();
        fs::write(sibling.join("plan.md"), "old\n").unwrap();
        symlink(&work, attempt.join("outside-parent")).unwrap();
        symlink(work.join("outside.md"), attempt.join("linked.md")).unwrap();
        let python = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
        let db = temp.path().join("fixture.db");
        assert!(Command::new(&python).args(["-c","import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('CREATE TABLE sessions(id TEXT PRIMARY KEY,parent_session_id TEXT,end_reason TEXT)'); c.execute('CREATE TABLE session_model_usage(session_id TEXT,model TEXT,billing_provider TEXT,task TEXT,api_call_count INTEGER)'); c.commit()"]).arg(&db).status().unwrap().success());
        let script = bin.join("hermes");
        fs::write(
            &script,
            format!("#!/bin/sh\nexec '{}' '{}' \"$@\"\n", python, FAKE),
        )
        .unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            attempt.join("task.md"),
            format!(
                "Complete only the assigned fixture step.\nFIXTURE_RESULT_PATH={}\n",
                serde_json::to_string(&attempt.join("result.json")).unwrap()
            ),
        )
        .unwrap();
        let config=serde_json::from_value(json!({"HermesAcp":{"profile":"fixture-profile","model_id":"openai-codex:gpt-fixture","version":"0.22.4","state_db":db}})).unwrap();
        let log = temp.path().join("fixture.jsonl");
        let env = BTreeMap::from([
            ("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())),
            ("ACP_SCENARIO".into(), "permission".into()),
            ("ACP_STATE_DB".into(), db.display().to_string()),
            ("ACP_LOG".into(), log.display().to_string()),
            ("ACP_MODEL_ID".into(), "openai-codex:gpt-fixture".into()),
            (
                "ACP_DISPLAY_PATH".into(),
                attempt.join("plan.md").display().to_string(),
            ),
        ]);
        Self {
            _temp: temp,
            root,
            work,
            attempt,
            log,
            env,
            config,
        }
    }
    fn decide(mut self, args: Value, wrong_session: bool) -> Value {
        self.env
            .insert("ACP_PATCH_ARGUMENTS".into(), args.to_string());
        if wrong_session {
            self.env.insert(
                "ACP_PERMISSION_SESSION".into(),
                "fixture-wrong-session".into(),
            );
        }
        let mut agent = ProcessAgent::new(
            &self.root,
            &self.work,
            WORKER,
            BTreeMap::from([("fixture".into(), self.config)]),
            Duration::from_secs(8),
        )
        .unwrap()
        .with_env(self.env);
        let handle = agent
            .start(&StartRequest {
                run_id: "FIXTURE-PATCH".into(),
                step: "work".into(),
                bot: "fixture".into(),
                number: 1,
                freezes: BTreeMap::new(),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            match agent.poll(&handle).unwrap() {
                Poll::Running => {
                    assert!(
                        Instant::now() < deadline,
                        "finite patch fixture did not settle"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Poll::Finished {
                    process_success,
                    completion,
                    ..
                } => {
                    assert!(
                        process_success && completion,
                        "permission response must keep the fixture transport usable"
                    );
                    break;
                }
                other => panic!("unexpected fixture outcome {other:?}"),
            }
        }
        let events: Vec<Value> = fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        events.into_iter().find(|event|event["event"]=="permission_response").expect("client must answer the public permission request")["response"]["result"]["outcome"].clone()
    }
}
fn patch(paths: &[&Path]) -> String {
    let mut text = "*** Begin Patch\n".to_owned();
    for path in paths {
        text.push_str(&format!(
            "*** Update File: {}\n@@\n-old\n+new\n",
            path.display()
        ));
    }
    text.push_str("*** End Patch");
    text
}
fn args(patch: String) -> Value {
    json!({"mode":"patch","patch":patch})
}
fn denied(outcome: Value) {
    assert!(
        outcome["outcome"] == "cancelled" || outcome["optionId"] == "reject_once",
        "unsafe raw patch was allowed: {outcome}"
    );
}

#[test]
fn canonical_single_and_multiple_owned_updates_receive_allow_once() {
    for multiple in [false, true] {
        let f = Fixture::new();
        let plan = f.attempt.join("plan.md");
        let notes = f.attempt.join("artifacts/notes.md");
        let paths = if multiple {
            vec![plan.as_path(), notes.as_path()]
        } else {
            vec![plan.as_path()]
        };
        let outcome = f.decide(args(patch(&paths)), false);
        assert_eq!(outcome["outcome"], "selected");
        assert_eq!(
            outcome["optionId"], "allow_once",
            "never choose offered allow_always"
        );
    }
}
#[test]
fn non_update_operations_malformed_headers_and_unknown_modes_are_denied() {
    for case in [
        "add",
        "delete",
        "move",
        "unknown",
        "no-end",
        "no-hunk",
        "spaced-header",
        "trailing-path-space",
        "unknown-mode",
    ] {
        let f = Fixture::new();
        let plan = f.attempt.join("plan.md");
        let canonical = patch(&[&plan]);
        let value = match case {
            "add" => args(format!(
                "*** Begin Patch\n*** Add File: {}\n+new\n*** End Patch",
                f.attempt.join("new.md").display()
            )),
            "delete" => args(format!(
                "*** Begin Patch\n*** Delete File: {}\n*** End Patch",
                plan.display()
            )),
            "move" => args(canonical.replace(
                "\n@@",
                &format!(
                    "\n*** Move to: {}\n@@",
                    f.attempt.join("moved.md").display()
                ),
            )),
            "unknown" => args(canonical.replace("*** Update File:", "*** Unknown Operation:")),
            "no-end" => args(canonical.replace("*** End Patch", "")),
            "no-hunk" => args(canonical.replace("@@\n-old\n+new\n", "")),
            "spaced-header" => args(canonical.replace("*** Update File:", "***   Update File:")),
            "trailing-path-space" => args(
                canonical.replace(&plan.display().to_string(), &format!("{} ", plan.display())),
            ),
            _ => json!({"mode":"replace","patch":canonical}),
        };
        denied(f.decide(value, false));
    }
}
#[test]
fn path_boundaries_and_mixed_targets_follow_raw_arguments_not_cosmetic_diff() {
    for case in [
        "relative",
        "traversal",
        "outside",
        "other-attempt",
        "missing",
        "parent-symlink",
        "file-symlink",
        "mixed",
    ] {
        let f = Fixture::new();
        let safe = f.attempt.join("plan.md");
        let target = match case {
            "relative" => PathBuf::from("plan.md"),
            "traversal" => f.attempt.join("artifacts/../plan.md"),
            "outside" => f.work.join("outside.md"),
            "other-attempt" => f.root.join("FIXTURE-PATCH/steps/work/2/plan.md"),
            "missing" => f.attempt.join("missing.md"),
            "parent-symlink" => f.attempt.join("outside-parent/outside.md"),
            "file-symlink" => f.attempt.join("linked.md"),
            _ => f.work.join("outside.md"),
        };
        let text = if case == "mixed" {
            patch(&[&safe, &target])
        } else {
            patch(&[&target])
        };
        // The fixture always displays the safe plan.md diff/title, even here.
        denied(f.decide(args(text), false));
    }
}
#[test]
fn protected_hidden_lock_targets_and_wrong_session_are_denied() {
    for name in [
        "task.md",
        "process.json",
        "launch.json",
        "worker.log",
        ".hidden.md",
        "artifact.lock",
    ] {
        let f = Fixture::new();
        let target = f.attempt.join(name);
        denied(f.decide(args(patch(&[&target])), false));
    }
    let f = Fixture::new();
    let safe = f.attempt.join("plan.md");
    denied(f.decide(args(patch(&[&safe])), true));
}
