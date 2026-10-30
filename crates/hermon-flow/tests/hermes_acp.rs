//! Independent public-ACP transport acceptance; no vendor process is contacted.
#![cfg(unix)]
use hermon_flow::{
    agent::{BotConfig, ProcessAgent},
    hub::{Agent, BlockKind, Poll, Settlement, StartRequest},
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
const ACP_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hermes-acp.py");
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
fn acp_start_records_runtime_session_and_preserves_profile_argv() {
    let fixture = Fixture::new("done");
    let request = fixture.request("target", 1);
    let mut agent = fixture.agent(Duration::from_secs(5));
    let handle = agent.start(&request).unwrap();
    match finish(&mut agent, &handle) {
        Poll::Finished {
            process_success,
            completion,
            result,
        } => {
            assert!(process_success && completion);
            assert_eq!(
                serde_json::from_str::<Value>(&result.unwrap()).unwrap()["status"],
                "done"
            );
        }
        other => panic!("expected ACP end_turn, got {other:?}"),
    }
    let attempt = agent.attempt(&handle).unwrap();
    let state = fixture.state(&handle);
    assert!(
        attempt
            .session_id
            .unwrap()
            .starts_with("hermes-sdlc-implementer-")
    );
    assert_eq!(state["hermes"]["profile"], "sdlc-implementer");
    assert_eq!(state["hermes"]["model_id"], MODEL);
    assert_eq!(state["hermes"]["family"], "openai");
    assert_eq!(state["hermes"]["settled"], true);
    assert_eq!(agent.family("target"), "openai");
    let events = fixture.events();
    assert!(events.iter().any(|e|e["event"]=="spawn"&&e["argv"]==json!(["-p","sdlc-implementer","acp"])));
    assert!(events.iter().any(|e| e["event"] == "new"
        && e["cwd"] == fixture.workdir.canonicalize().unwrap().to_str().unwrap()
        && e["mcpServers"] == json!([])));
    assert!(events.iter().any(|e| e["event"] == "received"
        && e["message"]["method"] == "session/set_mode"
        && e["message"]["params"]["modeId"] == "accept_edits"));
}

#[test]
fn acp_deadline_cancels_exact_session_and_leaves_unrelated_session_running() {
    let fixture = Fixture::new("hold");
    let mut target = fixture.agent(Duration::from_millis(450));
    let mut control = fixture.agent(Duration::from_secs(20));
    let control_handle = control.start(&fixture.request("control", 1)).unwrap();
    let control_id = started(&fixture, &control, &control_handle);
    let target_handle = target.start(&fixture.request("target", 1)).unwrap();
    let target_id = started(&fixture, &target, &target_handle);
    wait(|| fixture.heartbeat(&control_id));
    wait(|| fixture.heartbeat(&target_id));
    failed_terminal(finish(&mut target, &target_handle));
    assert!(target.attempt(&target_handle).unwrap().timed_out);
    assert_eq!(fixture.state(&target_handle)["hermes"]["settled"], true);
    let target_heartbeat = fixture.heartbeat(&target_id);
    let control_heartbeat = fixture.heartbeat(&control_id);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        fixture.heartbeat(&target_id),
        target_heartbeat,
        "target's active tool must stop after its terminal cancellation"
    );
    assert_ne!(
        fixture.heartbeat(&control_id),
        control_heartbeat,
        "unrelated session must continue after targeted cancellation"
    );
    let acp = fixture.state(&target_handle)["hermes"]["acp_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        fixture
            .events()
            .iter()
            .any(|e| e["event"] == "cancel" && e["sessionId"] == acp)
    );
    assert!(matches!(
        control.poll(&control_handle).unwrap(),
        Poll::Running
    ));
    let _ = control.stop(&control_handle);
    failed_terminal(finish(&mut control, &control_handle));
    assert!(matches!(
        control.stop(&control_handle).unwrap(),
        Settlement::Confirmed
    ));
}

#[test]
fn acp_late_end_turn_cannot_override_persisted_cancellation() {
    let fixture = Fixture::new("late_end");
    let mut agent = fixture.agent(Duration::from_secs(5));
    let handle = agent.start(&fixture.request("target", 1)).unwrap();
    started(&fixture, &agent, &handle);
    let _ = agent.stop(&handle);
    failed_terminal(finish(&mut agent, &handle));
    assert!(agent.attempt(&handle).unwrap().cancelled);
    assert_eq!(fixture.state(&handle)["hermes"]["settled"], true);
}

#[test]
fn acp_viewer_exit_without_terminal_response_remains_uncertain() {
    let fixture = Fixture::new("lose");
    let mut agent = fixture.agent(Duration::from_secs(5));
    let handle = agent.start(&fixture.request("target", 1)).unwrap();
    wait(|| {
        fixture
            .events()
            .iter()
            .any(|e| e["event"] == "viewer_exit_without_terminal")
            .then_some(())
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match agent.poll(&handle) {
            Err(block) => {
                assert!(matches!(
                    block.kind,
                    BlockKind::Transient | BlockKind::Unknown | BlockKind::Capability
                ));
                break;
            }
            Ok(Poll::Finished {
                process_success,
                completion,
                ..
            }) => {
                assert!(!process_success && !completion);
                break;
            }
            Ok(Poll::Running) => {
                assert!(
                    Instant::now() < deadline,
                    "lost viewer must surface an uncertain outcome"
                );
                thread::sleep(Duration::from_millis(20));
            }
            Ok(_) => break, // Explicit unsettled observation is acceptable.
        }
    }
    assert_ne!(
        fixture.state(&handle)["hermes"]["settled"],
        true,
        "viewer death cannot prove gateway settlement"
    );
    assert!(
        !matches!(agent.stop(&handle), Ok(Settlement::Confirmed)),
        "unknown session outcome cannot be confirmed settled from viewer death"
    );
    assert!(
        !agent.available("target"),
        "uncertain shared-session work must retain its concurrency slot"
    );
}

#[test]
fn acp_owner_answer_loads_exact_prior_acp_session_and_delivers_answer() {
    let mut fixture = Fixture::new("question");
    let mut agent = fixture.agent(Duration::from_secs(5));
    let previous = agent.start(&fixture.request("target", 1)).unwrap();
    let _ = finish(&mut agent, &previous);
    let acp = fixture.state(&previous)["hermes"]["acp_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    fixture.env.insert("ACP_SCENARIO".into(), "done".into());
    // Restart the host adapter to prove the session comes from the prior receipt.
    let mut resumed = fixture.agent(Duration::from_secs(5));
    let next = resumed
        .resume_after_answer(
            &previous,
            &fixture.request("target", 2),
            "Use the owner's exact blue behavior.",
        )
        .unwrap();
    let _ = finish(&mut resumed, &next);
    let events = fixture.events();
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "load" && e["sessionId"] == acp),
        "resume must explicitly load the recorded ACP session"
    );
    assert!(
        events
            .iter()
            .filter(|e| e["event"] == "prompt")
            .any(|e| e["text"]
                .as_str()
                .unwrap()
                .contains("Use the owner's exact blue behavior.")),
        "resumed prompt must carry the owner answer"
    );
    assert_eq!(fixture.state(&next)["hermes"]["acp_session_id"], acp);
}

#[test]
fn acp_dangerous_permission_request_is_denied_by_default() {
    let fixture = Fixture::new("permission");
    let mut agent = fixture.agent(Duration::from_secs(5));
    let handle = agent.start(&fixture.request("target", 1)).unwrap();
    let _ = finish(&mut agent, &handle);
    let response = fixture
        .events()
        .into_iter()
        .find(|e| e["event"] == "permission_response")
        .expect("ACP client must answer a permission request explicitly");
    assert_ne!(
        response["response"]["result"]["outcome"]["optionId"], "allow_once",
        "dangerous tool execution must never be automatically approved"
    );
    assert!(
        response["response"]["result"]["outcome"]["outcome"] == "cancelled"
            || response["response"]["result"]["outcome"]["optionId"] == "reject_once",
        "default policy must reject the request: {response}"
    );
}
