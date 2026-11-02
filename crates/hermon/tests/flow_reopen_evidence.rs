#![cfg(unix)]
//! Reopen must anchor on the peeled evidence-tag commit, not `base_commit` or the tag name.

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
    time::{Duration, Instant},
};

const RUN: &str = "TICKET-123";
const SECRET: &str = "synthetic-central-token-123";

#[allow(dead_code)]
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
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
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
    _server: Server,
    root: PathBuf,
    _repo: PathBuf,
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
        fs::write(
            &workflow,
            "version: 1\nname: cli-fixture\nsteps:\n  work:\n    bot: worker\n",
        )
        .unwrap();
        let server = Server::new();
        script(
            &bin.join("probe-bot"),
            "#!/bin/sh\nset -eu\nprintf '%s' '{\"schema\":1,\"status\":\"done\",\"summary\":\"doctor probe complete\",\"artifacts\":[],\"block\":null,\"verdict\":null,\"questions\":[]}' > \"${1%/*}/result.json\"\n",
        );
        script(
            &bin.join("project-check"),
            "#!/bin/sh\nset -eu\n[ \"$HUB_ONLY\" = yes ]\n",
        );
        script(
            &bin.join("gh"),
            "#!/bin/sh\nset -eu\n[ \"${GH_AUTH_FAIL:-}\" != yes ]\n",
        );
        let bot = json!({"Command":{"start":["probe-bot","{task}","{workdir}"],"resume":null,"task_stdin":false,"protocol":"ResultFile","family":"fixture"}});
        let config = json!({
            "repo":repo,"worktree_root":root.join("worktrees"),"branch_prefix":"flow",
            "base":"main","start":"plan",
            "env":{"PATH":format!("{}:/usr/bin:/bin",bin.display()),"HUB_ONLY":"yes","PROBE_LOG":root.join("probes.log")},
            "bots":{"worker":bot,"reviewer":bot.clone()},
            "commands":{"project_check":{"argv":["project-check"],"timeout_seconds":3}},
            "tracker":{"endpoint":server.endpoint,"token_env":"HERMON_TEST_LINEAR_TOKEN","owner_ids":["owner-123"],"owner_mention":"[Owner](https://linear.app/fixture/profiles/owner-123)"},
            "code_host":{"repo":"fixture/project","remote":"origin"}
        });
        let project = root.join("project.json");
        let data = root.join("data");
        let fixture = Self {
            _temp: temp,
            _server: server,
            root,
            _repo: repo,
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
    fn start(&self) {
        let out = self.cli(&[
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
        ]);
        assert!(out.status.success(), "intake must succeed: {}", text(&out));
    }
    fn run_dir(&self) -> PathBuf {
        self.data.join("runs").join(RUN)
    }
    fn record(&self) -> Value {
        serde_json::from_slice(&fs::read(self.run_dir().join("run.json")).unwrap()).unwrap()
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
fn hub_once(fixture: &Fixture) -> Value {
    let out = fixture.cli(&[
        "hub",
        "--once",
        "--no-notify",
        "--data-dir",
        fixture.data.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "host: {}", text(&out));
    fixture.record()
}
fn evidence_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fs::write(
        &fixture.workflow,
        "version: 1\nname: vitest-fixture\nsteps:\n  plan:\n    bot: worker\n    produces: [test-expectations.json]\n    next: characterize\n  characterize:\n    bot: worker\n    check: [tests_pass]\n    tag: characterize\n    next: owner_go\n  owner_go:\n    gate: owner_go\n",
    )
    .unwrap();
    script(
        &fixture.root.join("bin/probe-bot"),
        r#"#!/bin/sh
set -eu
if /usr/bin/grep -q '^Doctor capability' "$1"; then
  printf '%s' '{"schema":1,"status":"done","summary":"ready","artifacts":[],"block":null,"verdict":null,"questions":[]}' > "${1%/*}/result.json"
elif /usr/bin/grep -q '^# .* / plan' "$1"; then
  /bin/cp "$EXPECTATIONS" "${1%/*}/test-expectations.json"
  printf '%s' '{"schema":1,"status":"done","summary":"plan accepted","artifacts":["test-expectations.json"],"block":null,"verdict":null,"questions":[]}' > "${1%/*}/result.json"
else
  printf 'new test\n' > characterize.test.js
  /usr/bin/git add characterize.test.js
  /usr/bin/git -c user.name=Fixture -c user.email=fixture@example.invalid commit -m characterize
  printf '%s' '{"schema":1,"status":"done","summary":"characterize ready","artifacts":[],"block":null,"verdict":null,"questions":[]}' > "${1%/*}/result.json"
fi
"#,
    );
    let report = json!({"numPassedTests":2,"numFailedTests":0,"numPendingTests":1,"numTotalTests":3,
        "success":true,"testResults":[{"assertionResults":[
            {"fullName":"suite new case","status":"passed"},
            {"fullName":"baseline","status":"passed"},
            {"fullName":"old skipped","status":"pending"}]}]});
    let expectations = json!({"new_tests":["suite new case"],"baseline_failures":[]});
    let report_path = fixture.root.join("report.json");
    let expectations_path = fixture.root.join("expected.json");
    fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
    fs::write(
        &expectations_path,
        serde_json::to_vec(&expectations).unwrap(),
    )
    .unwrap();
    fixture.config["env"]["EXPECTATIONS"] = json!(expectations_path);
    fixture.config["env"]["REPORT"] = json!(report_path);
    fixture.config["env"]["TEST_EXIT"] = json!("0");
    fixture.config["commands"]["test"] = json!({
        "argv":["/bin/sh","-c",r#"if [ -f characterize.test.js ]; then /bin/cat "$REPORT"; exit "$TEST_EXIT"; fi; printf '%s\n' '{"numPassedTests":1,"numFailedTests":0,"numPendingTests":0,"numTotalTests":1,"testResults":[{"assertionResults":[{"fullName":"old passed","status":"passed"}]}]}'"#],
        "timeout_seconds":3,
        "report":"VitestJson"
    });
    fixture.save_config();
    fixture.start();
    fixture
}
fn drive_to_gate(fixture: &Fixture) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let record = hub_once(fixture);
        if record["run"]["status"] == "Gate" {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "fixture did not reach the owner_go gate: {record}"
        );
        thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn reopen_anchors_on_peeled_evidence_commit_and_preserves_evidence() {
    let fixture = evidence_fixture();
    let record = drive_to_gate(&fixture);
    assert_eq!(record["run"]["current"], "owner_go", "{record}");
    assert_eq!(record["run"]["gate_requests"]["owner_go"], 1, "{record}");
    let tag = record["run"]["freezes"]["characterize"]
        .as_str()
        .expect("characterize evidence tag")
        .to_owned();
    assert!(!tag.is_empty(), "{record}");
    let evidence_ref = record["run"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|event| event["kind"] == "evidence")
        .expect("evidence event")["reason"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        evidence_ref, tag,
        "the freeze and the latest evidence event must name the same ref"
    );
    let freezes = record["run"]["freezes"].clone();
    let revisions = record["run"]["evidence_revisions"].clone();
    let attempts = record["run"]["attempts"].clone();
    let runs = record["run"]["runs"].clone();
    let gates = record["run"]["gate_requests"].clone();
    assert!(
        revisions["characterize"].as_u64().unwrap_or(0) >= 1,
        "{revisions}"
    );
    assert!(
        attempts.as_array().is_some_and(|items| !items.is_empty()),
        "{attempts}"
    );
    assert!(runs["plan"].as_u64().unwrap_or(0) >= 1, "{runs}");
    assert!(runs["characterize"].as_u64().unwrap_or(0) >= 1, "{runs}");
    let worktree = PathBuf::from(record["worktree"].as_str().unwrap());
    let peeled = git(&worktree, &["rev-parse", &format!("{tag}^{{commit}}")]);
    let base = record["base_commit"].as_str().unwrap().to_owned();
    assert!(!peeled.is_empty() && peeled.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(peeled, tag, "the stored ref is a tag, not the commit");
    assert_ne!(
        peeled, base,
        "accepted evidence must not collapse to base_commit"
    );
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), peeled);
    assert!(
        git(
            &worktree,
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .is_empty(),
        "the tagged worktree must be clean before the off-evidence commit"
    );

    let cancel = fixture.control(&["cancel", RUN]);
    assert!(cancel.status.success(), "cancel: {}", text(&cancel));
    let frozen = durable(&fixture);

    fs::write(worktree.join("off-evidence.txt"), "assisted\n").unwrap();
    git(&worktree, &["add", "off-evidence.txt"]);
    git(&worktree, &["commit", "-m", "off accepted evidence"]);
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    assert_ne!(head, peeled);
    let refused = fixture.control(&["reopen", RUN, "--reason", "continue from accepted evidence"]);
    assert!(
        !refused.status.success(),
        "a commit past the evidence tag must be refused"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains(&peeled) && stderr.contains(&head),
        "refused reopen must name the peeled tag commit {peeled} and HEAD {head}: {stderr}"
    );
    assert_durable(&fixture, &frozen);

    git(&worktree, &["reset", "--hard", &peeled]);
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), peeled);
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
    let reopened = fixture.control(&["reopen", RUN, "--reason", "continue from accepted evidence"]);
    assert!(reopened.status.success(), "reopen: {}", text(&reopened));
    let after = fixture.record();
    assert_eq!(after["cancelled"], false, "{after}");
    assert_eq!(after["run"]["cancelled"], false, "{after}");
    assert_eq!(after["run"]["current"], "owner_go", "{after}");
    assert_eq!(after["run"]["freezes"], freezes, "{after}");
    assert_eq!(after["run"]["evidence_revisions"], revisions, "{after}");
    assert_eq!(after["run"]["attempts"], attempts, "{after}");
    assert_eq!(after["run"]["runs"], runs, "{after}");
    assert_eq!(after["run"]["gate_requests"], gates, "{after}");

    fixture.success(&["retry", RUN, "owner_go"]);
    let ticked = hub_once(&fixture);
    assert_eq!(ticked["run"]["current"], "owner_go", "{ticked}");
    assert_eq!(ticked["run"]["status"], "Gate", "{ticked}");
    assert_eq!(
        ticked["run"]["gate_requests"]["owner_go"], 2,
        "a reopened owner_go must open request 2 so approval of request 1 cannot pass: {ticked}"
    );
}
