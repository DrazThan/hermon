//! Regression for issue #145: a superseding RED after GREEN production work
//! establishes its expected failures against pre-implementation production.
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

const WORKFLOW_145: &str = r#"version: 1
name: correction-fixture
steps:
  plan:
    bot: worker
    produces: [test-expectations.json]
    next: red
  red:
    bot: worker
    check: [only_paths_changed:tests, tests_fail]
    tag: red
    next: green
  green:
    bot: reviewer
    check: [frozen_unchanged:red, project_check]
    tag: green
    next: finished
    on_block: {test_defect: red_fix}
  red_fix:
    bot: worker
    check: [only_paths_changed:tests, tests_fail]
    tag: red
    next: green
    max_runs: 2
  finished:
    gate: owner_go
"#;

/// The acceptance case fails only on the exact pre-implementation production
/// state: `base.txt` present, and neither GREEN's renamed file (whose name has a
/// space) nor its added file present. Any production change makes it pass, as a
/// real fix does. A test containing `vacuous` passes everywhere.
const TEST_REPORT: &str = r##"#!/bin/sh
pass() { printf '%s\n' '{"passed":1,"failed":0,"ignored":0,"failures":[]}'; exit 0; }
[ -f tests/red.test.js ] || pass
if grep -q vacuous tests/red.test.js; then pass; fi
if [ ! -f base.txt ] || [ -e "prod file.txt" ] || [ -e added.txt ]; then pass; fi
if [ -n "${CRASH_MARKER:-}" ] && grep -q corrected tests/red.test.js && [ ! -e "$CRASH_MARKER" ]; then
  : > "$CRASH_MARKER"
  kill -9 "$PPID"
  sleep 5
fi
printf '%s\n' '{"passed":0,"failed":1,"ignored":0,"failures":["acceptance_case"]}'
exit 1
"##;

/// `red_fix_body` is the shell run by the first red_fix attempt.
fn bot(red_fix_body: &str) -> String {
    format!(
        r##"#!/bin/sh
set -eu
task="$1"
result="${{task%/*}}/result.json"
done_result() {{ printf '{{"schema":1,"status":"done","summary":"%s","artifacts":[],"block":null,"verdict":null,"questions":[]}}' "$1" > "$result"; }}
case "$task" in
  */steps/plan/*/task.md)
    printf '%s' '{{"new_tests":["acceptance_case"],"baseline_failures":[]}}' > "${{task%/*}}/test-expectations.json"
    done_result "independent test IDs"
    ;;
  */steps/red/1/task.md)
    mkdir -p tests
    printf '%s' 'original independent acceptance test' > tests/red.test.js
    git add tests/red.test.js
    git commit -m 'independent acceptance RED' >/dev/null
    done_result "behavioral RED"
    ;;
  */steps/green/1/task.md)
    git mv base.txt 'prod file.txt'
    printf '%s' 'complete production fix' > 'prod file.txt'
    printf '%s' 'new production module' > added.txt
    git add -A
    git commit -m 'complete production fix before test defect' >/dev/null
    printf '%s' '{{"schema":1,"status":"block","summary":"test harness defect","artifacts":[],"block":{{"kind":"test_defect","reason":"fixture requires a narrow independent correction"}},"verdict":null,"questions":[]}}' > "$result"
    ;;
  */steps/red_fix/1/task.md)
{red_fix_body}
    ;;
  */steps/green/2/task.md)
    printf '%s' 'final production polish' >> added.txt
    git add added.txt
    git commit -m 'GREEN after corrected freeze' >/dev/null
    done_result "GREEN after correction"
    ;;
  *)
    done_result "ready"
    ;;
esac
"##
    )
}

const CORRECTION: &str = r#"    printf '%s' 'corrected independent acceptance test' > tests/red.test.js
    git add tests/red.test.js
    git commit -m 'correct only independent test fixture' >/dev/null
    done_result "corrected behavioral RED""#;

fn configure(fixture: &mut Fixture, red_fix_body: &str) {
    fs::write(&fixture.workflow, WORKFLOW_145).unwrap();
    fixture.config["start"] = json!("plan");
    fixture.config["paths"] = json!({"tests":["tests/**"]});
    fixture.config["commands"]["test"] =
        json!({"argv":["fixture-test-report"],"timeout_seconds":10,"report":"Json"});
    fixture.save_config();
    script(&fixture.root.join("bin/fixture-test-report"), TEST_REPORT);
    script(&fixture.root.join("bin/probe-bot"), &bot(red_fix_body));
}

/// Tick the real hub until the run waits at a gate or blocks. A tick killed by
/// the fixture's crash hook is tolerated once per `allowed_crashes`.
fn settle(fixture: &Fixture, mut allowed_crashes: u32) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let out = fixture.cli(&[
            "hub",
            "--once",
            "--no-notify",
            "--data-dir",
            fixture.data.to_str().unwrap(),
        ]);
        if !out.status.success() {
            assert!(allowed_crashes > 0, "host: {}", text(&out));
            allowed_crashes -= 1;
        }
        let record = fixture.record();
        if ["Gate", "Blocked"]
            .iter()
            .any(|s| record["run"]["status"] == *s)
        {
            return record;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run did not settle: {record}"
        );
        thread::sleep(Duration::from_millis(30));
    }
}

fn assert_clean_on_branch(record: &Value) {
    let worktree = PathBuf::from(record["worktree"].as_str().unwrap());
    assert_eq!(
        git(&worktree, &["symbolic-ref", "--short", "HEAD"]),
        record["branch"].as_str().unwrap(),
        "checks must leave the run worktree on its branch"
    );
    assert_eq!(
        git(
            &worktree,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        "",
        "checks must leave the run worktree clean"
    );
}

#[test]
fn test_only_correction_after_complete_green_fix_establishes_red_on_pre_implementation_production()
{
    let mut fixture = Fixture::new();
    configure(&mut fixture, CORRECTION);
    fixture.start();
    let record = settle(&fixture, 0);
    assert_eq!(
        record["run"]["status"], "Gate",
        "a test-only correction must prove RED against pre-implementation production, \
         not against GREEN's fixed HEAD: {record}"
    );
    assert_eq!(record["run"]["current"], "finished");
    assert_eq!(record["run"]["runs"]["red_fix"], 1);
    assert_eq!(record["run"]["runs"]["green"], 2);
    assert_clean_on_branch(&record);

    let worktree = PathBuf::from(record["worktree"].as_str().unwrap());
    let red = record["run"]["freezes"]["red"].as_str().unwrap();
    assert!(red.ends_with("/red.2"), "superseding RED tag: {red}");
    let note: Value =
        serde_json::from_str(&git(&worktree, &["notes", "--ref=hermon", "show", red])).unwrap();
    assert_eq!(note["frozen_paths"], json!(["tests/red.test.js"]));
    // The superseding RED evidence still describes the real run-branch commit.
    let tagged = git(&worktree, &["rev-parse", &format!("{red}^{{commit}}")]);
    assert_eq!(note["commit"], tagged);
    assert_eq!(
        git(&worktree, &["show", &format!("{tagged}:tests/red.test.js")]),
        "corrected independent acceptance test"
    );
    assert_eq!(
        git(&worktree, &["show", &format!("{tagged}:prod file.txt")]),
        "complete production fix",
        "GREEN's production commit stays in the candidate; nothing is reverted"
    );
    let test = note["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "test")
        .expect("test command evidence");
    assert_eq!(test["exit_code"], 1);
    assert_eq!(test["tests"]["failures"], json!(["acceptance_case"]));
    assert_eq!(
        test["production_base"], record["base_commit"],
        "evidence must record which production state tests_fail ran against"
    );
    // The original RED evidence is retained unchanged.
    let original = git(&worktree, &["rev-parse", "hermon/TICKET-123/red^{commit}"]);
    assert_eq!(
        git(
            &worktree,
            &["show", &format!("{original}:tests/red.test.js")]
        ),
        "original independent acceptance test"
    );
}

#[test]
fn correction_whose_tests_pass_on_pre_implementation_production_is_still_refused() {
    let mut fixture = Fixture::new();
    configure(
        &mut fixture,
        r#"    printf '%s' 'vacuous corrected test' > tests/red.test.js
    git add tests/red.test.js
    git commit -m 'weaken the acceptance test' >/dev/null
    done_result "corrected""#,
    );
    fixture.start();
    let record = settle(&fixture, 0);
    assert_eq!(record["run"]["status"], "Blocked", "{record}");
    assert_eq!(record["run"]["current"], "red_fix");
    assert!(
        record["run"]["block"]["reason"]
            .as_str()
            .unwrap()
            .contains("tests_fail did not establish"),
        "{record}"
    );
    assert_clean_on_branch(&record);
}

#[test]
fn correction_that_edits_production_stays_refused_after_the_automatic_check_retry() {
    let mut fixture = Fixture::new();
    configure(
        &mut fixture,
        r#"    printf '%s' 'corrected independent acceptance test' > tests/red.test.js
    git rm -q 'prod file.txt'
    git add -A
    git commit -m 'correction that also reverts production' >/dev/null
    done_result "corrected""#,
    );
    fixture.start();
    let record = settle(&fixture, 0);
    assert_eq!(record["run"]["status"], "Blocked", "{record}");
    assert_eq!(record["run"]["current"], "red_fix");
    assert!(
        record["run"]["block"]["reason"]
            .as_str()
            .unwrap()
            .contains("is outside tests"),
        "{record}"
    );
    assert_clean_on_branch(&record);
}

#[test]
fn hub_killed_while_checking_the_pre_implementation_tree_recovers_without_accepting_early() {
    let mut fixture = Fixture::new();
    configure(&mut fixture, CORRECTION);
    let marker = fixture.root.join("crashed");
    fixture.config["env"]["CRASH_MARKER"] = json!(marker);
    fixture.save_config();
    fixture.start();
    let record = settle(&fixture, 1);
    assert!(
        marker.exists(),
        "the crash hook must fire while the test command sees pre-implementation production"
    );
    assert_eq!(record["run"]["status"], "Gate", "{record}");
    assert_eq!(record["run"]["current"], "finished");
    assert_eq!(
        record["run"]["runs"]["red_fix"], 1,
        "recovery must re-check the same attempt, not dispatch another worker"
    );
    let kinds: Vec<_> = fixture
        .events()
        .iter()
        .filter(|e| e["step"] == "red_fix")
        .map(|e| e["kind"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        kinds.iter().filter(|k| *k == "evidence").count(),
        1,
        "{kinds:?}"
    );
    assert!(!kinds.iter().any(|k| k == "check_retry"), "{kinds:?}");
    assert_clean_on_branch(&record);
}
