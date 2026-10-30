//! Synthetic CLI fixture for Vitest and accepted-plan expectation checks.
#![allow(dead_code)]
#![cfg(unix)]

use hermon_flow::hub::{Block, Status};
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

/// Execute only fake agents, commands and a loopback tracker. No live vendor data.
pub fn exercise(report: Value, status: i32, expectations: Value) -> Value {
    exercise_inner(report, status, expectations, false)
}

pub fn exercise_with_unaccepted_plan(report: Value, status: i32, expectations: Value) -> Value {
    exercise_inner(report, status, expectations, true)
}

fn exercise_inner(report: Value, status: i32, expectations: Value, poison: bool) -> Value {
    let mut fixture = Fixture::new();
    fs::write(&fixture.workflow, "version: 1\nname: vitest-fixture\nsteps:\n  plan:\n    bot: worker\n    produces: [test-expectations.json]\n    next: red\n  red:\n    bot: worker\n    check: [tests_fail]\n    tag: red\n    next: owner_go\n  owner_go:\n    gate: owner_go\n").unwrap();
    let script_body = r##"#!/bin/sh
set -eu
if /usr/bin/grep -q '^Doctor capability' "$1"; then
  printf '%s' '{"schema":1,"status":"done","summary":"ready","artifacts":[],"block":null,"verdict":null,"questions":[]}' > "${1%/*}/result.json"
elif /usr/bin/grep -q '^# .* / plan' "$1"; then
  /bin/cp "$EXPECTATIONS" "${1%/*}/test-expectations.json"
  printf '%s' '{"schema":1,"status":"done","summary":"plan accepted","artifacts":["test-expectations.json"],"block":null,"verdict":null,"questions":[]}' > "${1%/*}/result.json"
else
  printf 'new test\n' > red.test.js
  /usr/bin/git add red.test.js
  /usr/bin/git -c user.name=Fixture -c user.email=fixture@example.invalid commit -m red
  printf '%s' '{"schema":1,"status":"done","summary":"red ready","artifacts":[],"block":null,"verdict":null,"questions":[]}' > "${1%/*}/result.json"
fi
"##;
    script(&fixture.root.join("bin/probe-bot"), script_body);
    let report_path = fixture.root.join("report.json");
    let expectations_path = fixture.root.join("expected.json");
    fs::write(&report_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    fs::write(
        &expectations_path,
        serde_json::to_vec(&expectations).unwrap(),
    )
    .unwrap();
    fixture.config["env"]["EXPECTATIONS"] = json!(expectations_path);
    fixture.config["env"]["REPORT"] = json!(report_path);
    fixture.config["env"]["TEST_EXIT"] = json!(status.to_string());
    fixture.config["start"] = json!("plan");
    fixture.config["commands"]["test"] = json!({
        "argv":["/bin/sh","-c",r#"if [ -f red.test.js ]; then /bin/cat "$REPORT"; exit "$TEST_EXIT"; fi; printf '%s\n' '{"numPassedTests":1,"numFailedTests":0,"numPendingTests":0,"numTotalTests":1,"testResults":[{"assertionResults":[{"fullName":"old passed","status":"passed"}]}]}'"#],
        "timeout_seconds":3,"report":"VitestJson"
    });
    fixture.save_config();
    fixture.start();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let out = fixture.cli(&[
            "hub",
            "--once",
            "--no-notify",
            "--data-dir",
            fixture.data.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "host: {}", text(&out));
        let mut record = fixture.record();
        if poison
            && record["run"]["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["step"] == "plan" && event["kind"] == "step_done")
        {
            // A newer directory is not proof that its artifact was accepted.
            let unaccepted = fixture.run_dir().join("steps/plan/999");
            fs::create_dir_all(&unaccepted).unwrap();
            fs::write(
                unaccepted.join("test-expectations.json"),
                r#"{"new_tests":["wrong unaccepted expectation"],"baseline_failures":[]}"#,
            )
            .unwrap();
        }
        let state = record["run"]["status"].as_str().unwrap();
        if state == "Gate" || state == "Blocked" {
            if let Some(tag) = record["run"]["freezes"]["red"].as_str() {
                let worktree = PathBuf::from(record["worktree"].as_str().unwrap());
                record["fixture_evidence"] =
                    serde_json::from_str(&git(&worktree, &["notes", "--ref=hermon", "show", tag]))
                        .unwrap();
            }
            return record;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture stalled: {record}"
        );
        thread::sleep(Duration::from_millis(30));
    }
}

/// A normal assertion failure, without suite/runtime errors.
pub fn valid_report() -> Value {
    json!({"numPassedTests":1,"numFailedTests":1,"numPendingTests":0,"numTotalTests":2,
    "numRuntimeErrorTestSuites":0,"success":false,
    "testResults":[{"name":"red.test.js","status":"failed","message":"","assertionResults":[
        {"fullName":"suite new case","title":"new case","status":"failed"},
        {"fullName":"suite old case","status":"passed"}
    ]}]})
}
