//! Failed admitted turns must settle as failures; ambiguous errors must retain ownership.
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

fn failed_terminal(state: &Value) {
    assert_eq!(
        state["finished"], true,
        "known admitted-turn failure must settle"
    );
    assert_eq!(state["hermes"]["settled"], true);
    if state["process_success"] == true && state["completion"] == true {
        let result: Value = serde_json::from_str(state["result"].as_str().unwrap()).unwrap();
        assert_eq!(result["status"], "block", "failure cannot become success");
    }
}
#[test]
fn explicit_failed_admission_settles_without_a_result_or_followup() {
    let f = Fixture::new("known");
    let mut a = f.agent(Duration::from_secs(8));
    let h = a.start(&f.request()).unwrap();
    f.wait_dead(&mut a, &h);
    failed_terminal(&f.state());
    assert!(matches!(a.poll(&h).unwrap(), Poll::Finished { .. }));
    assert_eq!(f.prompts().len(), 1);
    assert!(!f.directory().join("result.json").exists());
}
#[test]
fn generic_unmatched_or_request_errors_cannot_claim_settlement() {
    for case in [
        "generic",
        "wrong-code",
        "wrong-detail",
        "notification",
        "request-collision",
        "wrong-id",
    ] {
        let f = Fixture::new(case);
        let mut a = f.agent(Duration::from_secs(8));
        let h = a.start(&f.request()).unwrap();
        f.wait_dead(&mut a, &h);
        assert_eq!(f.state()["finished"], false, "unsafe case {case}");
        assert_eq!(f.state()["hermes"]["settled"], false);
        assert!(a.poll(&h).is_err());
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
fn reconciliation_recovers_only_the_recorded_failed_turn_and_preserves_artifacts() {
    let f = Fixture::new("known");
    let mut a = f.agent(Duration::from_secs(8));
    let h = a.start(&f.request()).unwrap();
    f.wait_dead(&mut a, &h);
    legacy(&f);
    record(&f, &h);
    fs::write(
        f.directory().join("result.json"),
        "invalid candidate result\n",
    )
    .unwrap();
    let log = fs::read(f.directory().join("log")).unwrap();
    let launch = fs::read(f.directory().join("launch.json")).unwrap();
    let before = f.events().len();
    let out = reconcile(&f, "work");
    assert!(
        out.status.success(),
        "reconcile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    failed_terminal(&f.state());
    assert_eq!(fs::read(f.directory().join("log")).unwrap(), log);
    assert_eq!(fs::read(f.directory().join("launch.json")).unwrap(), launch);
    assert_eq!(
        fs::read_to_string(f.directory().join("result.json")).unwrap(),
        "invalid candidate result\n"
    );
    assert_eq!(
        f.events().len(),
        before,
        "reconciliation cannot start a viewer, session or prompt"
    );
    let result = f.state()["result"].as_str().unwrap().to_owned();
    assert_ne!(
        serde_json::from_str::<Value>(&result).unwrap()["status"],
        "done"
    );
    // The host must still observe settlement and require an explicit retry.
    let run: Value =
        serde_json::from_slice(&fs::read(f.root.join("FIXTURE-FAILED/run.json")).unwrap()).unwrap();
    assert_eq!(run["run"]["status"], "Blocked");
    assert_eq!(run["run"]["current"], "work");
}
#[test]
fn reconciliation_refuses_ambiguous_or_unbound_recorded_evidence() {
    for case in [
        "generic",
        "wrong-code",
        "wrong-detail",
        "notification",
        "request-collision",
        "wrong-id",
        "session-mismatch",
        "broken-log",
        "trailing-frame",
        "missing-log",
        "symlink-log",
        "wrong-step",
    ] {
        let f = Fixture::new(
            if matches!(
                case,
                "session-mismatch"
                    | "broken-log"
                    | "trailing-frame"
                    | "missing-log"
                    | "symlink-log"
                    | "wrong-step"
            ) {
                "known"
            } else {
                case
            },
        );
        let mut a = f.agent(Duration::from_secs(8));
        let h = a.start(&f.request()).unwrap();
        f.wait_dead(&mut a, &h);
        legacy(&f);
        record(&f, &h);
        let path = f.directory().join("log");
        match case {
            "session-mismatch" => {
                let p = f.directory().join("process.json");
                let mut s = f.state();
                s["hermes"]["acp_session_id"] = json!("foreign-session");
                fs::write(p, serde_json::to_vec(&s).unwrap()).unwrap();
            }
            "broken-log" => {
                fs::write(&path, b"not json\n").unwrap();
            }
            "trailing-frame" => {
                use std::io::Write;
                fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap()
                    .write_all(b"{\"id\":999,\"result\":{}}\n")
                    .unwrap();
            }
            "missing-log" => {
                fs::remove_file(&path).unwrap();
            }
            "symlink-log" => {
                let target = f.workdir.join("other-log");
                fs::copy(&path, &target).unwrap();
                fs::remove_file(&path).unwrap();
                std::os::unix::fs::symlink(target, &path).unwrap();
            }
            _ => {}
        }
        let before = fs::read(f.directory().join("process.json")).unwrap();
        assert!(
            !reconcile(
                &f,
                if case == "wrong-step" {
                    "other"
                } else {
                    "work"
                }
            )
            .status
            .success(),
            "unsafe case {case}"
        );
        assert_eq!(
            fs::read(f.directory().join("process.json")).unwrap(),
            before
        );
    }
}

#[test]
fn reconciliation_never_touches_a_live_worker_receipt() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let f = Fixture::new("known");
    let mut a = f.agent(Duration::from_secs(8));
    let h = a.start(&f.request()).unwrap();
    f.wait_dead(&mut a, &h);
    legacy(&f);
    record(&f, &h);
    let mut locker = Command::new("python3")
        .args(["-u", "-c", "import fcntl,select,sys; f=open(sys.argv[1],'a'); fcntl.flock(f,fcntl.LOCK_EX); print('held',flush=True); select.select([sys.stdin],[],[],10)"])
        .arg(f.directory().join("worker.lock"))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(locker.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "held");
    let before = fs::read(f.directory().join("process.json")).unwrap();
    let output = reconcile(&f, "work");
    drop(locker.stdin.take());
    locker.wait().unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fs::read(f.directory().join("process.json")).unwrap(),
        before
    );
}
