//! Independent, frozen Slice 4 process-adapter acceptance tests.
#![cfg(unix)]
use hermon_flow::{agent::*, hub::*, workflow::Workflow};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const WORKER: &str = env!("CARGO_BIN_EXE_hermon-flow-worker");
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/agent.py");

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    workdir: PathBuf,
    bots: BTreeMap<String, BotConfig>,
    timeout: Duration,
}
impl Fixture {
    fn new(mode: &str, protocol: Protocol, timeout: Duration) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("run records");
        let workdir = dir.path().join("work tree");
        fs::create_dir_all(&workdir).unwrap();
        fs::create_dir_all(&root).unwrap();
        let argv = vec![
            "python3".into(),
            FIXTURE.into(),
            mode.into(),
            "{task}".into(),
            "{workdir}".into(),
        ];
        let config = CommandConfig {
            start: argv.clone(),
            resume: Some([argv, vec!["resume".into(), "{session}".into()]].concat()),
            task_stdin: mode == "stdin",
            protocol,
            family: "fixture-family".into(),
        };
        Self {
            _dir: dir,
            root,
            workdir,
            bots: BTreeMap::from([("fixture".into(), BotConfig::Command(config))]),
            timeout,
        }
    }
    fn agent(&self) -> ProcessAgent {
        ProcessAgent::new(
            &self.root,
            &self.workdir,
            WORKER,
            self.bots.clone(),
            self.timeout,
        )
        .unwrap()
    }
    fn request(&self, number: u32) -> StartRequest {
        let request = StartRequest {
            run_id: "RUN-119".into(),
            step: "work".into(),
            bot: "fixture".into(),
            number,
            freezes: BTreeMap::new(),
        };
        let directory = self.directory(number);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("task.md"),
            "literal task $() ; spaces\nsecond line\n",
        )
        .unwrap();
        request
    }
    fn directory(&self, number: u32) -> PathBuf {
        self.root
            .join("RUN-119/steps/work")
            .join(number.to_string())
    }
    fn started(&self, number: u32) -> Value {
        let path = self.directory(number).join("started.json");
        let started: Value = wait_until(|| {
            fs::read(&path)
                .ok()
                .and_then(|s| serde_json::from_slice(&s).ok())
        });
        if started.get("descendant").is_some() {
            wait_until(|| {
                self.directory(number)
                    .join("heartbeat")
                    .exists()
                    .then_some(())
            });
        }
        started
    }
}

fn wait_until<T>(mut condition: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(value) = condition() {
            return value;
        }
        assert!(Instant::now() < deadline, "fixture observation timed out");
        thread::sleep(Duration::from_millis(20));
    }
}
fn finish(agent: &mut ProcessAgent, handle: &str) -> Poll {
    wait_until(|| match agent.poll(handle).expect("observe owned worker") {
        Poll::Running => None,
        observed => Some(observed),
    })
}
fn successful(observed: Poll) -> Option<String> {
    match observed {
        Poll::Finished {
            process_success,
            completion,
            result,
        } => {
            assert!(process_success, "fixture process must succeed");
            assert!(completion, "fixture must report completion");
            result
        }
        other => panic!("expected settled result, got {other:?}"),
    }
}
fn is_running(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .unwrap();
    output.status.success()
        && String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .any(|s| !s.starts_with('Z'))
}
fn kill_owned(pid: u32, group: bool) {
    let action = if group { "os.killpg" } else { "os.kill" };
    let script = format!(
        "import os,signal;\ntry: {action}({pid},signal.SIGKILL)\nexcept ProcessLookupError: pass"
    );
    assert!(
        Command::new("python3")
            .args(["-c", &script])
            .status()
            .unwrap()
            .success()
    );
}
struct Sentinel(Child);
impl Sentinel {
    fn new() -> Self {
        Self(
            Command::new("python3")
                .args(["-c", "import time; time.sleep(30)"])
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}
impl Drop for Sentinel {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn agent_command_done() {
    let fixture = Fixture::new("done", Protocol::ResultFile, Duration::from_secs(5));
    let request = fixture.request(1);
    let mut agent = fixture.agent();
    assert_eq!(agent.family("fixture"), "fixture-family");
    let handle = agent.start(&request).unwrap();
    let raw = successful(finish(&mut agent, &handle)).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&raw).unwrap()["status"],
        "done"
    );
    let attempt = agent.attempt(&handle).unwrap();
    assert!(attempt.pid > 0);
    assert!(attempt.process_group.unwrap() > 0);
    assert!(attempt.log_path.is_absolute());
    assert!(
        fs::read_to_string(&attempt.log_path)
            .unwrap()
            .contains("fixture stdout")
    );
    assert!(
        fs::read_to_string(&attempt.stderr_path)
            .unwrap()
            .contains("fixture stderr")
    );
    let started = fixture.started(1);
    assert_eq!(
        started["cwd"],
        fixture.workdir.canonicalize().unwrap().to_str().unwrap()
    );
    assert_ne!(started["pgrp"].as_u64().unwrap(), std::process::id() as u64);
    assert_ne!(started["sid"].as_u64().unwrap(), std::process::id() as u64);
    assert!(!attempt.timed_out && !attempt.cancelled);
}

#[derive(Default)]
struct NoEffects;
impl Tracker for NoEffects {
    fn gate_approved(&mut self, _: &str, _: &str, _: u32) -> Result<bool, Block> {
        panic!("no gate")
    }
    fn alert(&mut self, _: &str, _: &Alert) -> anyhow::Result<()> {
        Ok(())
    }
}
impl CodeHost for NoEffects {
    fn open_pr(&mut self, _: &str) -> Result<String, Block> {
        panic!("no PR")
    }
    fn merged(&mut self, _: &str) -> Result<bool, Block> {
        panic!("no merge")
    }
}
impl Git for NoEffects {
    fn check(&mut self, _: &str, _: &[String], _: &BTreeMap<String, String>) -> Result<(), Block> {
        Ok(())
    }
    fn evidence(&mut self, _: &str, _: &str, _: u32) -> Result<String, Block> {
        panic!("no tag")
    }
}
impl Clock for NoEffects {
    fn now(&self) -> u64 {
        0
    }
}
fn hub_outcome(mode: &str) -> Run {
    let fixture = Fixture::new(mode, Protocol::ResultFile, Duration::from_secs(5));
    fixture.request(1);
    let mut agent = fixture.agent();
    let workflow = Workflow::from_str("fixture.yaml", "version: 1\nname: fixture\nsteps:\n  work:\n    bot: fixture\nlimits:\n  transient_retries: 0\n").unwrap();
    let mut run = Run::new("RUN-119", "work");
    let (mut tracker, mut host, mut git, clock) = (NoEffects, NoEffects, NoEffects, NoEffects);
    wait_until(|| {
        advance(
            &mut run,
            &workflow,
            &mut Dependencies {
                agent: &mut agent,
                tracker: &mut tracker,
                code_host: &mut host,
                git: &mut git,
                clock: &clock,
            },
        )
        .unwrap();
        matches!(run.status, Status::Done | Status::Blocked).then_some(())
    });
    run
}
#[test]
fn agent_command_block() {
    let run = hub_outcome("block");
    assert_eq!(run.status, Status::Blocked);
    let block = run.block.unwrap();
    assert_eq!(block.kind, BlockKind::Capability);
    assert_eq!(block.reason, "fixture lacks tool");
}
#[test]
fn agent_command_invalid_result() {
    for mode in ["garbage", "missing"] {
        let run = hub_outcome(mode);
        assert_eq!(run.status, Status::Blocked);
        assert_eq!(run.block.unwrap().kind, BlockKind::Unknown, "{mode}");
        assert!(!run.events.iter().any(|e| e.kind == "step_done"));
    }
}
#[test]
fn agent_command_timeout_kills_group() {
    let fixture = Fixture::new("timeout", Protocol::ResultFile, Duration::from_millis(500));
    let request = fixture.request(1);
    let mut unrelated = Sentinel::new();
    let mut agent = fixture.agent();
    let handle = agent.start(&request).unwrap();
    let started = fixture.started(1);
    let child = started["descendant"].as_u64().unwrap() as u32;
    match finish(&mut agent, &handle) {
        Poll::Finished {
            process_success,
            completion,
            ..
        } => assert!(
            !process_success || !completion,
            "late zero exit cannot accept timeout"
        ),
        observed => panic!("timeout must settle owned group, got {observed:?}"),
    }
    assert!(agent.attempt(&handle).unwrap().timed_out);
    wait_until(|| (!is_running(child)).then_some(()));
    assert!(
        unrelated.0.try_wait().unwrap().is_none(),
        "unrelated process must survive"
    );
}
#[test]
fn agent_restart_reattach() {
    let fixture = Fixture::new("sleep", Protocol::ResultFile, Duration::from_secs(5));
    let request = fixture.request(1);
    let (handle, before) = {
        let mut first = fixture.agent();
        let handle = first.start(&request).unwrap();
        fixture.started(1);
        let before = first.attempt(&handle).unwrap();
        (handle, before)
    };
    let mut restarted = fixture.agent();
    assert!(is_running(before.pid), "detached worker survives hub drop");
    assert_eq!(
        restarted.start(&request).unwrap(),
        handle,
        "restart must not duplicate launch"
    );
    assert_eq!(restarted.attempt(&handle).unwrap().pid, before.pid);
    successful(finish(&mut restarted, &handle));
    assert_eq!(
        fs::read_to_string(fixture.directory(1).join("launches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}
#[test]
fn agent_hermes_argv() {
    let task = Path::new("/work tree/task file.md");
    assert_eq!(
        hermes_argv("sdlc-docs", task, None),
        vec![
            "hermes",
            "-p",
            "sdlc-docs",
            "chat",
            "-Q",
            "--query-file",
            "/work tree/task file.md"
        ]
    );
    assert_eq!(
        hermes_argv("sdlc-docs", task, Some("local-opaque-119")),
        vec![
            "hermes",
            "-p",
            "sdlc-docs",
            "chat",
            "-Q",
            "--query-file",
            "/work tree/task file.md",
            "--resume",
            "local-opaque-119"
        ]
    );
    assert_eq!(
        hermes_session_id(
            "session_id: first\r\nstdout mention session_id: false\n session_id: false\nsession_id: too many words\nsession_id: local-opaque-119\r\n"
        ),
        Some("local-opaque-119".into())
    );
    assert_eq!(hermes_session_id("session_id: \nSession: wrong\n"), None);
}
#[test]
fn agent_hermes_supervision_requires_verified_capability() {
    let mut fixture = Fixture::new("done", Protocol::ResultFile, Duration::from_secs(5));
    fixture.bots.insert(
        "fixture".into(),
        BotConfig::Hermes {
            profile: "sdlc-docs".into(),
        },
    );
    let request = fixture.request(1);
    let error = fixture.agent().start(&request).unwrap_err();
    assert_eq!(error.kind, BlockKind::Capability);
    assert!(!fixture.directory(1).join("started.json").exists());
}
#[test]
fn agent_command_resume_argv_and_task_stdin() {
    let fixture = Fixture::new("stdin", Protocol::ResultFile, Duration::from_secs(5));
    let request = fixture.request(2);
    let mut agent = fixture.agent();
    agent.set_resume(&request, Some("opaque session $() ;".into()));
    let handle = agent.start(&request).unwrap();
    successful(finish(&mut agent, &handle));
    let info = fixture.started(2);
    assert_eq!(
        info["extra"],
        serde_json::json!(["resume", "opaque session $() ;"])
    );
    assert_eq!(info["stdin"], "literal task $() ; spaces\nsecond line\n");
    assert_eq!(
        info["task"],
        fixture
            .directory(2)
            .canonicalize()
            .unwrap()
            .join("task.md")
            .to_str()
            .unwrap()
    );
    assert_eq!(
        info["workdir"],
        fixture.workdir.canonicalize().unwrap().to_str().unwrap()
    );
}
#[test]
fn agent_command_missing_resume_template_starts_fresh() {
    let mut fixture = Fixture::new("done", Protocol::ResultFile, Duration::from_secs(5));
    let BotConfig::Command(config) = fixture.bots.get_mut("fixture").unwrap() else {
        unreachable!()
    };
    config.resume = None;
    let request = fixture.request(1);
    let mut agent = fixture.agent();
    agent.set_resume(&request, Some("old-session".into()));
    let handle = agent.start(&request).unwrap();
    successful(finish(&mut agent, &handle));
    assert_eq!(fixture.started(1)["extra"], serde_json::json!([]));
}
#[test]
fn agent_timeout_remains_enforced_while_hub_is_absent() {
    let fixture = Fixture::new("timeout", Protocol::ResultFile, Duration::from_millis(500));
    let (handle, child) = {
        let mut agent = fixture.agent();
        let handle = agent.start(&fixture.request(1)).unwrap();
        let child = fixture.started(1)["descendant"].as_u64().unwrap() as u32;
        (handle, child)
    };
    wait_until(|| (!is_running(child)).then_some(()));
    let mut restarted = fixture.agent();
    match finish(&mut restarted, &handle) {
        Poll::Finished {
            process_success,
            completion,
            ..
        } => assert!(!process_success || !completion),
        observed => panic!("expected timeout receipt, got {observed:?}"),
    }
    assert!(restarted.attempt(&handle).unwrap().timed_out);
}
#[test]
fn agent_command_protocol_completion_and_session_capture() {
    for (mode, protocol, completed, session) in [
        ("codex", Protocol::CodexJson, true, "opaque-thread-119"),
        (
            "codex-no-completion",
            Protocol::CodexJson,
            false,
            "opaque-thread-119",
        ),
        ("claude", Protocol::ClaudeJson, true, "claude-119"),
        ("claude-error", Protocol::ClaudeJson, false, "claude-119"),
    ] {
        let fixture = Fixture::new(mode, protocol, Duration::from_secs(5));
        let mut agent = fixture.agent();
        let handle = agent.start(&fixture.request(1)).unwrap();
        match finish(&mut agent, &handle) {
            Poll::Finished {
                process_success,
                completion,
                ..
            } => {
                assert!(process_success, "{mode}");
                assert_eq!(completion, completed, "{mode}");
            }
            observed => panic!("expected process exit: {observed:?}"),
        }
        assert_eq!(
            agent.attempt(&handle).unwrap().session_id.as_deref(),
            Some(session),
            "{mode}"
        );
        let restarted = fixture.agent();
        assert_eq!(
            restarted.attempt(&handle).unwrap().session_id.as_deref(),
            Some(session),
            "session survives restart"
        );
    }
}
#[test]
fn agent_command_nonzero_exit_rejects_valid_result() {
    let fixture = Fixture::new("nonzero", Protocol::ResultFile, Duration::from_secs(5));
    let mut agent = fixture.agent();
    let handle = agent.start(&fixture.request(1)).unwrap();
    match finish(&mut agent, &handle) {
        Poll::Finished {
            process_success, ..
        } => assert!(!process_success),
        observed => panic!("expected failure: {observed:?}"),
    }
}
#[test]
fn agent_command_cancel_cannot_become_late_success() {
    let fixture = Fixture::new("cancel", Protocol::ResultFile, Duration::from_secs(5));
    let mut agent = fixture.agent();
    let handle = agent.start(&fixture.request(1)).unwrap();
    let child = fixture.started(1)["descendant"].as_u64().unwrap() as u32;
    wait_until(|| match agent.stop(&handle).unwrap() {
        Settlement::Confirmed => Some(()),
        Settlement::Unsettled => None,
    });
    assert!(agent.attempt(&handle).unwrap().cancelled);
    match finish(&mut agent, &handle) {
        Poll::Finished {
            process_success,
            completion,
            ..
        } => assert!(!process_success || !completion),
        observed => panic!("expected settled cancellation, got {observed:?}"),
    }
    wait_until(|| (!is_running(child)).then_some(()));
}
#[test]
fn agent_dead_supervisor_is_transient_and_does_not_signal_unrelated_process() {
    let fixture = Fixture::new("sleep", Protocol::ResultFile, Duration::from_secs(5));
    let request = fixture.request(1);
    let mut unrelated = Sentinel::new();
    let (handle, attempt) = {
        let mut agent = fixture.agent();
        let handle = agent.start(&request).unwrap();
        fixture.started(1);
        let attempt = agent.attempt(&handle).unwrap();
        kill_owned(attempt.pid, false);
        wait_until(|| (!is_running(attempt.pid)).then_some(()));
        (handle, attempt)
    };
    let mut restarted = fixture.agent();
    let error = restarted.poll(&handle).unwrap_err();
    assert_eq!(error.kind, BlockKind::Transient);
    assert!(unrelated.0.try_wait().unwrap().is_none());
    assert_eq!(
        restarted.start(&request).unwrap(),
        handle,
        "uncertain worker cannot be resent"
    );
    if let Some(group) = attempt.process_group {
        kill_owned(group, true);
    }
}
