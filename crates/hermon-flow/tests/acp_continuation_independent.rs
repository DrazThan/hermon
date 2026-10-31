//! Independent bounded same-session continuation contract for missing results.
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
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};
const WORKER: &str = env!("CARGO_BIN_EXE_hermon-flow-worker");
const SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/hermes-acp-continuation.py"
);
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    workdir: PathBuf,
    log: PathBuf,
    env: BTreeMap<String, String>,
    config: BotConfig,
}
impl Fixture {
    fn new(interim: u32) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs");
        let workdir = temp.path().join("work");
        let bin = temp.path().join("bin");
        for p in [&root, &workdir, &bin] {
            fs::create_dir(p).unwrap();
        }
        let python = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
        let db = temp.path().join("fixture.db");
        assert!(Command::new(&python).args(["-c","import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('CREATE TABLE sessions(id TEXT PRIMARY KEY,parent_session_id TEXT,end_reason TEXT)'); c.execute('CREATE TABLE session_model_usage(session_id TEXT,model TEXT,billing_provider TEXT,task TEXT,api_call_count INTEGER)'); c.commit()"]).arg(&db).status().unwrap().success());
        let wrapper = bin.join("hermes");
        fs::write(
            &wrapper,
            format!("#!/bin/sh\nexec '{}' '{}' \"$@\"\n", python, SCRIPT),
        )
        .unwrap();
        fs::set_permissions(wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let log = temp.path().join("fixture.jsonl");
        let config=serde_json::from_value(json!({"HermesAcp":{"profile":"fixture-profile","model_id":"openai-codex:gpt-fixture","version":"0.22.4","state_db":db}})).unwrap();
        let env = BTreeMap::from([
            ("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())),
            ("ACP_SCENARIO".into(), "done".into()),
            ("ACP_STATE_DB".into(), db.display().to_string()),
            ("ACP_LOG".into(), log.display().to_string()),
            ("ACP_MODEL_ID".into(), "openai-codex:gpt-fixture".into()),
            ("ACP_INTERIM_TURNS".into(), interim.to_string()),
        ]);
        Self {
            _temp: temp,
            root,
            workdir,
            log,
            env,
            config,
        }
    }
    fn request(&self) -> StartRequest {
        let dir = self.directory();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("task.md"),format!("Continue only the assigned fixture step. Write its existing result.json contract.\nFIXTURE_RESULT_PATH={}\n",serde_json::to_string(&dir.join("result.json")).unwrap())).unwrap();
        StartRequest {
            run_id: "FIXTURE-CONTINUATION".into(),
            step: "work".into(),
            bot: "fixture".into(),
            number: 1,
            freezes: BTreeMap::new(),
        }
    }
    fn directory(&self) -> PathBuf {
        self.root.join("FIXTURE-CONTINUATION/steps/work/1")
    }
    fn agent(&self, timeout: Duration) -> ProcessAgent {
        ProcessAgent::new(
            &self.root,
            &self.workdir,
            WORKER,
            BTreeMap::from([("fixture".into(), self.config.clone())]),
            timeout,
        )
        .unwrap()
        .with_env(self.env.clone())
    }
    fn events(&self) -> Vec<Value> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
    fn prompts(&self) -> Vec<Value> {
        self.events()
            .into_iter()
            .filter(|e| e["event"] == "prompt")
            .collect()
    }
    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.directory().join("process.json")).unwrap()).unwrap()
    }
    fn assert_one_session_attempt(&self) {
        let events = self.events();
        assert_eq!(events.iter().filter(|e| e["event"] == "spawn").count(), 1);
        assert_eq!(events.iter().filter(|e| e["event"] == "new").count(), 1);
        let prompts = self.prompts();
        let first = &prompts[0]["sessionId"];
        assert!(prompts.iter().all(|p| &p["sessionId"] == first));
        assert!(
            !events
                .iter()
                .any(|e| e["message"]["method"] == "session/set_model")
        );
        assert_eq!(
            fs::read_dir(self.directory().parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        assert!(
            !self.directory().join("owner-answer.json").exists(),
            "reminder is not an owner answer"
        );
    }
    fn await_followup(&self) {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if self.prompts().len() >= 2 {
                return;
            }
            if self.directory().join("process.json").exists() {
                assert_ne!(
                    self.state()["finished"],
                    true,
                    "adapter stopped after interim end_turn without continuing the same contract"
                );
            }
            assert!(Instant::now() < end, "follow-up prompt was never sent");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
fn finish(agent: &mut ProcessAgent, handle: &str) -> Poll {
    let end = Instant::now() + Duration::from_secs(12);
    loop {
        let value = agent.poll(handle).expect("observe finite ACP fixture");
        if !matches!(value, Poll::Running) {
            return value;
        }
        assert!(Instant::now() < end, "fixture did not settle");
        thread::sleep(Duration::from_millis(10));
    }
}
fn successful_result(poll: Poll) -> Value {
    match poll {
        Poll::Finished {
            process_success,
            completion,
            result,
        } => {
            assert!(
                process_success && completion,
                "confirmed end_turn must carry its explicit outcome"
            );
            serde_json::from_str(&result.unwrap()).unwrap()
        }
        other => panic!("expected settled result, got {other:?}"),
    }
}
fn failed(poll: Poll) {
    match poll {
        Poll::Finished {
            process_success,
            completion,
            ..
        } => assert!(
            !process_success && !completion,
            "cancellation/timeout must remain failed"
        ),
        other => panic!("expected settled cancellation, got {other:?}"),
    }
}

#[test]
fn interim_end_turn_continues_same_contract_and_stops_when_result_appears() {
    let f = Fixture::new(1);
    let mut a = f.agent(Duration::from_secs(8));
    let handle = a.start(&f.request()).unwrap();
    let result = successful_result(finish(&mut a, &handle));
    assert_eq!(result["status"], "done");
    assert_eq!(f.prompts().len(), 2);
    f.assert_one_session_attempt();
    let reminder = f.prompts()[1]["text"].as_str().unwrap().to_lowercase();
    for term in ["continue", "only", "step", "result.json"] {
        assert!(
            reminder.contains(term),
            "follow-up must remain scoped to the existing result contract"
        );
    }
}
#[test]
fn missing_result_exhausts_exactly_three_followups_as_a_capability_block() {
    let f = Fixture::new(100);
    let mut a = f.agent(Duration::from_secs(8));
    let handle = a.start(&f.request()).unwrap();
    let result = successful_result(finish(&mut a, &handle));
    assert_eq!(result["status"], "block");
    assert_eq!(result["block"]["kind"], "capability");
    assert_eq!(
        f.prompts().len(),
        4,
        "one initial turn and at most three reminders"
    );
    assert!(!f.directory().join("result.json").exists());
    assert_eq!(f.state()["hermes"]["settled"], true);
    f.assert_one_session_attempt();
}
#[test]
fn existing_invalid_result_stops_without_another_prompt_or_synthetic_success() {
    let mut f = Fixture::new(0);
    f.env.insert("ACP_INVALID_RESULT".into(), "yes".into());
    let mut a = f.agent(Duration::from_secs(8));
    let handle = a.start(&f.request()).unwrap();
    match finish(&mut a, &handle) {
        Poll::Finished { result, .. } => assert_eq!(
            result.as_deref(),
            Some("invalid fixture result JSON"),
            "strict result validation must receive the existing invalid bytes"
        ),
        other => panic!("expected settled raw result, got {other:?}"),
    };
    assert_eq!(f.prompts().len(), 1);
    f.assert_one_session_attempt();
}
#[test]
fn cancellation_during_followup_targets_the_same_owned_session() {
    let mut f = Fixture::new(1);
    f.env.insert("ACP_HOLD_FOLLOWUP".into(), "yes".into());
    let mut a = f.agent(Duration::from_secs(8));
    let handle = a.start(&f.request()).unwrap();
    f.await_followup();
    let _ = a.stop(&handle);
    failed(finish(&mut a, &handle));
    assert!(a.attempt(&handle).unwrap().cancelled);
    let prompts = f.prompts();
    let target = &prompts[0]["sessionId"];
    assert!(
        f.events()
            .iter()
            .any(|e| e["event"] == "cancel" && &e["sessionId"] == target)
    );
    f.assert_one_session_attempt();
}
#[test]
fn followup_uses_the_original_overall_deadline_instead_of_a_new_budget() {
    let mut f = Fixture::new(1);
    f.env.insert("ACP_HOLD_FOLLOWUP".into(), "yes".into());
    f.env
        .insert("ACP_INTERIM_DELAY_SECONDS".into(), "1.8".into());
    let mut a = f.agent(Duration::from_secs(3));
    let start = Instant::now();
    let handle = a.start(&f.request()).unwrap();
    failed(finish(&mut a, &handle));
    assert!(a.attempt(&handle).unwrap().timed_out);
    assert!(
        f.prompts().len() >= 2,
        "deadline case must reach a follow-up"
    );
    assert!(
        start.elapsed() < Duration::from_millis(4200),
        "a follow-up must not restart the three-second launch budget"
    );
    f.assert_one_session_attempt();
}
