//! Shared black-box fixture for the sdlc-light acceptance tests.
//! The fake `gh` never talks to GitHub. A lost `pr ready` response is armed with
//! `drop_ready_once` in the state file: the fake marks the PR ready, then exits.
#![allow(dead_code)]

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

pub const REPO: &str = "fixture/light";
pub const JOB: &str = "selected-build";
pub const FAIL_URL: &str = "https://github.com/fixture/light/actions/runs/9001";
pub const PASS_URL: &str = "https://github.com/fixture/light/actions/runs/11";
pub const PLAN_TOKEN: &str = "PLAN-TEXT-151-UNIQUE";
pub const CORRECTION: &str = "LIGHT-CORRECTION-ADDED";
pub const FORGED: &str = "WORKER-FORGED-DIFF-NOT-AUTHORITY";
pub const UNACCEPTED_ID: &str = "unaccepted_other_id";

const SECRET: &str = "synthetic-central-token-123";

const ENFORCE_WORKFLOW: &str = r#"version: 1
name: light-review
steps:
  plan:
    bot: planner
    produces: [plan.md, test-brief.md, impl-brief.md, test-expectations.json]
    next: red
  red:
    bot: test-writer
    inputs: [plan.md, test-brief.md]
    check: [only_paths_changed:tests, lint, tests_fail]
    tag: red
    next: green
  green:
    bot: implementer
    inputs: [plan.md, impl-brief.md, red, block_reason]
    check: [red_guard:red, lint, tests_pass]
    tag: green
    max_runs: 4
    next: draft_pr
  draft_pr:
    action: open_draft_pr
    next: ci
  ci:
    gate: github_ci
    next: review
    on_block: {ci_failed: green}
  review:
    bot: reviewer
    inputs: [plan.md, "diff:red..green", amendments]
    require_different_family: green
    review_amendments: {red: red, green: green}
    max_rounds: 2
    on_verdict: {changes_needed: green}
    next: ready
  ready:
    action: ready_pr
    on_block: {ci_failed: green}
    next: merge
  merge:
    gate: owner_merge
    next: docs
  docs:
    bot: writer
    inputs: [plan.md, merge]
"#;

const MATERIALIZE_WORKFLOW: &str = r#"version: 1
name: light-diff
steps:
  plan:
    bot: planner
    produces: [plan.md, test-expectations.json]
    next: red
  red:
    bot: test-writer
    inputs: [plan.md]
    check: [only_paths_changed:tests, lint, tests_fail]
    tag: red
    next: green
  green:
    bot: implementer
    inputs: [plan.md, red]
    check: [lint, tests_pass]
    tag: green
    next: draft_pr
  draft_pr:
    action: open_draft_pr
    next: review
  review:
    bot: reviewer
    inputs: [plan.md, "diff:red..green", amendments]
    require_different_family: green
    max_rounds: 2
    on_verdict: {changes_needed: green}
    next: docs
  docs:
    bot: writer
    inputs: [plan.md]
"#;

const GH_SCRIPT: &str = r##"#!/usr/bin/python3
import json, os, sys, subprocess

STATE = os.environ["HERMON_FAKE_GH_STATE"]
PROBE = os.environ["HERMON_FAKE_GH_PROBE"]
BARE = os.environ["HERMON_FAKE_GH_BARE"]
SLURP_MSG = "the `--slurp` option is not supported with `--jq` or `--template`"
VALUE_FLAGS = {
    "--hostname", "--method", "--jq", "--template", "--repo", "--base", "--head",
    "--state", "--limit", "--json", "--title", "--body-file", "--body",
}

def log(argv):
    with open(PROBE, "a") as handle:
        handle.write(json.dumps({"cmd": "gh", "argv": argv}) + "\n")
        handle.flush()

def fail(message, code=1):
    sys.stderr.write(message.rstrip() + "\n")
    raise SystemExit(code)

def parse(argv):
    flags = {}
    positionals = []
    index = 0
    while index < len(argv):
        arg = argv[index]
        if arg == "--":
            positionals.extend(argv[index + 1:])
            break
        if arg.startswith("--") and "=" in arg:
            name, value = arg.split("=", 1)
            flags[name] = value
            index += 1
            continue
        if arg in VALUE_FLAGS:
            if index + 1 >= len(argv):
                fail("fake gh: missing value for %s" % arg)
            flags[arg] = argv[index + 1]
            index += 2
            continue
        if arg.startswith("-"):
            flags[arg] = True
            index += 1
            continue
        positionals.append(arg)
        index += 1
    return flags, positionals

def load():
    with open(STATE) as handle:
        return json.load(handle)

def store(state):
    temporary = STATE + ".tmp"
    with open(temporary, "w") as handle:
        json.dump(state, handle)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, STATE)

def head_oid(branch):
    out = subprocess.run(
        ["/usr/bin/git", "--git-dir", BARE, "rev-parse", "refs/heads/%s" % branch],
        check=False, capture_output=True, text=True,
    )
    if out.returncode != 0:
        fail("fake gh: missing remote branch %s" % branch)
    return out.stdout.strip()

def project_pr(pr, fields):
    full = {
        "number": pr["number"],
        "url": pr["url"],
        "state": pr["state"],
        "isDraft": bool(pr["isDraft"]),
        "baseRefName": pr["baseRefName"],
        "headRefName": pr["headRefName"],
        "body": pr["body"],
        "headRepositoryOwner": pr["headRepositoryOwner"],
        "mergeCommit": pr.get("mergeCommit"),
        "title": pr.get("title", ""),
        "headRefOid": head_oid(pr["headRefName"]),
    }
    if not fields:
        return full
    wanted = [part.strip() for part in fields.split(",") if part.strip()]
    return {key: full[key] for key in wanted if key in full}

def find_pr(state, number):
    for pr in state["prs"]:
        if int(pr["number"]) == int(number):
            return pr
    fail("fake gh: unknown pull request %s" % number)

def repo_of(flags, state):
    repo = flags.get("--repo", state["repo"])
    if repo != state["repo"]:
        fail("fake gh: unexpected repository %s" % repo)
    return repo

def emit(value):
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")

def normalize_run(run, sha):
    app = run.get("app") or {"slug": "github-actions"}
    suite = run.get("check_suite") or {"id": run["id"]}
    return {
        "id": run["id"],
        "name": run["name"],
        "head_sha": run.get("head_sha", sha),
        "status": run["status"],
        "conclusion": run.get("conclusion", None),
        "html_url": run["html_url"],
        "app": {"slug": app.get("slug", "github-actions")},
        "check_suite": {"id": suite.get("id", run["id"])},
    }

def check_runs(flags, positionals):
    if flags.get("--hostname") != "github.com":
        fail("fake gh: check-runs requires --hostname github.com")
    if flags.get("--method") != "GET":
        fail("fake gh: check-runs requires --method GET")
    if "--paginate" not in flags:
        fail("fake gh: check-runs requires --paginate")
    if not flags.get("--jq"):
        fail("fake gh: check-runs requires --jq")
    endpoint = next((arg for arg in positionals[1:] if "check-runs" in arg), None)
    if endpoint is None:
        fail("fake gh: missing check-runs endpoint")
    path, _, query = endpoint.partition("?")
    params = {}
    for part in query.split("&"):
        if not part:
            continue
        key, _, value = part.partition("=")
        params[key] = value
    if params.get("filter") != "all":
        fail("fake gh: check-runs requires filter=all")
    if params.get("per_page") != "100":
        fail("fake gh: check-runs requires per_page=100")
    parts = [part for part in path.strip("/").split("/") if part]
    if len(parts) != 6 or parts[0] != "repos" or parts[3] != "commits" or parts[5] != "check-runs":
        fail("fake gh: unexpected check-runs path")
    state = load()
    if "%s/%s" % (parts[1], parts[2]) != state["repo"]:
        fail("fake gh: check-runs repository mismatch")
    sha = parts[4]
    runs = [normalize_run(run, sha) for run in state.get("checks", {}).get(sha, [])]
    total = len(runs)
    if total == 0:
        emit({"total_count": 0, "check_runs": []})
        return
    for start in range(0, total, 100):
        emit({"total_count": total, "check_runs": runs[start:start + 100]})

def main():
    argv = sys.argv[1:]
    if "--slurp" in argv and ("--jq" in argv or "--template" in argv):
        log(argv)
        fail(SLURP_MSG)
    log(argv)
    flags, positionals = parse(argv)
    state = load()
    fail_keys = set(state.get("fail", []))
    key = " ".join(positionals[:2])
    if key in fail_keys or (positionals and positionals[0] in fail_keys):
        fail("fake gh: refused %s" % (key or positionals[0]))
    if not positionals:
        fail("fake gh: missing command")
    command = positionals[0]
    if command == "auth" and len(positionals) > 1 and positionals[1] == "status":
        host = flags.get("--hostname")
        if host not in (None, "github.com"):
            fail("fake gh: unexpected hostname")
        sys.stdout.write("github.com\n")
        return
    if command == "repo" and len(positionals) >= 3 and positionals[1] == "view":
        if positionals[2] != state["repo"]:
            fail("fake gh: unexpected repository")
        emit({"nameWithOwner": state["repo"]})
        return
    if command == "pr":
        action = positionals[1] if len(positionals) > 1 else ""
        if action == "merge":
            fail("fake gh: pr merge is not a hub operation")
        repo_of(flags, state)
        if action == "list":
            base = flags.get("--base")
            head = flags.get("--head")
            wanted = flags.get("--state", "open")
            matched = []
            for pr in state["prs"]:
                if base and pr["baseRefName"] != base:
                    continue
                head_name = pr["headRefName"]
                if head and head_name != head and head_name != head.split(":")[-1]:
                    continue
                if wanted != "all" and pr["state"].lower() != wanted.lower():
                    continue
                matched.append(project_pr(pr, flags.get("--json")))
            emit(matched)
            return
        if action == "view":
            if len(positionals) < 3:
                fail("fake gh: pr view requires a number")
            emit(project_pr(find_pr(state, positionals[2]), flags.get("--json")))
            return
        if action == "create":
            for required in ("--repo", "--base", "--head", "--title", "--body-file"):
                if required not in flags:
                    fail("fake gh: pr create requires %s" % required)
            with open(flags["--body-file"]) as handle:
                body = handle.read()
            number = int(state.get("next_number", 1))
            owner = state["repo"].split("/")[0]
            pr = {
                "number": number,
                "url": "https://github.com/%s/pull/%s" % (state["repo"], number),
                "state": "OPEN",
                "isDraft": "--draft" in flags,
                "baseRefName": flags["--base"],
                "headRefName": flags["--head"],
                "title": flags["--title"],
                "body": body,
                "headRepositoryOwner": {"login": owner},
                "mergeCommit": None,
            }
            state.setdefault("prs", []).append(pr)
            state["next_number"] = number + 1
            store(state)
            sys.stdout.write(pr["url"] + "\n")
            return
        if action == "edit":
            if len(positionals) < 3 or "--body-file" not in flags:
                fail("fake gh: pr edit requires a number and --body-file")
            pr = find_pr(state, positionals[2])
            with open(flags["--body-file"]) as handle:
                pr["body"] = handle.read()
            store(state)
            sys.stdout.write(pr["url"] + "\n")
            return
        if action == "ready":
            if len(positionals) < 3:
                fail("fake gh: pr ready requires a number")
            pr = find_pr(state, positionals[2])
            pr["isDraft"] = False
            store(state)
            if state.get("drop_ready_once"):
                state["drop_ready_once"] = False
                store(state)
                fail("fake gh: lost pr ready response")
            sys.stdout.write(pr["url"] + "\n")
            return
        fail("fake gh: unsupported pr action %s" % action)
    if command == "api":
        check_runs(flags, positionals)
        return
    fail("fake gh: unsupported command %s" % " ".join(positionals))

if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as error:
        sys.stderr.write("fake gh: %s\n" % error)
        sys.exit(1)
"##;

const GIT_WRAPPER: &str = "\
#!/bin/sh
set -eu
if [ -n \"${HERMON_FAKE_GH_PROBE:-}\" ]; then
  {
    printf 'git'
    for arg in \"$@\"; do
      printf '\\t%s' \"$arg\"
    done
    printf '\\n'
  } >> \"$HERMON_FAKE_GH_PROBE\"
fi
exec /usr/bin/git \"$@\"
";

const BOT_SCRIPT: &str = r#"#!/bin/sh
set -eu
[ "${HUB_ONLY:-}" = yes ]
[ -z "${HOST_ONLY_123:-}" ]
task="$1"
workdir="$2"
root="${HERMON_FIXTURE_ROOT:?}"
result="${task%/*}/result.json"
done_result() {
  printf '%s' "{\"schema\":1,\"status\":\"done\",\"summary\":\"$1\",\"artifacts\":[],\"block\":null,\"verdict\":null,\"questions\":[]}" > "$result"
}
cd "$workdir"
case "$task" in
  */steps/plan/*/task.md)
    cp "$root/plan.md" "${task%/*}/plan.md"
    cp "$root/test-brief.md" "${task%/*}/test-brief.md"
    cp "$root/impl-brief.md" "${task%/*}/impl-brief.md"
    cp "$root/test-expectations.json" "${task%/*}/test-expectations.json"
    done_result "planned the light fixture"
    ;;
  */steps/red/*/task.md)
    mkdir -p tests
    printf 'light acceptance failing\n' > tests/light_acceptance.rs
    git add tests/light_acceptance.rs
    git commit -m "red acceptance" >/dev/null
    done_result "established red"
    ;;
  */steps/green/*/task.md)
    url="${HERMON_FAIL_URL:-}"
    if [ -n "$url" ] && grep -F -q "$url" "$task"; then
      printf 'production fix\n' >> src/light_fix.rs
      git add src/light_fix.rs
      git commit -m "production ci fix" >/dev/null
    else
      printf 'production behavior\n' > src/light_app.rs
      git add src/light_app.rs
      git commit -m "production" >/dev/null
      printf 'light acceptance\nLIGHT-CORRECTION-ADDED\n' > tests/light_acceptance.rs
      git add tests/light_acceptance.rs
      git commit -m "test-only correction" >/dev/null
      if [ -f "$root/forge-test-artifact" ]; then
        printf 'WORKER-FORGED-DIFF-NOT-AUTHORITY\n' > tests/worker-forged.diff
        git add tests/worker-forged.diff
        git commit -m "worker forged test artifact" >/dev/null
      fi
    fi
    done_result "implemented"
    ;;
  */steps/review/*/task.md)
    mode=$(cat "$root/review-mode")
    attempt=$(basename "$(dirname "$task")")
    printf '%s\n' "WORKER-FORGED-DIFF-NOT-AUTHORITY" > "${task%/*}/worker-forged-amendments.diff"
    if [ "$mode" = negative ] && [ "$attempt" = 1 ]; then
      printf '%s' '{"schema":1,"status":"done","summary":"clean without digest","artifacts":["worker-forged-amendments.diff"],"block":null,"verdict":"clean","questions":[]}' > "$result"
    elif [ "$mode" = negative ]; then
      printf '%s' '{"schema":1,"status":"done","summary":"wrong digest","artifacts":["worker-forged-amendments.diff"],"block":null,"verdict":"clean","questions":[],"accepted_amendments":"0000000000000000000000000000000000000000000000000000000000000000"}' > "$result"
    elif [ "$mode" = exact ]; then
      digest=$(/usr/bin/shasum -a 256 "${task%/*}/amendments.diff" | /usr/bin/awk '{print $1}')
      printf '%s' "{\"schema\":1,\"status\":\"done\",\"summary\":\"accepted ${digest}\",\"artifacts\":[\"worker-forged-amendments.diff\"],\"block\":null,\"verdict\":\"clean\",\"questions\":[],\"accepted_amendments\":\"${digest}\"}" > "$result"
    else
      printf '%s' '{"schema":1,"status":"done","summary":"reviewed host diff","artifacts":["worker-forged-amendments.diff"],"block":null,"verdict":"clean","questions":[]}' > "$result"
    fi
    ;;
  */steps/docs/*/task.md)
    done_result "documented the merge"
    ;;
  *)
    done_result "doctor probe complete"
    ;;
esac
"#;

const TEST_SCRIPT: &str = r#"#!/bin/sh
set -eu
root="${HERMON_FIXTURE_ROOT:?}"
if [ ! -f tests/light_acceptance.rs ]; then
  /bin/cat "$root/report-empty.json"
  exit 0
fi
if /usr/bin/grep -q LIGHT-CORRECTION-ADDED tests/light_acceptance.rs; then
  /bin/cat "$root/report-pass.json"
  exit 0
fi
/bin/cat "$root/report-fail.json"
exit 1
"#;

const LINT_SCRIPT: &str = "#!/bin/sh\nexit 0\n";
const PROJECT_CHECK_SCRIPT: &str = "\
#!/bin/sh
printf 'project_check\\n' >> \"$HERMON_PROJECT_CHECK_LOG\"
exit 1
";

#[derive(Clone, Copy)]
enum Kind {
    Enforce,
    Materialize,
    Pack,
}

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
            json!({"data":{"issue":{"id":"issue-151","comments":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}})
        } else if query.contains("viewer") {
            json!({"data":{"viewer":{"id":"central-app-151","name":"Synthetic app"}}})
        } else if query.contains("issue(") {
            json!({"data":{"issue":{"id":"issue-151","title":"Light fixture","description":"Implement the light fixture ticket."}}})
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

pub struct Fixture {
    _temp: tempfile::TempDir,
    server: Server,
    pub root: PathBuf,
    pub repo: PathBuf,
    pub bare: PathBuf,
    pub data: PathBuf,
    workflow: PathBuf,
    project: PathBuf,
    state_path: PathBuf,
    probe_path: PathBuf,
    check_log: PathBuf,
    pub run_id: String,
    pub expected_id: String,
    failure: &'static str,
    uses_ci: bool,
}

pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        repo.display(),
        text(&output)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

pub fn sha256_file(path: &Path) -> String {
    let output = Command::new("/usr/bin/shasum")
        .args(["-a", "256", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success(), "shasum: {}", text(&output));
    String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

fn script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn vitest(id: &str, status: &str) -> Value {
    let failed = status == "failed";
    json!({
        "numPassedTests": usize::from(!failed),
        "numFailedTests": usize::from(failed),
        "numPendingTests": 0,
        "numTodoTests": 0,
        "numTotalTests": 1,
        "numRuntimeErrorTestSuites": 0,
        "numFailedTestSuites": usize::from(failed),
        "success": !failed,
        "testResults": [{
            "name": "tests/light_acceptance.rs",
            "message": "",
            "status": status,
            "assertionResults": [{"fullName": id, "status": status}]
        }]
    })
}

impl Fixture {
    pub fn enforce(run_id: &str) -> Self {
        Self::open(Kind::Enforce, run_id, "light_review_expected")
    }

    pub fn materialize() -> Self {
        Self::open(Kind::Materialize, "DIFF-151", "light_diff_expected")
    }

    pub fn pack() -> Self {
        Self::open(Kind::Pack, "LIGHT-151", "light_sdlc_expected")
    }

    fn open(kind: Kind, run_id: &str, expected_id: &str) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("light fixture");
        let repo = root.join("repo");
        let bare = root.join("remote.git");
        let bin = root.join("bin");
        let data = root.join("data");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&bin).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.name", "Light fixture"]);
        git(&repo, &["config", "user.email", "light@example.invalid"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        git(&repo, &["config", "tag.gpgsign", "false"]);
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(repo.join("tests")).unwrap();
        fs::write(repo.join("src/base.rs"), "base\n").unwrap();
        fs::write(repo.join("tests/base.rs"), "base test\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "fixture base"]);
        git(&root, &["init", "--bare", bare.to_str().unwrap()]);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&repo, &["push", "origin", "main"]);
        let workflow = root.join("workflow.yaml");
        let (workflow_text, failure, uses_ci) = match kind {
            Kind::Enforce => (ENFORCE_WORKFLOW, "unknown field review_amendments", true),
            Kind::Materialize => (MATERIALIZE_WORKFLOW, "light review intake failed", false),
            Kind::Pack => ("", "missing pack", true),
        };
        if matches!(kind, Kind::Pack) {
            let pack = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../workflows/sdlc-light/workflow.yaml");
            fs::write(&workflow, pack.display().to_string()).unwrap();
        } else {
            fs::write(&workflow, workflow_text).unwrap();
        }
        let plan = format!(
            "# Light plan\n\n{PLAN_TOKEN}\n\nExpected ID: {expected_id}\nThe plan is published in the draft PR. There is no Owner GO.\n"
        );
        fs::write(root.join("plan.md"), plan).unwrap();
        fs::write(
            root.join("test-brief.md"),
            format!("Write failing acceptance for {expected_id}. Do not rename the ID.\n"),
        )
        .unwrap();
        fs::write(
            root.join("impl-brief.md"),
            "Implement in src/. Amend tests only in a separate test-only commit.\n",
        )
        .unwrap();
        fs::write(
            root.join("test-expectations.json"),
            serde_json::to_vec_pretty(&json!({
                "new_tests": [expected_id],
                "baseline_failures": []
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("report-empty.json"),
            serde_json::to_vec(&json!({
                "numPassedTests": 0,
                "numFailedTests": 0,
                "numPendingTests": 0,
                "numTodoTests": 0,
                "numTotalTests": 0,
                "numRuntimeErrorTestSuites": 0,
                "numFailedTestSuites": 0,
                "success": true,
                "testResults": []
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("report-pass.json"),
            serde_json::to_vec(&vitest(expected_id, "passed")).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("report-fail.json"),
            serde_json::to_vec(&vitest(expected_id, "failed")).unwrap(),
        )
        .unwrap();
        fs::write(root.join("review-mode"), "exact\n").unwrap();
        if matches!(kind, Kind::Materialize) {
            fs::write(root.join("forge-test-artifact"), "yes\n").unwrap();
        }
        let state_path = root.join("gh-state.json");
        let probe_path = root.join("gh-probe.log");
        let check_log = root.join("project-check.log");
        fs::write(&probe_path, "").unwrap();
        fs::write(&check_log, "").unwrap();
        fs::write(
            &state_path,
            serde_json::to_vec_pretty(&json!({
                "repo": REPO,
                "next_number": 1,
                "prs": [],
                "checks": {},
                "fail": [],
                "drop_ready_once": false
            }))
            .unwrap(),
        )
        .unwrap();
        script(&bin.join("gh"), GH_SCRIPT);
        script(&bin.join("git"), GIT_WRAPPER);
        script(&bin.join("probe-bot"), BOT_SCRIPT);
        script(&bin.join("light-test"), TEST_SCRIPT);
        script(&bin.join("light-lint"), LINT_SCRIPT);
        script(&bin.join("light-project-check"), PROJECT_CHECK_SCRIPT);
        let server = Server::new();
        let bot = |name: &str, family: &str| {
            (
                name.to_owned(),
                json!({"Command":{
                    "start": ["probe-bot", "{task}", "{workdir}"],
                    "resume": null,
                    "task_stdin": false,
                    "protocol": "ResultFile",
                    "family": family
                }}),
            )
        };
        let bots = if matches!(kind, Kind::Pack) {
            json!({
                "sdlc-planner": bot("sdlc-planner", "planner-family").1,
                "sdlc-test-writer": bot("sdlc-test-writer", "writer-family").1,
                "sdlc-implementer": bot("sdlc-implementer", "implementer-family").1,
                "sdlc-reviewer": bot("sdlc-reviewer", "reviewer-family").1,
                "sdlc-docs": bot("sdlc-docs", "docs-family").1
            })
        } else {
            json!({
                "planner": bot("planner", "planner-family").1,
                "test-writer": bot("test-writer", "writer-family").1,
                "implementer": bot("implementer", "implementer-family").1,
                "reviewer": bot("reviewer", "reviewer-family").1,
                "writer": bot("writer", "docs-family").1
            })
        };
        let mut config = json!({
            "repo": repo,
            "worktree_root": root.join("worktrees"),
            "branch_prefix": "flow",
            "base": "main",
            "start": "plan",
            "env": {
                "PATH": format!("{}:/usr/bin:/bin", bin.display()),
                "HUB_ONLY": "yes",
                "HERMON_FAKE_GH_STATE": state_path,
                "HERMON_FAKE_GH_PROBE": probe_path,
                "HERMON_FAKE_GH_BARE": bare,
                "HERMON_FIXTURE_ROOT": root,
                "HERMON_PROJECT_CHECK_LOG": check_log,
                "HERMON_FAIL_URL": FAIL_URL
            },
            "bots": bots,
            "commands": {
                "lint": {"argv": ["light-lint"], "timeout_seconds": 10},
                "test": {"argv": ["light-test"], "timeout_seconds": 10, "report": "VitestJson"}
            },
            "paths": {
                "tests": ["tests/**"],
                "production": ["src/**"]
            },
            "tracker": {
                "endpoint": server.endpoint,
                "token_env": "HERMON_TEST_LINEAR_TOKEN",
                "owner_ids": ["owner-151"],
                "owner_mention": "[Owner](https://linear.app/fixture/profiles/owner-151)"
            },
            "code_host": {"repo": REPO, "remote": "origin"}
        });
        if uses_ci {
            config["commands"]["project_check"] =
                json!({"argv": ["light-project-check"], "timeout_seconds": 5});
            config["code_host"]["ci"] = json!({
                "selected_checks": [JOB],
                "poll_interval_seconds": 1,
                "no_checks_timeout_seconds": 90,
                "timeout_seconds": 120
            });
        }
        let project = root.join("project.json");
        fs::write(&project, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        let workflow_path = if matches!(kind, Kind::Pack) {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../workflows/sdlc-light/workflow.yaml")
        } else {
            workflow
        };
        Self {
            _temp: temporary,
            server,
            root,
            repo,
            bare,
            data,
            workflow: workflow_path,
            project,
            state_path,
            probe_path,
            check_log,
            run_id: run_id.to_owned(),
            expected_id: expected_id.to_owned(),
            failure,
            uses_ci,
        }
    }

    pub fn set_review_mode(&self, mode: &str) {
        fs::write(self.root.join("review-mode"), format!("{mode}\n")).unwrap();
    }

    pub fn arm_drop_ready(&self) {
        let mut state = self.gh_state();
        state["drop_ready_once"] = json!(true);
        self.write_state(&state);
    }

    pub fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hermon"))
            .args(args)
            .env("HERMON_TEST_LINEAR_TOKEN", SECRET)
            .env("HOST_ONLY_123", "must-not-leak-into-hub")
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    pub fn start(&self) {
        let out = self.cli(&[
            "run",
            self.workflow.to_str().unwrap(),
            &self.run_id,
            "--project",
            self.project.to_str().unwrap(),
            "--data-dir",
            self.data.to_str().unwrap(),
            "--confirm",
            "--run-id",
            &self.run_id,
        ]);
        assert!(
            out.status.success(),
            "{} {}: {}",
            self.failure,
            self.workflow.display(),
            text(&out)
        );
    }

    pub fn tick(&self) -> Output {
        self.cli(&[
            "hub",
            "--once",
            "--no-notify",
            "--data-dir",
            self.data.to_str().unwrap(),
        ])
    }

    pub fn run_dir(&self) -> PathBuf {
        self.data.join("runs").join(&self.run_id)
    }

    pub fn record(&self) -> Value {
        serde_json::from_slice(&fs::read(self.run_dir().join("run.json")).unwrap()).unwrap()
    }

    pub fn events(&self) -> Vec<Value> {
        fs::read_to_string(self.run_dir().join("events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub fn until(&self, mut pred: impl FnMut(&Value) -> bool, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            let out = self.tick();
            let record = self.record();
            if pred(&record) {
                return record;
            }
            assert!(out.status.success(), "hub: {}", text(&out));
            assert!(Instant::now() < deadline, "run did not settle: {record:#}");
            thread::sleep(Duration::from_millis(40));
        }
    }

    pub fn gh_state(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.state_path).unwrap()).unwrap()
    }

    pub fn write_state(&self, state: &Value) {
        fs::write(&self.state_path, serde_json::to_vec_pretty(state).unwrap()).unwrap();
    }

    pub fn set_checks(&self, sha: &str, runs: Value) {
        let mut state = self.gh_state();
        state["checks"][sha] = runs;
        self.write_state(&state);
    }

    pub fn probes(&self) -> String {
        fs::read_to_string(&self.probe_path).unwrap_or_default()
    }

    pub fn gh_argv(&self) -> Vec<Vec<String>> {
        self.probes()
            .lines()
            .filter_map(|line| {
                let value: Value = serde_json::from_str(line).ok()?;
                (value["cmd"] == "gh").then(|| {
                    value["argv"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|item| item.as_str().unwrap().to_owned())
                        .collect()
                })
            })
            .collect()
    }

    pub fn comment_bodies(&self) -> Vec<String> {
        self.server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| {
                request["query"]
                    .as_str()
                    .is_some_and(|query| query.contains("commentCreate"))
            })
            .map(|request| {
                request["variables"]["input"]["body"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    pub fn check_log(&self) -> String {
        fs::read_to_string(&self.check_log).unwrap_or_default()
    }

    pub fn remote_sha(&self, branch: &str) -> String {
        git(&self.bare, &["rev-parse", &format!("refs/heads/{branch}")])
    }

    pub fn worktree(&self, record: &Value) -> PathBuf {
        PathBuf::from(record["worktree"].as_str().unwrap())
    }

    pub fn branch<'a>(&self, record: &'a Value) -> &'a str {
        record["branch"].as_str().unwrap()
    }

    pub fn task(&self, step: &str, number: u32) -> String {
        fs::read_to_string(
            self.run_dir()
                .join("steps")
                .join(step)
                .join(number.to_string())
                .join("task.md"),
        )
        .unwrap_or_else(|error| panic!("read {step}/{number} task.md: {error}"))
    }

    pub fn attempt_dir(&self, step: &str, number: u32) -> PathBuf {
        self.run_dir()
            .join("steps")
            .join(step)
            .join(number.to_string())
    }

    pub fn pr_body(&self) -> String {
        self.gh_state()["prs"][0]["body"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    pub fn code_host_state(&self) -> Value {
        let path = self.run_dir().join("code-host/run.json");
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap()
    }

    pub fn owner_merge(&self, head: &str) -> String {
        let dest = self.root.join(format!("merge-{}", &head[..12]));
        let output = Command::new("/usr/bin/git")
            .args(["clone", self.bare.to_str().unwrap(), dest.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success(), "clone: {}", text(&output));
        git(&dest, &["config", "user.name", "Owner"]);
        git(&dest, &["config", "user.email", "owner@example.invalid"]);
        git(&dest, &["config", "commit.gpgsign", "false"]);
        git(&dest, &["checkout", "main"]);
        git(&dest, &["merge", "--no-ff", "-m", "owner merge", head]);
        let merge = git(&dest, &["rev-parse", "HEAD"]);
        let second = git(&dest, &["rev-parse", "HEAD^2"]);
        assert_eq!(second, head, "merge second parent must be the merged head");
        git(&dest, &["push", "origin", "main"]);
        merge
    }

    pub fn push_external(&self, branch: &str) -> String {
        let dest = self.root.join("external-push");
        let output = Command::new("/usr/bin/git")
            .args(["clone", self.bare.to_str().unwrap(), dest.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success(), "clone: {}", text(&output));
        git(&dest, &["config", "user.name", "External"]);
        git(&dest, &["config", "user.email", "external@example.invalid"]);
        git(&dest, &["config", "commit.gpgsign", "false"]);
        git(
            &dest,
            &["checkout", "-B", branch, &format!("origin/{branch}")],
        );
        fs::write(dest.join("external.txt"), "external commit after ready\n").unwrap();
        git(&dest, &["add", "external.txt"]);
        git(&dest, &["commit", "-m", "external commit after ready"]);
        let sha = git(&dest, &["rev-parse", "HEAD"]);
        git(
            &dest,
            &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        );
        sha
    }

    pub fn mark_merged(&self, oid: &str) {
        let mut state = self.gh_state();
        state["prs"][0]["state"] = json!("MERGED");
        state["prs"][0]["isDraft"] = json!(false);
        state["prs"][0]["mergeCommit"] = json!({"oid": oid});
        self.write_state(&state);
    }
}

pub fn check_run(id: u64, status: &str, conclusion: Option<&str>, url: &str) -> Value {
    json!({
        "id": id,
        "name": JOB,
        "status": status,
        "conclusion": conclusion,
        "html_url": url
    })
}

pub fn is_pr(argv: &[String], action: &str) -> bool {
    argv.windows(2)
        .any(|pair| pair[0] == "pr" && pair[1] == action)
}

pub fn no_force(fixture: &Fixture) {
    let probe = fixture.probes();
    assert!(
        !probe.contains("--force"),
        "probe log used --force:\n{probe}"
    );
    for line in probe.lines().filter(|line| line.starts_with("git\tpush")) {
        for arg in line.split('\t').skip(1) {
            assert!(
                !arg.starts_with('+'),
                "push used a forced refspec {arg:?} in {line}"
            );
        }
    }
}
