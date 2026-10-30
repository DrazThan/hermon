//! Independent regression for bidirectional ACP JSON-RPC ID collisions.
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
    "/tests/fixtures/hermes-acp-id-collision.py"
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

#[test]
fn acp_permission_request_id_cannot_be_mistaken_for_prompt_response() {
    let fixture = Fixture::new("permission");
    let mut agent = fixture.agent(Duration::from_secs(5));
    let handle = agent.start(&fixture.request("target", 1)).unwrap();
    match finish(&mut agent, &handle) {
        Poll::Finished {
            process_success,
            completion,
            result,
        } => {
            assert!(
                process_success && completion,
                "handled incoming permission request must not terminate the unrelated outbound prompt"
            );
            assert_eq!(
                serde_json::from_str::<Value>(&result.unwrap()).unwrap()["status"],
                "done"
            );
        }
        other => panic!(
            "a denied server request sharing the prompt ID must not become an uncertain prompt outcome: {other:?}"
        ),
    }
    let events = fixture.events();
    let prompt = events
        .iter()
        .find(|e| e["event"] == "received" && e["message"]["method"] == "session/prompt")
        .unwrap();
    let denied = events
        .iter()
        .find(|e| e["event"] == "permission_response")
        .expect("client must still deny the incoming permission request");
    assert_eq!(
        denied["response"]["id"], prompt["message"]["id"],
        "fixture must actually collide RPC IDs"
    );
    assert!(
        denied["response"]["result"]["outcome"]["outcome"] == "cancelled"
            || denied["response"]["result"]["outcome"]["optionId"] == "reject_once"
    );
    assert_eq!(fixture.state(&handle)["hermes"]["settled"], true);
}
