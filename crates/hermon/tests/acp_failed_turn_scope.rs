//! Additional recovery binding boundaries.
#![cfg(unix)]
use hermon_flow::{
    agent::{BotConfig, ProcessAgent},
    hub::{Agent, StartRequest},
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
const WORKER: &str = env!("CARGO_BIN_EXE_hermon");
const SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../hermon-flow/tests/fixtures/hermes-acp-failed-turn.py"
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
    fn new(case: &str) -> Self {
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
            ("ACP_FAILURE_CASE".into(), case.into()),
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
            run_id: "FIXTURE-FAILED".into(),
            step: "work".into(),
            bot: "fixture".into(),
            number: 1,
            freezes: BTreeMap::new(),
        }
    }
    fn directory(&self) -> PathBuf {
        self.root.join("FIXTURE-FAILED/steps/work/1")
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
        .with_worker_subcommand("__flow-worker")
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
    fn wait_dead(&self, agent: &mut ProcessAgent, handle: &str) {
        let end = Instant::now() + Duration::from_secs(12);
        loop {
            if agent.poll(handle).is_err()
                || (self.directory().join("process.json").exists()
                    && self.state()["finished"] == true)
            {
                return;
            }
            assert!(Instant::now() < end, "finite fixture did not end");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn legacy(f: &Fixture) {
    let path = f.directory().join("process.json");
    let mut state = f.state();
    state["finished"] = json!(false);
    state["process_success"] = json!(false);
    state["completion"] = json!(false);
    state["result"] = Value::Null;
    state["exit_code"] = Value::Null;
    state["hermes"]["settled"] = json!(false);
    fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
}
fn record(f: &Fixture, handle: &str) {
    use hermon_flow::{
        control::RunRecord,
        hub::{Attempt, Run, Status},
    };
    let mut run = Run::new("FIXTURE-FAILED", "work");
    run.status = Status::Blocked;
    run.unsettled = true;
    run.active = Some(Attempt {
        step: "work".into(),
        number: 1,
        handle: handle.into(),
        started_at: 0,
        family: "openai".into(),
        freezes: BTreeMap::new(),
    });
    run.attempts.push(run.active.clone().unwrap());
    let workflow_source =
        "version: 1\nname: fixture\nsteps:\n  work:\n    bot: fixture\n".to_owned();
    let record = RunRecord {
        schema: 1,
        ticket: "FIXTURE-FAILED".into(),
        workflow_name: "fixture".into(),
        workflow_hash: hermon_flow::intake::hash(&workflow_source),
        workflow_source,
        template_hashes: BTreeMap::new(),
        templates: BTreeMap::new(),
        project: f.workdir.join("project.yaml"),
        worktree: f.workdir.clone(),
        base: "main".into(),
        base_commit: "fixture-base".into(),
        branch: "fixture".into(),
        paused: false,
        cancelled: false,
        run,
        notices: vec![],
        started: true,
    };
    hermon_flow::control::save(&f.root.join("FIXTURE-FAILED"), &record).unwrap();
}
fn reconcile(f: &Fixture, step: &str) -> std::process::Output {
    Command::new(WORKER)
        .args(["reconcile", "FIXTURE-FAILED", step, "--data-dir"])
        .arg(f.root.parent().unwrap())
        .output()
        .unwrap()
}

#[test]
fn recovery_requires_a_bound_session_and_a_literal_owned_attempt() {
    for case in [
        "missing-open-session",
        "different-open-session",
        "foreign-resume",
        "symlink-attempt",
    ] {
        let f = Fixture::new("known");
        let mut agent = f.agent(Duration::from_secs(8));
        let handle = agent.start(&f.request()).unwrap();
        f.wait_dead(&mut agent, &handle);
        assert_eq!(f.prompts().len(), 1);
        legacy(&f);
        record(&f, &handle);
        let log = f.directory().join("log");
        if case.ends_with("open-session") {
            let mut frames: Vec<Value> = fs::read_to_string(&log)
                .unwrap()
                .lines()
                .map(|s| serde_json::from_str(s).unwrap())
                .collect();
            let open = frames.iter_mut().find(|v| v["id"] == 2).unwrap()["result"]
                .as_object_mut()
                .unwrap();
            if case == "missing-open-session" {
                open.remove("sessionId");
            } else {
                open.insert("sessionId".into(), json!("foreign-session"));
            }
            fs::write(
                &log,
                frames
                    .iter()
                    .map(|v| v.to_string() + "\n")
                    .collect::<String>(),
            )
            .unwrap();
        } else if case == "foreign-resume" {
            let path = f.directory().join("launch.json");
            let mut launch: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            launch["resume_session"] = json!("foreign-session");
            fs::write(path, serde_json::to_vec(&launch).unwrap()).unwrap();
        } else {
            let original = fs::canonicalize(f.directory()).unwrap();
            let other = original.with_file_name("2");
            fs::rename(&original, &other).unwrap();
            std::os::unix::fs::symlink(&other, &original).unwrap();
            let path = other.join("process.json");
            let mut state = f.state();
            state["attempt"]["log_path"] = json!(other.join("log"));
            fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
        }
        let before = fs::read(f.directory().join("process.json")).unwrap();
        assert!(
            !reconcile(&f, "work").status.success(),
            "unsafe binding {case} was accepted"
        );
        assert_eq!(
            fs::read(f.directory().join("process.json")).unwrap(),
            before
        );
    }
}
