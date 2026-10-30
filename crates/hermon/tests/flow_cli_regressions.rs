//! Independent supplemental Slice 8 review regressions; original freeze remains unchanged.
#![cfg(unix)]

use hermon_flow::{hub::*, workflow::Workflow};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const RUN: &str = "TICKET-123";
const WORKFLOW: &str = "version: 1\nname: cli-fixture\nsteps:\n  work:\n    bot: worker\n    next: owner_go\n  owner_go:\n    gate: owner_go\n    next: finish\n  finish:\n    bot: reviewer\n    next: merge\n  merge:\n    gate: owner_merge\n";
const SECRET: &str = "synthetic-central-token-123";

struct Server {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

#[test]
fn doctor_rejects_required_workflow_command_missing_from_project() {
    let mut fixture = Fixture::new();
    fs::write(
        &fixture.workflow,
        WORKFLOW.replace("bot: worker\n", "bot: worker\n    check: [project_check]\n"),
    )
    .unwrap();
    fixture.config["commands"] = json!({});
    fixture.save_config();
    let out = fixture.doctor();
    assert!(
        !out.status.success(),
        "doctor passed without a command required by the workflow: {}",
        text(&out)
    );
    assert!(text(&out).contains("project_check"), "{}", text(&out));
    assert!(
        fixture
            .server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| { r["query"].as_str().unwrap().contains("viewer") }),
        "doctor still checks tracker auth while reporting missing workflow commands"
    );
    let out = fixture.intake();
    assert!(
        !out.status.success(),
        "intake accepted a workflow with missing project capabilities"
    );
    assert!(!fixture.run_dir().exists());
    assert!(git(&fixture.repo, &["branch", "--list", "flow/*"]).is_empty());
}

#[test]
fn doctor_checks_configured_code_host_repository_access() {
    let fixture = Fixture::new();
    script(
        &fixture.root.join("bin/gh"),
        "#!/bin/sh\nset -eu\nprintf 'gh:%s\\n' \"$*\" >> \"$PROBE_LOG\"\ncase \"$1\" in\n  auth) exit 0 ;;\n  repo|api) exit 7 ;;\n  *) exit 8 ;;\nesac\n",
    );
    let out = fixture.doctor();
    assert!(
        !out.status.success(),
        "authenticated gh cannot access the configured repository: {}",
        text(&out)
    );
    assert!(
        text(&out).contains("fixture/project"),
        "doctor must name the inaccessible repository: {}",
        text(&out)
    );
    assert!(fixture.probes().contains("gh:auth status"));
}

#[test]
fn status_replays_committed_control_when_snapshot_rename_was_lost() {
    let fixture = Fixture::new();
    fixture.start();
    let old = fs::read(fixture.run_dir().join("run.json")).unwrap();
    fixture.success(&["pause", RUN]);
    let count = fixture.events().len();
    fs::write(fixture.run_dir().join("run.json"), old).unwrap();
    let out = fixture.control(&["status", RUN, "--json"]);
    assert!(out.status.success(), "{}", text(&out));
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["runs"][0]["paused"], true);
    assert_eq!(fixture.record()["paused"], true);
    assert_eq!(fixture.events().len(), count, "replay duplicated an event");
}

#[test]
fn status_discards_torn_tail_without_losing_committed_control() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.success(&["pause", RUN]);
    let events = fixture.run_dir().join("events.jsonl");
    let committed = fs::read(&events).unwrap();
    let mut torn = committed.clone();
    torn.extend_from_slice(b"{\"schema\":1,\"kind\":\"resume\"");
    fs::write(&events, torn).unwrap();
    let out = fixture.control(&["status", RUN, "--json"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fixture.record()["paused"], true);
    assert_eq!(fs::read(events).unwrap(), committed);
}

#[test]
fn failed_answer_mirror_is_durable_and_retried_without_reapplying_answer() {
    let mut fixture = Fixture::new();
    fixture.start();
    fixture.state(
        Status::Blocked,
        "work",
        Some(Block::new(BlockKind::OwnerQuestion, "owner question")),
    );
    let mut record = fixture.record();
    record["run"]["owner_question"] = serde_json::to_value(OwnerQuestion {
        step: "work".into(),
        request: 1,
        bot: "worker".into(),
        questions: vec!["Which?".into()],
        previous_handle: "settled".into(),
        answer: None,
    })
    .unwrap();
    fs::write(
        fixture.run_dir().join("run.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let endpoint = fixture.config["tracker"]["endpoint"].clone();
    fixture.config["tracker"]["endpoint"] = json!("http://127.0.0.1:9/graphql");
    fixture.save_config();
    let out = fixture.control(&["answer", RUN, "Choose blue"]);
    assert!(
        !out.status.success(),
        "the mirror cannot have been delivered during the outage"
    );
    assert_eq!(
        fixture.record()["run"]["owner_question"]["answer"],
        "Choose blue"
    );
    assert_eq!(fixture.record()["notices"][0]["delivered"], false);
    fixture.config["tracker"]["endpoint"] = endpoint;
    fixture.save_config();
    fixture.success(&["pause", RUN]);
    assert_eq!(fixture.record()["notices"][0]["delivered"], true);
    assert_eq!(
        fixture
            .events()
            .iter()
            .filter(|e| e["kind"] == "answer")
            .count(),
        1
    );
}

#[test]
fn cancel_records_intent_without_stopping_a_foreign_attempt_handle() {
    let fixture = Fixture::new();
    fixture.start();
    let foreign = fixture.root.join("foreign-attempt");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("sentinel"), "leave this worker alone").unwrap();
    let mut record = fixture.record();
    record["run"]["status"] = json!("Running");
    record["run"]["active"] = serde_json::to_value(Attempt {
        step: "work".into(),
        number: 1,
        handle: foreign.to_str().unwrap().into(),
        started_at: 0,
        family: "fixture".into(),
        freezes: Default::default(),
    })
    .unwrap();
    fs::write(
        fixture.run_dir().join("run.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let out = fixture.control(&["cancel", RUN]);
    assert!(
        !out.status.success(),
        "foreign handle cannot be settled as an owned attempt"
    );
    assert_eq!(
        fixture.record()["cancelled"],
        true,
        "cancellation intent is durable even if settlement fails"
    );
    assert_eq!(fixture.record()["run"]["unsettled"], true);
    assert!(
        !foreign.join("cancel").exists(),
        "cancel wrote to a foreign worker handle"
    );
    assert_eq!(
        fs::read_to_string(foreign.join("sentinel")).unwrap(),
        "leave this worker alone"
    );
}

#[derive(Default)]
struct CoreAgent {
    starts: usize,
    polls: usize,
}
impl Agent for CoreAgent {
    fn family(&self, _: &str) -> String {
        "fixture".into()
    }
    fn available(&self, _: &str) -> bool {
        true
    }
    fn start(&mut self, _: &StartRequest) -> Result<String, Block> {
        self.starts += 1;
        Ok("owned-handle".into())
    }
    fn poll(&mut self, _: &str) -> Result<Poll, Block> {
        self.polls += 1;
        Ok(Poll::Finished {
            process_success: true, completion: true,
            result: Some(json!({"schema":1,"status":"done","summary":"late result","artifacts":[],"block":null,"verdict":null,"questions":[]}).to_string()),
        })
    }
    fn stop(&mut self, _: &str) -> Result<Settlement, Block> {
        Ok(Settlement::Unsettled)
    }
}
struct CoreTracker;
impl Tracker for CoreTracker {
    fn gate_approved(&mut self, _: &str, _: &str, _: u32) -> Result<bool, Block> {
        Ok(false)
    }
    fn alert(&mut self, _: &str, _: &Alert) -> anyhow::Result<()> {
        Ok(())
    }
}
struct CoreHost;
impl CodeHost for CoreHost {
    fn open_pr(&mut self, _: &str) -> Result<String, Block> {
        panic!("unexpected PR publication")
    }
    fn merged(&mut self, _: &str) -> Result<bool, Block> {
        panic!("unexpected merge check")
    }
}
struct CoreGit;
impl Git for CoreGit {
    fn check(
        &mut self,
        _: &str,
        _: &[String],
        _: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), Block> {
        panic!("cancelled result must not be accepted")
    }
    fn evidence(&mut self, _: &str, _: &str, _: u32) -> Result<String, Block> {
        panic!("cancelled result must not publish evidence")
    }
}
struct CoreClock;
impl Clock for CoreClock {
    fn now(&self) -> u64 {
        10
    }
}
fn advance_core(run: &mut Run, agent: &mut CoreAgent) {
    let workflow = Workflow::from_str("fixture", WORKFLOW).unwrap();
    let mut tracker = CoreTracker;
    let mut host = CoreHost;
    let mut git = CoreGit;
    hermon_flow::hub::advance(
        run,
        &workflow,
        &mut Dependencies {
            agent,
            tracker: &mut tracker,
            code_host: &mut host,
            git: &mut git,
            clock: &CoreClock,
        },
    )
    .unwrap();
}

#[test]
fn paused_core_does_not_dispatch_a_new_worker() {
    let mut run = Run::new(RUN, "work");
    run.paused = true;
    let mut agent = CoreAgent::default();
    advance_core(&mut run, &mut agent);
    assert_eq!(agent.starts, 0);
    assert_eq!(run.current, "work");
    assert_eq!(run.status, Status::Ready);
}

#[test]
fn cancelled_core_reconciles_owned_worker_settlement_without_accepting_late_success() {
    let mut run = Run::new(RUN, "work");
    run.cancelled = true;
    run.status = Status::Blocked;
    run.block = Some(Block::new(
        BlockKind::Capability,
        "Owner cancelled run in Hermon",
    ));
    run.unsettled = true;
    run.active = Some(Attempt {
        step: "work".into(),
        number: 1,
        handle: "owned-handle".into(),
        started_at: 0,
        family: "fixture".into(),
        freezes: Default::default(),
    });
    let mut agent = CoreAgent::default();
    advance_core(&mut run, &mut agent);
    assert_eq!(
        agent.polls, 1,
        "cancelled handles still need settlement reconciliation"
    );
    assert!(run.active.is_none());
    assert!(!run.unsettled);
    assert!(run.cancelled);
    assert_eq!(run.status, Status::Blocked);
    assert_eq!(run.current, "work");
    assert_eq!(agent.starts, 0);
    assert!(!run.events.iter().any(|e| e.kind == "step_done"));
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/graphql", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(vec![]));
        let stop = Arc::new(AtomicBool::new(false));
        let calls = requests.clone();
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => Self::serve(stream, &calls),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fake tracker accept: {error}"),
                }
            }
        });
        Self {
            endpoint,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn serve(mut stream: TcpStream, calls: &Mutex<Vec<Value>>) {
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = vec![];
        let mut chunk = [0; 4096];
        let (header, length) = loop {
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0, "incomplete HTTP request");
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(index) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                let length = String::from_utf8_lossy(&bytes[..index])
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                            .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                    })
                    .expect("GraphQL JSON content length");
                break (index + 4, length);
            }
        };
        while bytes.len() < header + length {
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&chunk[..count]);
        }
        let request: Value = serde_json::from_slice(&bytes[header..header + length]).unwrap();
        calls.lock().unwrap().push(request.clone());
        let query = request["query"].as_str().unwrap();
        let response = if query.contains("commentCreate") {
            let input = &request["variables"]["input"];
            json!({"data":{"commentCreate":{"success":true,"comment":{"id":input["id"],"body":input["body"]}}}})
        } else if query.contains("comments") {
            json!({"data":{"issue":{"id":"issue-123","comments":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}})
        } else if query.contains("viewer") {
            json!({"data":{"viewer":{"id":"central-app-123","name":"Synthetic app"}}})
        } else if query.contains("issue(") {
            json!({"data":{"issue":{"id":"issue-123","title":"Fixture issue","description":"Implement the fixture ticket."}}})
        } else {
            panic!("unexpected tracker query: {query}")
        };
        let body = serde_json::to_string(&response).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    server: Server,
    root: PathBuf,
    repo: PathBuf,
    data: PathBuf,
    workflow: PathBuf,
    project: PathBuf,
    config: Value,
}
fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out));
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
fn script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("fixture with spaces");
        let repo = root.join("repo");
        let bin = root.join("bin");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&bin).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.name", "CLI fixture"]);
        git(&repo, &["config", "user.email", "fixture@example.invalid"]);
        fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "fixture base"]);
        let workflow = root.join("workflow.yaml");
        fs::write(&workflow, WORKFLOW).unwrap();
        let server = Server::new();
        script(
            &bin.join("probe-bot"),
            "#!/bin/sh\nset -eu\n[ \"${HUB_ONLY:-}\" = yes ]\n[ -z \"${HOST_ONLY_123:-}\" ]\nprintf 'bot:%s\\n' \"$1\" >> \"$PROBE_LOG\"\n[ -s \"$1\" ]\n[ \"${BOT_FAIL:-}\" != yes ] || exit 9\n[ \"${BOT_NO_COMPLETION:-}\" != yes ] || exit 0\nprintf '%s' '{\"schema\":1,\"status\":\"done\",\"summary\":\"doctor probe complete\",\"artifacts\":[],\"block\":null,\"verdict\":null,\"questions\":[]}' > \"${1%/*}/result.json\"\n",
        );
        script(
            &bin.join("project-check"),
            "#!/bin/sh\nset -eu\n[ \"$HUB_ONLY\" = yes ]\n[ -z \"${HOST_ONLY_123:-}\" ]\nprintf 'command:%s\\n' \"$PWD\" >> \"$PROBE_LOG\"\n[ \"${CHECK_FAIL:-}\" != yes ]\n",
        );
        script(
            &bin.join("gh"),
            "#!/bin/sh\nset -eu\nprintf 'gh:%s\\n' \"$*\" >> \"$PROBE_LOG\"\n[ \"${GH_AUTH_FAIL:-}\" != yes ]\n",
        );
        let bot = json!({"Command":{"start":["probe-bot","{task}","{workdir}"],"resume":null,"task_stdin":false,"protocol":"ResultFile","family":"fixture"}});
        let config = json!({
            "repo":repo,"worktree_root":root.join("worktrees"),"branch_prefix":"flow",
            "base":"main","start":"work",
            "env":{"PATH":format!("{}:/usr/bin:/bin",bin.display()),"HUB_ONLY":"yes","PROBE_LOG":root.join("probes.log")},
            "bots":{"worker":bot,"reviewer":bot},
            "commands":{"project_check":{"argv":["project-check"],"timeout_seconds":3}},
            "tracker":{"endpoint":server.endpoint,"token_env":"HERMON_TEST_LINEAR_TOKEN","owner_ids":["owner-123"],"owner_mention":"[Owner](https://linear.app/fixture/profiles/owner-123)"},
            "code_host":{"repo":"fixture/project","remote":"origin"}
        });
        let project = root.join("project.json");
        let data = root.join("data");
        let fixture = Self {
            _temp: temp,
            server,
            root,
            repo,
            data,
            workflow,
            project,
            config,
        };
        fixture.save_config();
        fixture
    }
    fn save_config(&self) {
        fs::write(
            &self.project,
            serde_json::to_vec_pretty(&self.config).unwrap(),
        )
        .unwrap();
    }
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hermon"))
            .args(args)
            .env("HERMON_TEST_LINEAR_TOKEN", SECRET)
            .env("HOST_ONLY_123", "must-not-leak-into-hub")
            .current_dir(&self.root)
            .output()
            .unwrap()
    }
    fn doctor(&self) -> Output {
        self.cli(&[
            "doctor",
            self.workflow.to_str().unwrap(),
            "--project",
            self.project.to_str().unwrap(),
            "--ticket",
            "TICKET-123",
            "--json",
        ])
    }
    fn intake(&self) -> Output {
        self.cli(&[
            "run",
            self.workflow.to_str().unwrap(),
            "TICKET-123",
            "--project",
            self.project.to_str().unwrap(),
            "--data-dir",
            self.data.to_str().unwrap(),
            "--confirm",
            "--run-id",
            RUN,
        ])
    }
    fn start(&self) {
        let out = self.intake();
        assert!(out.status.success(), "intake must succeed: {}", text(&out));
    }
    fn run_dir(&self) -> PathBuf {
        self.data.join("runs").join(RUN)
    }
    fn record(&self) -> Value {
        serde_json::from_slice(&fs::read(self.run_dir().join("run.json")).unwrap()).unwrap()
    }
    fn state(&self, status: Status, step: &str, block: Option<Block>) {
        let mut record = self.record();
        let mut run: hermon_flow::hub::Run = serde_json::from_value(record["run"].clone()).unwrap();
        run.current = step.into();
        run.status = status;
        run.block = block;
        run.active = None;
        run.unsettled = false;
        record["run"] = serde_json::to_value(run).unwrap();
        fs::write(
            self.run_dir().join("run.json"),
            serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
    }
    fn control(&self, args: &[&str]) -> Output {
        let mut args = args.to_vec();
        args.extend(["--data-dir", self.data.to_str().unwrap()]);
        self.cli(&args)
    }
    fn success(&self, args: &[&str]) {
        let out = self.control(args);
        assert!(out.status.success(), "{args:?}: {}", text(&out));
    }
    fn events(&self) -> Vec<Value> {
        fs::read_to_string(self.run_dir().join("events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn probes(&self) -> String {
        fs::read_to_string(self.root.join("probes.log")).unwrap_or_default()
    }
}
