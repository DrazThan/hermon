//! Independent Slice 8 CLI acceptance, frozen before production implementation.
#![cfg(unix)]

use hermon_flow::hub::{Block, BlockKind, OwnerQuestion, Status};
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

#[test]
fn intake_refuses_on_doctor_failure() {
    let mut fixture = Fixture::new();
    fixture.config["env"]["BOT_FAIL"] = json!("yes");
    fixture.save_config();
    let out = fixture.intake();
    assert!(!out.status.success(), "failed probe must reject intake");
    assert!(text(&out).contains("worker"), "{}", text(&out));
    assert!(!fixture.run_dir().exists(), "doctor failure created a run");
    assert!(
        !git(&fixture.repo, &["branch", "--list", "flow/*"]).contains(RUN),
        "doctor failure created a branch"
    );
    assert!(
        !fixture.root.join("worktrees").join(RUN).exists(),
        "doctor failure created a worktree"
    );
    assert!(
        fixture
            .server
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| { !r["query"].as_str().unwrap().contains("commentCreate") }),
        "failed intake posted a started comment"
    );
}

#[test]
fn doctor_reports_missing_bot() {
    let mut fixture = Fixture::new();
    fixture.config["bots"]
        .as_object_mut()
        .unwrap()
        .remove("worker");
    fixture.save_config();
    let out = fixture.doctor();
    assert!(!out.status.success());
    assert!(text(&out).contains("worker"), "{}", text(&out));
    assert!(
        text(&out).to_lowercase().contains("bot"),
        "name the missing bot and a fix: {}",
        text(&out)
    );
}

#[test]
fn doctor_reports_command_not_on_path() {
    let mut fixture = Fixture::new();
    fixture.config["commands"]["project_check"]["argv"] = json!(["missing-hermon-check-123"]);
    fixture.save_config();
    let out = fixture.doctor();
    assert!(!out.status.success());
    assert!(
        text(&out).contains("missing-hermon-check-123"),
        "{}",
        text(&out)
    );
    assert!(text(&out).contains("PATH"), "{}", text(&out));
}

#[test]
fn doctor_probes_every_bot_auth_and_project_commands_under_hub_env() {
    let fixture = Fixture::new();
    let out = fixture.doctor();
    assert!(out.status.success(), "{}", text(&out));
    let report: Value = serde_json::from_slice(&out.stdout).expect("doctor JSON on stdout");
    assert!(report.is_object());
    let probes = fixture.probes();
    assert_eq!(probes.lines().filter(|l| l.starts_with("bot:")).count(), 2);
    assert!(probes.contains("command:"), "{probes}");
    assert!(probes.contains("gh:auth status"), "{probes}");
    assert!(
        fixture
            .server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| { r["query"].as_str().unwrap().contains("viewer") }),
        "tracker auth must actually be checked"
    );
    assert!(!fixture.data.exists(), "doctor created durable run state");
}

#[test]
fn doctor_rejects_failed_command_and_code_host_auth() {
    let mut fixture = Fixture::new();
    fixture.config["env"]["CHECK_FAIL"] = json!("yes");
    fixture.save_config();
    let out = fixture.doctor();
    assert!(!out.status.success(), "project command failed");
    assert!(text(&out).contains("project_check"), "{}", text(&out));
    fixture.config["env"]
        .as_object_mut()
        .unwrap()
        .remove("CHECK_FAIL");
    fixture.config["env"]["GH_AUTH_FAIL"] = json!("yes");
    fixture.save_config();
    let out = fixture.doctor();
    assert!(!out.status.success(), "gh auth failure must fail doctor");
    assert!(text(&out).contains("gh"), "{}", text(&out));
}

#[test]
fn doctor_requires_bot_completion_not_just_exit_zero() {
    let mut fixture = Fixture::new();
    fixture.config["env"]["BOT_NO_COMPLETION"] = json!("yes");
    fixture.save_config();
    let out = fixture.doctor();
    assert!(
        !out.status.success(),
        "zero exit without result is not a successful bot probe"
    );
    assert!(text(&out).contains("worker"), "{}", text(&out));
}

#[test]
fn intake_snapshots_workflow_and_templates_without_credentials() {
    let fixture = Fixture::new();
    fixture.start();
    let record = fixture.record();
    assert_eq!(record["schema"], 1);
    assert_eq!(record["ticket"], "TICKET-123");
    assert_eq!(record["workflow_name"], "cli-fixture");
    assert_eq!(record["workflow_source"], WORKFLOW);
    assert_eq!(record["workflow_hash"].as_str().unwrap().len(), 64);
    let hashes = record["template_hashes"].as_object().unwrap();
    assert_eq!(hashes.len(), 7);
    assert!(hashes.values().all(|v| v.as_str().unwrap().len() == 64));
    assert_eq!(record["base"], "main");
    assert_eq!(record["branch"], "flow/TICKET-123");
    let worktree = Path::new(record["worktree"].as_str().unwrap());
    assert!(worktree.is_dir());
    assert_eq!(
        git(worktree, &["branch", "--show-current"]),
        "flow/TICKET-123"
    );
    assert!(
        fs::read_to_string(fixture.run_dir().join("context/issue.md"))
            .unwrap()
            .contains("Fixture issue")
    );
    for path in [
        fixture.run_dir().join("run.json"),
        fixture.run_dir().join("events.jsonl"),
    ] {
        assert!(!fs::read_to_string(path).unwrap().contains(SECRET));
    }
    assert!(
        fixture.server.requests.lock().unwrap().iter().any(|r| {
            r["query"].as_str().unwrap().contains("commentCreate")
                && r["variables"]["input"]["body"]
                    .as_str()
                    .unwrap()
                    .contains("Started")
        }),
        "successful intake posts started through the central tracker"
    );
    let before = fs::read(fixture.run_dir().join("run.json")).unwrap();
    assert!(
        !fixture.intake().status.success(),
        "duplicate run IDs must be rejected"
    );
    assert_eq!(
        fs::read(fixture.run_dir().join("run.json")).unwrap(),
        before
    );
}

#[test]
fn cli_status_json_schema() {
    let fixture = Fixture::new();
    fixture.start();
    let out = fixture.control(&["status", RUN, "--json"]);
    assert!(out.status.success(), "{}", text(&out));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["schema"], 1);
    assert_eq!(value["runs"].as_array().unwrap().len(), 1);
    let run = &value["runs"][0];
    assert_eq!(run["run"]["id"], RUN);
    assert_eq!(run["run"]["current"], "work");
    assert_eq!(run["paused"], false);
    assert_eq!(run["cancelled"], false);
    let all = fixture.control(&["status", "--json"]);
    assert!(all.status.success(), "{}", text(&all));
    let all: Value = serde_json::from_slice(&all.stdout).unwrap();
    assert_eq!(all["runs"].as_array().unwrap().len(), 1);
    assert!(
        !fixture
            .control(&["status", "NO-SUCH-RUN", "--json"])
            .status
            .success()
    );
    assert!(!fixture.data.join("runs/NO-SUCH-RUN").exists());
}

#[test]
fn cli_controls_append_events() {
    let fixture = Fixture::new();
    fixture.start();
    let initial = fixture.events().len();
    fixture.success(&["pause", RUN]);
    assert_eq!(fixture.record()["paused"], true);
    fixture.success(&["resume", RUN]);
    assert_eq!(fixture.record()["paused"], false);

    fixture.state(
        Status::Blocked,
        "work",
        Some(Block::new(BlockKind::Capability, "missing tool")),
    );
    fixture.success(&["retry", RUN, "work"]);
    assert_eq!(fixture.record()["run"]["status"], "Ready");
    fixture.state(
        Status::Blocked,
        "work",
        Some(Block::new(BlockKind::Unknown, "owner waiver")),
    );
    fixture.success(&[
        "override",
        RUN,
        "work",
        "--reason",
        "Owner accepts fixture risk",
    ]);
    assert_eq!(fixture.record()["run"]["current"], "owner_go");

    fixture.state(Status::Gate, "owner_go", None);
    let mut record = fixture.record();
    record["run"]["gate_requests"]["owner_go"] = json!(1);
    fs::write(
        fixture.run_dir().join("run.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    fixture.success(&["approve", RUN]);
    assert_eq!(fixture.record()["run"]["current"], "finish");

    fixture.state(
        Status::Blocked,
        "finish",
        Some(Block::new(BlockKind::OwnerQuestion, "choose option")),
    );
    let mut record = fixture.record();
    record["run"]["owner_question"] = serde_json::to_value(OwnerQuestion {
        step: "finish".into(),
        request: 1,
        bot: "reviewer".into(),
        questions: vec!["Which option?".into()],
        previous_handle: "settled-fixture-handle".into(),
        answer: None,
    })
    .unwrap();
    fs::write(
        fixture.run_dir().join("run.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    fixture.success(&["answer", RUN, "Use option B"]);
    assert_eq!(
        fixture.record()["run"]["owner_question"]["answer"],
        "Use option B"
    );
    assert_eq!(fixture.record()["run"]["status"], "Ready");

    fixture.success(&["cancel", RUN]);
    assert_eq!(fixture.record()["cancelled"], true);
    let events = fixture.events();
    assert!(events.len() >= initial + 7);
    for kind in [
        "pause", "resume", "retry", "override", "approve", "answer", "cancel",
    ] {
        assert!(
            events[initial..].iter().any(|e| e["kind"] == kind),
            "missing {kind}: {events:?}"
        );
    }
    for event in events {
        assert_eq!(event["schema"], 1);
        assert_eq!(event["run_id"], RUN);
        assert!(event["sequence"].as_u64().is_some());
    }
    assert!(
        fixture.server.requests.lock().unwrap().iter().any(|r| {
            r["query"].as_str().unwrap().contains("commentCreate")
                && r["variables"]["input"]["body"]
                    .as_str()
                    .unwrap()
                    .contains("Hermon")
        }),
        "CLI answers and approvals must be mirrored to the ticket as entered in Hermon"
    );
}

#[test]
fn cli_controls_reject_stale_targets_and_merge_bypass() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.state(
        Status::Blocked,
        "work",
        Some(Block::new(BlockKind::Capability, "blocked")),
    );
    let before = fs::read(fixture.run_dir().join("run.json")).unwrap();
    let events = fs::read(fixture.run_dir().join("events.jsonl")).unwrap();
    for args in [
        vec!["retry", RUN, "finish"],
        vec!["override", RUN, "missing", "--reason", "waive"],
        vec!["override", RUN, "work", "--reason", "   "],
        vec!["approve", RUN],
        vec!["answer", RUN, "not answering a question"],
    ] {
        let out = fixture.control(&args);
        assert!(!out.status.success(), "invalid control succeeded: {args:?}");
        assert_eq!(
            fs::read(fixture.run_dir().join("run.json")).unwrap(),
            before
        );
        assert_eq!(
            fs::read(fixture.run_dir().join("events.jsonl")).unwrap(),
            events
        );
    }
    fixture.state(Status::Gate, "merge", None);
    let out = fixture.control(&["approve", RUN]);
    assert!(
        !out.status.success(),
        "CLI approval cannot stand in for verified owner merge"
    );
    assert_eq!(fixture.record()["run"]["current"], "merge");
    assert_eq!(fixture.record()["run"]["status"], "Gate");
    fixture.success(&["cancel", RUN]);
    for args in [
        vec!["resume", RUN],
        vec!["retry", RUN, "merge"],
        vec!["approve", RUN],
    ] {
        assert!(
            !fixture.control(&args).status.success(),
            "cancelled run was resurrected: {args:?}"
        );
    }
}

fn durable(fixture: &Fixture) -> (Vec<u8>, Vec<u8>) {
    (
        fs::read(fixture.run_dir().join("run.json")).unwrap(),
        fs::read(fixture.run_dir().join("events.jsonl")).unwrap(),
    )
}
fn assert_durable(fixture: &Fixture, before: &(Vec<u8>, Vec<u8>)) {
    let after = durable(fixture);
    assert_eq!(after.0, before.0, "run.json changed");
    assert_eq!(after.1, before.1, "events.jsonl changed");
}
fn worktree_of(fixture: &Fixture) -> PathBuf {
    PathBuf::from(fixture.record()["worktree"].as_str().unwrap())
}

#[test]
fn reopen_continues_cancelled_run_at_current_step() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.state(
        Status::Blocked,
        "work",
        Some(Block::new(BlockKind::Capability, "missing tool")),
    );
    let before = fixture.record();
    let attempts = before["run"]["attempts"].clone();
    let freezes = before["run"]["freezes"].clone();
    let revisions = before["run"]["evidence_revisions"].clone();
    let runs = before["run"]["runs"].clone();
    let gates = before["run"]["gate_requests"].clone();
    let paused = before["paused"].clone();
    let run_paused = before["run"]["paused"].clone();
    let pr = before["run"]["pr"].clone();
    let question = before["run"]["owner_question"].clone();
    assert_eq!(before["run"]["current"], "work");
    fixture.success(&["cancel", RUN]);
    let out = fixture.control(&["reopen", RUN, "--reason", "MCP auth fixed"]);
    assert!(out.status.success(), "reopen: {}", text(&out));
    let record = fixture.record();
    assert_eq!(record["cancelled"], false, "{record}");
    assert_eq!(record["run"]["cancelled"], false, "{record}");
    assert_eq!(record["run"]["status"], "Blocked", "{record}");
    assert_eq!(record["run"]["current"], "work", "{record}");
    assert_eq!(record["run"]["block"]["kind"], "capability", "{record}");
    assert!(
        record["run"]["block"]["reason"]
            .as_str()
            .unwrap()
            .contains("reopened"),
        "{record}"
    );
    assert_eq!(record["run"]["attempts"], attempts, "{record}");
    assert_eq!(record["run"]["freezes"], freezes, "{record}");
    assert_eq!(record["run"]["evidence_revisions"], revisions, "{record}");
    assert_eq!(record["run"]["runs"], runs, "{record}");
    assert_eq!(record["run"]["gate_requests"], gates, "{record}");
    assert_eq!(record["paused"], paused, "{record}");
    assert_eq!(record["run"]["paused"], run_paused, "{record}");
    assert_eq!(record["run"]["pr"], pr, "{record}");
    assert_eq!(record["run"]["owner_question"], question, "{record}");
    let events = fixture.events();
    let last = events.last().expect("reopen journal event");
    assert_eq!(last["kind"], "reopen", "{events:?}");
    assert_eq!(last["step"], "work", "{events:?}");
    let reason = last["reason"].as_str().unwrap();
    assert!(
        reason.contains("MCP auth fixed"),
        "reopen reason must carry the owner text: {reason}"
    );
    fixture.success(&["retry", RUN, "work"]);
    assert_eq!(fixture.record()["run"]["status"], "Ready");
}

#[test]
fn reopen_preserves_pause() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.success(&["pause", RUN]);
    assert_eq!(fixture.record()["paused"], true);
    assert_eq!(fixture.record()["run"]["paused"], true);
    fixture.success(&["cancel", RUN]);
    let out = fixture.control(&["reopen", RUN, "--reason", "check the fix first"]);
    assert!(out.status.success(), "reopen: {}", text(&out));
    assert_eq!(fixture.record()["paused"], true, "reopen cleared pause");
    assert_eq!(
        fixture.record()["run"]["paused"],
        true,
        "reopen cleared run pause"
    );
    fixture.success(&["resume", RUN]);
    assert_eq!(fixture.record()["paused"], false);
    assert_eq!(fixture.record()["run"]["paused"], false);
}

#[test]
fn reopen_refuses_invalid_targets_without_writing() {
    let fixture = Fixture::new();
    fixture.start();
    let live_bytes = durable(&fixture);
    let live = fixture.control(&["reopen", RUN, "--reason", "still running"]);
    assert!(!live.status.success(), "reopen of a live run must fail");
    assert!(
        String::from_utf8_lossy(&live.stderr).contains("cancelled"),
        "live reopen must name the cancelled precondition: {}",
        text(&live)
    );
    assert_durable(&fixture, &live_bytes);

    fixture.success(&["cancel", RUN]);
    let cancelled_bytes = durable(&fixture);
    let blank = fixture.control(&["reopen", RUN, "--reason", "   "]);
    assert!(
        !blank.status.success(),
        "a whitespace-only reason must fail"
    );
    assert!(
        String::from_utf8_lossy(&blank.stderr).contains("reason"),
        "whitespace reason must be rejected as a reason: {}",
        text(&blank)
    );
    assert_durable(&fixture, &cancelled_bytes);

    let unknown = fixture.control(&["reopen", "NO-SUCH-RUN", "--reason", "missing"]);
    assert!(!unknown.status.success(), "an unknown run must fail");
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("unknown run"),
        "unknown run: {}",
        text(&unknown)
    );
    assert_durable(&fixture, &cancelled_bytes);
    assert!(!fixture.data.join("runs/NO-SUCH-RUN").exists());

    let unsettled = Fixture::new();
    unsettled.start();
    let foreign = unsettled.root.join("foreign-attempt");
    fs::create_dir_all(&foreign).unwrap();
    let mut record = unsettled.record();
    record["run"]["status"] = json!("Running");
    record["run"]["active"] = serde_json::to_value(hermon_flow::hub::Attempt {
        step: "work".into(),
        number: 1,
        handle: foreign.to_str().unwrap().into(),
        started_at: 0,
        family: "fixture".into(),
        freezes: Default::default(),
    })
    .unwrap();
    fs::write(
        unsettled.run_dir().join("run.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let cancel = unsettled.control(&["cancel", RUN]);
    assert!(
        !cancel.status.success(),
        "foreign handle cannot be settled as an owned attempt"
    );
    assert_eq!(unsettled.record()["cancelled"], true);
    assert_eq!(unsettled.record()["run"]["unsettled"], true);
    assert!(unsettled.record()["run"]["active"].is_object());
    let pending = durable(&unsettled);
    let out = unsettled.control(&["reopen", RUN, "--reason", "worker still running"]);
    assert!(
        !out.status.success(),
        "an unsettled cancelled run must be refused"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("settlement"),
        "unsettled reopen must tell the owner to let the hub confirm settlement: {stderr}"
    );
    assert_durable(&unsettled, &pending);
}

#[test]
fn reopen_refuses_worktree_off_accepted_evidence() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.state(
        Status::Blocked,
        "work",
        Some(Block::new(BlockKind::Capability, "blocked")),
    );
    fixture.success(&["cancel", RUN]);
    let record = fixture.record();
    assert!(
        !record["run"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "evidence"),
        "a run with no evidence anchors on base_commit"
    );
    let base = record["base_commit"].as_str().unwrap().to_owned();
    assert!(!base.is_empty(), "{record}");
    let worktree = worktree_of(&fixture);
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), base);
    let frozen = durable(&fixture);

    fs::write(worktree.join("untracked.txt"), "dirty\n").unwrap();
    let dirty = fixture.control(&["reopen", RUN, "--reason", "worktree restored"]);
    assert!(!dirty.status.success(), "an untracked file must be refused");
    assert_durable(&fixture, &frozen);
    fs::remove_file(worktree.join("untracked.txt")).unwrap();

    fs::write(worktree.join("extra.txt"), "extra commit\n").unwrap();
    git(&worktree, &["add", "extra.txt"]);
    git(&worktree, &["commit", "-m", "past base_commit"]);
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    assert_ne!(head, base);
    let committed = fixture.control(&["reopen", RUN, "--reason", "worktree restored"]);
    assert!(
        !committed.status.success(),
        "HEAD past base_commit must be refused"
    );
    let stderr = String::from_utf8_lossy(&committed.stderr);
    assert!(
        stderr.contains(&base) && stderr.contains(&head),
        "refused reopen must name base_commit {base} and HEAD {head}: {stderr}"
    );
    assert_durable(&fixture, &frozen);

    git(&worktree, &["reset", "--hard", &base]);
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), base);
    assert!(
        git(
            &worktree,
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .is_empty()
    );
    assert_eq!(
        git(&worktree, &["symbolic-ref", "--short", "HEAD"]),
        record["branch"].as_str().unwrap()
    );
    let restored = fixture.control(&["reopen", RUN, "--reason", "worktree restored"]);
    assert!(restored.status.success(), "reopen: {}", text(&restored));
    assert_eq!(fixture.record()["cancelled"], false);
    assert_eq!(fixture.record()["run"]["cancelled"], false);
}

#[test]
fn reopen_mirrors_owner_notice_to_tracker() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.success(&["cancel", RUN]);
    let out = fixture.control(&["reopen", RUN, "--reason", "MCP auth fixed"]);
    assert!(out.status.success(), "reopen: {}", text(&out));
    let requests = fixture.server.requests.lock().unwrap();
    assert!(
        requests.iter().any(|request| {
            request["query"]
                .as_str()
                .unwrap_or("")
                .contains("commentCreate")
                && request["variables"]["input"]["body"]
                    .as_str()
                    .unwrap_or("")
                    .contains("reopened")
                && request["variables"]["input"]["body"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Hermon")
        }),
        "owner notice was not mirrored: {requests:?}"
    );
}

#[test]
fn cancelled_control_error_names_reopen() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.state(
        Status::Blocked,
        "work",
        Some(Block::new(BlockKind::Capability, "blocked")),
    );
    fixture.success(&["cancel", RUN]);
    let out = fixture.control(&["retry", RUN, "work"]);
    assert!(!out.status.success(), "retry on a cancelled run must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("hermon reopen"),
        "cancelled control error must name hermon reopen: {stderr}"
    );
}

#[test]
fn answer_binds_to_questions_on_a_scope_change_block() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.state(
        Status::Blocked,
        "work",
        Some(Block::new(
            BlockKind::ScopeChange,
            "repair needs separate scope",
        )),
    );
    let mut record = fixture.record();
    record["run"]["owner_question"] = serde_json::to_value(OwnerQuestion {
        step: "work".into(),
        request: 1,
        bot: "worker".into(),
        questions: vec!["Elapsed time or calendar days?".into()],
        previous_handle: "settled-fixture-handle".into(),
        answer: None,
    })
    .unwrap();
    fs::write(
        fixture.run_dir().join("run.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let out = fixture.control(&["status", RUN]);
    assert!(out.status.success(), "{}", text(&out));
    let shown = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        shown.contains("Elapsed time or calendar days?"),
        "status must show the pending question: {shown}"
    );
    assert!(
        shown.contains(&format!("hermon answer {RUN}")),
        "status must name the supported reply command: {shown}"
    );
    fixture.success(&["answer", RUN, "Use elapsed 24-hour periods"]);
    let record = fixture.record();
    assert_eq!(
        record["run"]["owner_question"]["answer"],
        "Use elapsed 24-hour periods"
    );
    assert_eq!(record["run"]["status"], "Ready");
    assert_eq!(record["run"]["current"], "work");
    assert!(record["run"]["block"].is_null(), "{record}");
    assert_eq!(record["run"]["gate_requests"], json!({}), "{record}");
    assert!(
        fixture.events().iter().any(|e| e["kind"] == "answer"),
        "the answer must be journaled"
    );
}

#[test]
fn answer_is_refused_for_a_scope_change_block_without_questions() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.state(
        Status::Blocked,
        "work",
        Some(Block::new(
            BlockKind::ScopeChange,
            "repair needs separate scope",
        )),
    );
    let before = durable(&fixture);
    let out = fixture.control(&["answer", RUN, "Use elapsed 24-hour periods"]);
    assert!(!out.status.success(), "answer without a question succeeded");
    assert_durable(&fixture, &before);
}
