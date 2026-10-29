//! Supplemental independent regressions: log authority and complete test reports.
#![cfg(unix)]
use hermon_flow::{checks::*, hub::*};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    config: CheckConfig,
}
fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let repo = fs::canonicalize(repo).unwrap();
        git(&repo, &["init", "-b", "run"]);
        git(&repo, &["config", "user.name", "Fixture"]);
        git(&repo, &["config", "user.email", "fixture@example.invalid"]);
        fs::write(repo.join("base"), "base").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base"]);
        let config = CheckConfig {
            workdir: repo.clone(),
            base: git(&repo, &["rev-parse", "HEAD"]),
            ticket: "120".into(),
            branch: "run".into(),
            log_dir: dir.path().join("logs"),
            env: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            paths: BTreeMap::new(),
            commands: BTreeMap::from([(
                "lint".into(),
                CheckCommand {
                    argv: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "printf 'original check output\\n'".into(),
                    ],
                    timeout_seconds: 3,
                    report: None,
                },
            )]),
        };
        Self {
            _dir: dir,
            repo,
            config,
        }
    }
    fn context(&self) -> AttemptContext {
        AttemptContext {
            number: 1,
            bot: "fixture".into(),
            new_tests: ["new_case".into()].into(),
            ..Default::default()
        }
    }
    fn adapter(&self) -> GitChecks {
        let mut a = GitChecks::new(self.config.clone()).unwrap();
        a.set_attempt(self.context());
        a
    }
    fn output_log(&self) -> PathBuf {
        let directory = fs::read_dir(&self.config.log_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.join("output.log").exists())
            .unwrap();
        directory.join("output.log")
    }
    fn check(&self, a: &mut GitChecks) {
        a.check("red", &["lint".into()], &BTreeMap::new()).unwrap();
    }
    fn tag_count(&self) -> usize {
        git(&self.repo, &["tag", "--list", "hermon/*"])
            .lines()
            .count()
    }
}

#[test]
fn evidence_rejects_modified_check_log() {
    let f = Fixture::new();
    let mut a = f.adapter();
    f.check(&mut a);
    fs::write(f.output_log(), "replaced output").unwrap();
    assert!(
        a.evidence("red", "red", 1).is_err(),
        "published evidence must bind the actual log bytes"
    );
    assert_eq!(f.tag_count(), 0);
}
#[test]
fn evidence_rejects_removed_check_log() {
    let f = Fixture::new();
    let mut a = f.adapter();
    f.check(&mut a);
    fs::remove_file(f.output_log()).unwrap();
    assert!(
        a.evidence("red", "red", 1).is_err(),
        "a nonexistent log cannot support a new tag"
    );
    assert_eq!(f.tag_count(), 0);
}
#[test]
fn evidence_retry_repairs_missing_explicit_note() {
    let f = Fixture::new();
    let mut a = f.adapter();
    f.check(&mut a);
    let tag = a.evidence("red", "red", 1).unwrap();
    let object = git(&f.repo, &["rev-parse", &tag]);
    git(&f.repo, &["notes", "--ref=hermon", "remove", &object]);
    assert_eq!(f.adapter().evidence("red", "red", 1).unwrap(), tag);
    let note = git(&f.repo, &["notes", "--ref=hermon", "show", &object]);
    let parsed: Value = serde_json::from_str(&note).unwrap();
    assert_eq!(parsed["step"], "red");
    assert_eq!(git(&f.repo, &["rev-parse", &tag]), object);
}
#[test]
fn evidence_binds_step_and_attempt_context() {
    let f = Fixture::new();
    let mut a = f.adapter();
    f.check(&mut a);
    assert!(a.evidence("green", "green", 1).is_err());
    let mut next = f.context();
    next.number = 2;
    a.set_attempt(next);
    assert!(
        a.evidence("red", "red", 1).is_err(),
        "a new attempt cannot inherit old check success"
    );
    assert_eq!(f.tag_count(), 0);
}
#[test]
fn check_relative_path_and_clean_environment() {
    let mut f = Fixture::new();
    fs::create_dir(f.repo.join("bin")).unwrap();
    let script = f.repo.join("bin/checker");
    fs::write(
        &script,
        "#!/bin/sh\ntest -z \"${HOME+x}\" || exit 3\ntest \"$CONFIGURED\" = 'yes' || exit 4\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    git(&f.repo, &["add", "."]);
    git(&f.repo, &["commit", "-m", "relative checker"]);
    f.config.env.insert("PATH".into(), "bin".into());
    f.config.env.insert("CONFIGURED".into(), "yes".into());
    f.config.commands.get_mut("lint").unwrap().argv = vec!["checker".into()];
    f.check(&mut f.adapter());
    f.config.commands.get_mut("lint").unwrap().argv = vec!["./bin/checker".into()];
    f.check(&mut f.adapter());
}
#[test]
fn check_command_dirty_mutation_cannot_receive_evidence() {
    let mut f = Fixture::new();
    f.config.commands.get_mut("lint").unwrap().argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf changed > base".into(),
    ];
    let mut a = f.adapter();
    assert!(a.check("red", &["lint".into()], &BTreeMap::new()).is_err());
    assert!(a.evidence("red", "red", 1).is_err());
    assert_eq!(f.tag_count(), 0);
}
#[test]
fn check_libtest_tail_cannot_silently_drop_earlier_failures() {
    let mut f = Fixture::new();
    let transcript = f._dir.path().join("test stdout");
    let mut text="running 1 test\ntest hidden_old_case ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n".to_string();
    text.push_str(&"diagnostic line\n".repeat(80_000));
    text.push_str("running 1 test\ntest new_case ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n");
    fs::write(&transcript, text).unwrap();
    f.config.commands.insert(
        "test".into(),
        CheckCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "/bin/cat \"$1\"; exit 101".into(),
                "fixture".into(),
                transcript.display().to_string(),
            ],
            timeout_seconds: 3,
            report: Some(TestReportFormat::Libtest),
        },
    );
    let mut a = f.adapter();
    if a.check("red", &["tests_fail".into()], &BTreeMap::new())
        .is_ok()
    {
        let tag = a.evidence("red", "red", 1).unwrap();
        let note = git(&f.repo, &["notes", "--ref=hermon", "show", &tag]);
        let parsed: Value = serde_json::from_str(&note).unwrap();
        assert_eq!(
            parsed["commands"][0]["tests"]["failed"], 2,
            "either reject incomplete stdout or count all failures"
        );
    }
}

struct AgentFixture;
impl Agent for AgentFixture {
    fn family(&self, _: &str) -> String {
        "fixture".into()
    }
    fn available(&self, _: &str) -> bool {
        true
    }
    fn start(&mut self, _: &StartRequest) -> Result<String, Block> {
        Ok("owned".into())
    }
    fn poll(&mut self, _: &str) -> Result<Poll, Block> {
        Ok(Poll::Finished{process_success:true,completion:true,result:Some(json!({"schema":1,"status":"done","summary":"done","artifacts":[],"block":null,"verdict":null,"questions":[]}).to_string())})
    }
    fn stop(&mut self, _: &str) -> Result<Settlement, Block> {
        Ok(Settlement::Confirmed)
    }
}
struct TrackerFixture;
impl Tracker for TrackerFixture {
    fn gate_approved(&mut self, _: &str, _: &str, _: u32) -> Result<bool, Block> {
        Ok(false)
    }
    fn alert(&mut self, _: &str, _: &Alert) -> anyhow::Result<()> {
        Ok(())
    }
}
struct HostFixture;
impl CodeHost for HostFixture {
    fn open_pr(&mut self, _: &str) -> Result<String, Block> {
        unreachable!()
    }
    fn merged(&mut self, _: &str) -> Result<bool, Block> {
        unreachable!()
    }
}
struct ClockFixture;
impl Clock for ClockFixture {
    fn now(&self) -> u64 {
        0
    }
}
#[test]
fn hub_real_checks_publish_only_successful_evidence() {
    let f = Fixture::new();
    let workflow=hermon_flow::workflow::Workflow::from_str("fixture","version: 1\nname: fixture\nsteps:\n  red:\n    bot: fixture\n    check: [lint]\n    tag: red\n").unwrap();
    let mut run = Run::new("120", "red");
    let mut a = AgentFixture;
    let mut tracker = TrackerFixture;
    let mut host = HostFixture;
    let clock = ClockFixture;
    let mut checks = f.adapter();
    let mut deps = Dependencies {
        agent: &mut a,
        tracker: &mut tracker,
        code_host: &mut host,
        git: &mut checks,
        clock: &clock,
    };
    advance(&mut run, &workflow, &mut deps).unwrap();
    advance(&mut run, &workflow, &mut deps).unwrap();
    assert_eq!(run.status, Status::Done);
    assert_eq!(run.freezes["red"], "hermon/120/red");
    assert_eq!(f.tag_count(), 1);
}

#[test]
fn frozen_check_rejects_mismatched_annotation_and_explicit_note() {
    let f = Fixture::new();
    let mut a = f.adapter();
    f.check(&mut a);
    let tag = a.evidence("red", "red", 1).unwrap();
    git(
        &f.repo,
        &[
            "notes",
            "--ref=hermon",
            "add",
            "-f",
            "-m",
            "{\"tampered\":true}",
            &tag,
        ],
    );
    let refs = BTreeMap::from([("red".into(), tag)]);
    assert!(
        a.check("green", &["frozen_unchanged:red".into()], &refs)
            .is_err(),
        "conflicting freeze authority must not pass silently"
    );
}
#[test]
fn frozen_check_rejects_another_evidence_kind() {
    let f = Fixture::new();
    let mut a = f.adapter();
    f.check(&mut a);
    let tag = a.evidence("red", "plan", 1).unwrap();
    let refs = BTreeMap::from([("red".into(), tag)]);
    assert!(
        a.check("green", &["frozen_unchanged:red".into()], &refs)
            .is_err(),
        "the requested freeze key must match the recorded evidence kind"
    );
}
#[test]
fn check_timeout_settles_owned_descendants_only() {
    let mut f = Fixture::new();
    let heartbeat = f._dir.path().join("heartbeat");
    let python = Command::new("python3")
        .args(["-c", "import sys;print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
    let script = "import os,signal,sys,time\nsignal.signal(signal.SIGTERM,signal.SIG_IGN)\npid=os.fork()\nend=time.monotonic()+6\nif pid == 0:\n while time.monotonic()<end:\n  open(sys.argv[1],'w').write(str(time.time_ns()))\n  time.sleep(.03)\nelse:\n while time.monotonic()<end:time.sleep(.03)\n";
    f.config.commands.insert(
        "lint".into(),
        CheckCommand {
            argv: vec![
                python,
                "-c".into(),
                script.into(),
                heartbeat.display().to_string(),
            ],
            timeout_seconds: 1,
            report: None,
        },
    );
    let mut sentinel = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let result = f.adapter().check("red", &["lint".into()], &BTreeMap::new());
    let alive = sentinel.try_wait().unwrap().is_none();
    let _ = sentinel.kill();
    let _ = sentinel.wait();
    assert!(
        alive,
        "check cleanup must leave unrelated processes running"
    );
    assert_eq!(result.unwrap_err().kind, BlockKind::Transient);
    let before = fs::read(&heartbeat).expect("owned descendant was active");
    std::thread::sleep(std::time::Duration::from_millis(180));
    assert_eq!(
        fs::read(&heartbeat).unwrap(),
        before,
        "all owned tools must settle before timeout returns"
    );
}
