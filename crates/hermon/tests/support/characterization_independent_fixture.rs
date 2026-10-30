//! Focused public-only intake/host fixture for trusted passing assertion IDs.
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
const RUN: &str = "FIXTURE-CHARACTERIZATION";
const WORKFLOW: &str = "version: 1\nname: independent-characterization-fixture\nsteps:\n  plan:\n    bot: worker\n    produces: [test-expectations.json]\n    next: characterize\n  characterize:\n    bot: worker\n    check: [tests_pass]\n    tag: characterized\n    next: owner_go\n  owner_go:\n    gate: owner_go\n";
struct Server {
    endpoint: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/graphql", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => Self::serve(stream),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("fixture accept: {e}"),
                }
            }
        });
        Self {
            endpoint,
            stop,
            worker: Some(worker),
        }
    }
    fn serve(mut stream: TcpStream) {
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = vec![];
        let mut chunk = [0; 4096];
        let request: Value = loop {
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0, "incomplete fixture request");
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let length: usize = header
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                }
            }
        };
        let query = request["query"].as_str().unwrap();
        let response = if query.contains("commentCreate") {
            json!({"data":{"commentCreate":{"success":true,"comment":{"id":request["variables"]["input"]["id"]}}}})
        } else if query.contains("comments") {
            json!({"data":{"issue":{"id":"fixture-issue","comments":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}})
        } else if query.contains("viewer") {
            json!({"data":{"viewer":{"id":"fixture-app","name":"Fixture central app"}}})
        } else if query.contains("issue(") {
            json!({"data":{"issue":{"id":"fixture-issue","title":"Synthetic characterization","description":"Public fixture acceptance only."}}})
        } else {
            panic!("unexpected fixture request")
        };
        let body = response.to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "DrazThan")
        .env("GIT_COMMITTER_NAME", "DrazThan")
        .env(
            "GIT_AUTHOR_EMAIL",
            "90212909+DrazThan@users.noreply.github.com",
        )
        .env(
            "GIT_COMMITTER_EMAIL",
            "90212909+DrazThan@users.noreply.github.com",
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn script(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
pub fn exercise(report: Value, status: i32, expectations: Value) -> Value {
    exercise_inner(report, status, expectations, false)
}
pub fn exercise_with_unaccepted_plan(report: Value, status: i32, expectations: Value) -> Value {
    exercise_inner(report, status, expectations, true)
}
fn exercise_inner(report: Value, status: i32, expectations: Value, poison: bool) -> Value {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let repo = root.join("repo");
    let bin = root.join("bin");
    fs::create_dir(&repo).unwrap();
    fs::create_dir(&bin).unwrap();
    git(&repo, &["init", "-b", "main"]);
    fs::write(repo.join("base.txt"), "public fixture baseline\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "public fixture baseline"]);
    let workflow = root.join("workflow.yaml");
    fs::write(&workflow, WORKFLOW).unwrap();
    let server = Server::new();
    script(&bin.join("gh"), "#!/bin/sh\nexit 0\n");
    script(
        &bin.join("fixture-agent"),
        r##"#!/bin/sh
set -eu
if /usr/bin/grep -q '^Doctor capability' "$1"; then
  printf '%s' '{"schema":1,"status":"done","summary":"ready","artifacts":[],"block":null,"verdict":null,"questions":[]}' > "${1%/*}/result.json"
elif /usr/bin/grep -q '^# .* / plan' "$1"; then
  /bin/cp "$EXPECTATIONS" "${1%/*}/test-expectations.json"
  printf '%s' '{"schema":1,"status":"done","summary":"independent plan accepted","artifacts":["test-expectations.json"],"block":null,"verdict":null,"questions":[]}' > "${1%/*}/result.json"
else
  printf 'fixture characterization test\n' > fixture.test.js
  /usr/bin/git add fixture.test.js
  /usr/bin/git diff --cached --quiet || /usr/bin/git -c user.name=DrazThan -c user.email=90212909+DrazThan@users.noreply.github.com commit -m characterization
  printf '%s' '{"schema":1,"status":"done","summary":"characterization ready","artifacts":[],"block":null,"verdict":null,"questions":[]}' > "${1%/*}/result.json"
fi
"##,
    );
    let report_path = root.join("report.json");
    fs::write(&report_path, report.to_string()).unwrap();
    let expectations_path = root.join("expected.json");
    fs::write(&expectations_path, expectations.to_string()).unwrap();
    let project = root.join("project.json");
    let data = root.join("data");
    let config = json!({"repo":repo,"worktree_root":root.join("worktrees"),"branch_prefix":"fixture","base":"main","start":"plan",
        "env":{"PATH":format!("{}:/usr/bin:/bin",bin.display()),"REPORT":report_path,"EXPECTATIONS":expectations_path,"TEST_EXIT":status.to_string()},
        "bots":{"worker":{"Command":{"start":["fixture-agent","{task}"],"resume":null,"task_stdin":false,"protocol":"ResultFile","family":"fixture"}}},
        "commands":{"test":{"argv":["/bin/sh","-c",r#"if [ -f fixture.test.js ]; then /bin/cat "$REPORT"; exit "$TEST_EXIT"; fi; printf '%s\n' '{"numPassedTests":1,"numFailedTests":0,"numPendingTests":0,"numTotalTests":1,"testResults":[{"assertionResults":[{"fullName":"fixture baseline","status":"passed"}]}]}'"#],"timeout_seconds":3,"report":"VitestJson"}},
        "tracker":{"endpoint":server.endpoint,"token_env":"HERMON_CHARACTERIZATION_FIXTURE_TOKEN","owner_ids":["fixture-owner"],"owner_mention":"@fixture-owner"},"code_host":{"repo":"fixture/project","remote":"origin"}});
    fs::write(&project, config.to_string()).unwrap();
    let cli = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_hermon"))
            .args(args)
            .env(
                "HERMON_CHARACTERIZATION_FIXTURE_TOKEN",
                "synthetic-fixture-token",
            )
            .current_dir(root)
            .output()
            .unwrap()
    };
    let out = cli(&[
        "run",
        workflow.to_str().unwrap(),
        "FIXTURE-CHAR",
        "--project",
        project.to_str().unwrap(),
        "--data-dir",
        data.to_str().unwrap(),
        "--confirm",
        "--run-id",
        RUN,
    ]);
    assert!(
        out.status.success(),
        "fixture intake: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let directory = data.join("runs").join(RUN);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let out = cli(&[
            "hub",
            "--once",
            "--no-notify",
            "--data-dir",
            data.to_str().unwrap(),
        ]);
        assert!(
            out.status.success(),
            "fixture host: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut record: Value =
            serde_json::from_slice(&fs::read(directory.join("run.json")).unwrap()).unwrap();
        if poison
            && record["run"]["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["step"] == "plan" && e["kind"] == "step_done")
        {
            let unaccepted = directory.join("steps/plan/999");
            fs::create_dir_all(&unaccepted).unwrap();
            fs::write(
                unaccepted.join("test-expectations.json"),
                r#"{"new_tests":["fixture wrong unaccepted ID"],"baseline_failures":[]}"#,
            )
            .unwrap();
        }
        if ["Gate", "Blocked"]
            .iter()
            .any(|state| record["run"]["status"] == *state)
        {
            if let Some(tag) = record["run"]["freezes"]["characterized"].as_str() {
                let worktree = PathBuf::from(record["worktree"].as_str().unwrap());
                record["fixture_evidence"] =
                    serde_json::from_str(&git(&worktree, &["notes", "--ref=hermon", "show", tag]))
                        .unwrap();
            }
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "synthetic characterization did not settle: {record}"
        );
        thread::sleep(Duration::from_millis(30));
    }
}
