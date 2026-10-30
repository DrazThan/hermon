//! Independent scope checks for ACP write_file permissions.
#![allow(dead_code)]
#![cfg(unix)]
use hermon_flow::{
    agent::{BotConfig, ProcessAgent},
    hub::{Agent, Poll, StartRequest},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

const WORKER: &str = env!("CARGO_BIN_EXE_hermon-flow-worker");
const ACP_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/hermes-acp-artifact-permission.py"
);
const MODEL: &str = "openai-codex:gpt-6.1-sol-900k";
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    workdir: PathBuf,
    logs: PathBuf,
    env: BTreeMap<String, String>,
    bots: BTreeMap<String, BotConfig>,
}
impl Fixture {
    fn new(scenario: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("run records");
        let workdir = temp.path().join("work tree");
        let logs = temp.path().join("ACP logs");
        let bin = temp.path().join("bin");
        for directory in [&root, &workdir, &logs, &bin] {
            fs::create_dir_all(directory).unwrap();
        }
        let db = temp.path().join("state.db");
        let python = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
        assert!(Command::new(&python).args(["-c","import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('CREATE TABLE sessions(id TEXT PRIMARY KEY,parent_session_id TEXT,end_reason TEXT)'); c.execute('CREATE TABLE session_model_usage(session_id TEXT,model TEXT,billing_provider TEXT,task TEXT,api_call_count INTEGER)'); c.commit()",db.to_str().unwrap()]).status().unwrap().success());
        let script = bin.join("hermes");
        fs::write(
            &script,
            format!("#!/bin/sh\nexec '{}' '{}' \"$@\"\n", python, ACP_FIXTURE),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        // Runtime deserialization intentionally makes pre-implementation RED a
        // missing supported behavior, rather than a future-enum compile error.
        let config: BotConfig=serde_json::from_value(json!({"HermesAcp":{"profile":"sdlc-implementer","model_id":MODEL,"version":"0.22.4","state_db":db}})).expect("verified public ACP must be an explicit supported opt-in binding");
        let env = BTreeMap::from([
            (
                "PATH".into(),
                format!("{}:/opt/homebrew/bin:/usr/bin:/bin", bin.display()),
            ),
            ("HOME".into(), temp.path().to_str().unwrap().to_owned()),
            ("ACP_STATE_DB".into(), db.to_str().unwrap().to_owned()),
            ("ACP_FIXTURE_LOG".into(), logs.to_str().unwrap().to_owned()),
            ("ACP_MODEL_ID".into(), MODEL.into()),
            ("ACP_SCENARIO".into(), scenario.into()),
        ]);
        Self {
            _temp: temp,
            root,
            workdir,
            logs,
            env,
            bots: BTreeMap::from([
                ("target".into(), config.clone()),
                ("control".into(), config),
            ]),
        }
    }
    fn agent(&self, timeout: Duration) -> ProcessAgent {
        ProcessAgent::new(
            &self.root,
            &self.workdir,
            WORKER,
            self.bots.clone(),
            timeout,
        )
        .unwrap()
        .with_env(self.env.clone())
    }
    fn request(&self, bot: &str, number: u32) -> StartRequest {
        let dir = self
            .root
            .join("RUN-125/steps")
            .join(bot)
            .join(number.to_string());
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("task.md"),
            format!(
                "Complete only the assigned fixture step.\nFIXTURE_RESULT_PATH={}\n",
                serde_json::to_string(&dir.join("result.json")).unwrap()
            ),
        )
        .unwrap();
        StartRequest {
            run_id: "RUN-125".into(),
            step: bot.into(),
            bot: bot.into(),
            number,
            freezes: BTreeMap::new(),
        }
    }
    fn events(&self) -> Vec<Value> {
        fs::read_dir(&self.logs)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|p| p.path().extension().is_some_and(|e| e == "jsonl"))
            .flat_map(|p| {
                fs::read_to_string(p.path())
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect::<Vec<Value>>()
            })
            .collect()
    }
    fn state(&self, handle: &str) -> Value {
        serde_json::from_slice(&fs::read(Path::new(handle).join("process.json")).unwrap()).unwrap()
    }
    fn heartbeat(&self, session: &str) -> Option<String> {
        let pid = session.rsplit('-').next().unwrap();
        fs::read_to_string(self.logs.join(format!("{pid}.heartbeat"))).ok()
    }
}
fn wait<T>(mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(v) = check() {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "ACP fixture observation timed out"
        );
        thread::sleep(Duration::from_millis(20));
    }
}
fn finish(agent: &mut ProcessAgent, handle: &str) -> Poll {
    wait(
        || match agent.poll(handle).expect("observe supervised ACP attempt") {
            Poll::Running => None,
            poll => Some(poll),
        },
    )
}
fn started(fixture: &Fixture, agent: &ProcessAgent, handle: &str) -> String {
    wait(|| {
        agent.attempt(handle).ok()?.session_id.filter(|id| {
            fixture
                .events()
                .iter()
                .any(|e| e["event"] == "prompt" && e["currentSessionId"] == *id)
        })
    })
}
fn failed_terminal(poll: Poll) {
    match poll {
        Poll::Finished {
            process_success,
            completion,
            ..
        } => assert!(
            !process_success || !completion,
            "cancelled/deadline attempt cannot become success"
        ),
        other => panic!("expected settled failed terminal observation, got {other:?}"),
    }
}

fn attempt_dir(fixture: &Fixture) -> PathBuf {
    fixture.root.join("RUN-125/steps/target/1")
}
fn permission(fixture: &mut Fixture, path: &Path) -> Value {
    fixture
        .env
        .insert("ACP_PERMISSION_PATH".into(), path.to_str().unwrap().into());
    let mut agent = fixture.agent(Duration::from_secs(5));
    let handle = agent.start(&fixture.request("target", 1)).unwrap();
    let _ = finish(&mut agent, &handle);
    fixture.events().into_iter().find(|e|e["event"]=="permission_response").expect("client must explicitly answer artifact permission")["response"]["result"]["outcome"].clone()
}
fn denied(outcome: &Value, path: &Path) {
    assert!(
        outcome["outcome"] == "cancelled" || outcome["optionId"] == "reject_once",
        "path {} was allowed: {outcome}",
        path.display()
    );
    assert_ne!(outcome["optionId"], "allow_always");
}

#[test]
fn acp_allows_only_once_for_owned_result_and_artifact_file_writes() {
    for name in ["result.json", "artifacts/plan.md"] {
        let mut fixture = Fixture::new("permission");
        fixture.request("target", 1);
        let path = attempt_dir(&fixture).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let outcome = permission(&mut fixture, &path);
        assert_eq!(
            outcome["outcome"], "selected",
            "owned output must receive an explicit one-time decision"
        );
        assert_eq!(
            outcome["optionId"],
            "allow_once",
            "never choose the offered allow_always option for owned output {}: {outcome}",
            path.display()
        );
    }
}

#[test]
fn acp_rejects_outside_relative_cross_attempt_and_symlink_write_targets() {
    use std::os::unix::fs::symlink;
    for case in [
        "outside",
        "relative",
        "other-attempt",
        "parent-link",
        "file-link",
    ] {
        let mut fixture = Fixture::new("permission");
        fixture.request("target", 1);
        let dir = attempt_dir(&fixture);
        let path = match case {
            "outside" => fixture.workdir.join("outside.md"),
            "relative" => PathBuf::from("result.json"),
            "other-attempt" => {
                let other = fixture.root.join("RUN-125/steps/target/2");
                fs::create_dir_all(&other).unwrap();
                other.join("result.json")
            }
            "parent-link" => {
                symlink(&fixture.workdir, dir.join("escaped-parent")).unwrap();
                dir.join("escaped-parent/artifact.md")
            }
            "file-link" => {
                let outside = fixture.workdir.join("outside.md");
                fs::write(&outside, "unrelated temporary file").unwrap();
                symlink(&outside, dir.join("artifact-link.md")).unwrap();
                dir.join("artifact-link.md")
            }
            _ => unreachable!(),
        };
        denied(&permission(&mut fixture, &path), &path);
    }
}

#[test]
fn acp_rejects_hub_metadata_control_locks_and_hidden_output_basenames() {
    for name in [
        "launch.json",
        "process.json",
        "task.md",
        "cancel",
        "log",
        "stderr",
        "worker.log",
        "owner-answer.json",
        "launch.lock",
        "answer.lock",
        "run.lock",
        "result.before-launch.json",
        ".hidden-artifact",
    ] {
        let mut fixture = Fixture::new("permission");
        fixture.request("target", 1);
        let path = attempt_dir(&fixture).join(name);
        denied(&permission(&mut fixture, &path), &path);
    }
}
