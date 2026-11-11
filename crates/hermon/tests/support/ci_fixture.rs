//! Shared black-box fixture for the draft-PR and GitHub Actions gate tests.
//!
//! The fake `gh` is a script on the project PATH. It never talks to GitHub.
//! State file (env `HERMON_FAKE_GH_STATE`):
//! ```json
//! {
//!   "repo": "fixture/project",
//!   "next_number": 1,
//!   "prs": [{
//!     "number": 1,
//!     "url": "https://github.com/fixture/project/pull/1",
//!     "state": "OPEN",
//!     "isDraft": true,
//!     "baseRefName": "main",
//!     "headRefName": "flow/CI-150",
//!     "title": "...",
//!     "body": "...",
//!     "headRepositoryOwner": {"login": "fixture"},
//!     "mergeCommit": null
//!   }],
//!   "checks": {
//!     "<sha>": [{
//!       "id": 1,
//!       "name": "selected-build",
//!       "status": "completed",
//!       "conclusion": "success",
//!       "html_url": "https://github.com/fixture/project/actions/runs/1"
//!     }]
//!   },
//!   "fail": ["pr ready"]
//! }
//! ```
//! `head_sha`, `app.slug` and `check_suite.id` are filled by the fake when omitted.
//! `gh pr view` reports `headRefOid` from `git --git-dir $HERMON_FAKE_GH_BARE rev-parse`.
//! Every invocation is appended to `HERMON_FAKE_GH_PROBE`: a JSON line
//! `{"cmd":"gh","argv":[...]}` or a tab-separated `git\t...` line from the git wrapper.
//! `--slurp` combined with `--jq` or `--template` exits 1 with gh 2.102.0's message.
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

pub const RUN: &str = "CI-150";
pub const TICKET: &str = "CI-150";
pub const REPO: &str = "fixture/project";
pub const JOB_BUILD: &str = "selected-build";
pub const JOB_LINT: &str = "selected-lint";
pub const JOB_UNSELECTED: &str = "unselected-flake";
pub const FAIL_URL: &str = "https://github.com/fixture/project/actions/runs/9001";
pub const SKIP_URL: &str = "https://github.com/fixture/project/actions/runs/9002";
pub const UNSELECTED_URL: &str = "https://github.com/fixture/project/actions/runs/9003";
pub const REVIEW_SUMMARY: &str = "fixture-review-summary-accepted";
pub const PLAN: &str = "\
# Fixture plan

Ship the ci-fixture behavior for issue 150.

Token: PLAN-TEXT-150-UNIQUE
Keep this paragraph intact.
";

pub const WORKFLOW: &str = "\
version: 1
name: ci-fixture
steps:
  plan:
    bot: planner
    produces: [plan.md]
    next: green
  green:
    bot: implementer
    inputs: [plan.md, block_reason]
    tag: green
    max_runs: 3
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
    require_different_family: green
    on_verdict: {changes_needed: green}
    max_rounds: 2
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
    inputs: [merge]
";

const LEGACY_WORKFLOW: &str = "\
version: 1
name: legacy-fixture
steps:
  work:
    bot: worker
    next: finish
  finish:
    gate: owner_merge
";

const SECRET: &str = "synthetic-central-token-123";
const SLURP_MSG: &str = "the `--slurp` option is not supported with `--jq` or `--template`";

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
BOOL_FLAGS = {"--draft", "--paginate", "--slurp", "--force", "--force-with-lease"}

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

const BOT_SCRIPT: &str = r##"#!/bin/sh
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
    done_result "planned the fixture"
    ;;
  */steps/green/*/task.md)
    url="${HERMON_FAIL_URL:-}"
    if [ -n "$url" ] && grep -F -q "$url" "$task" && [ ! -f "$root/signals/green-transient-done" ]; then
      i=0
      while [ ! -f "$root/signals/fail-green" ] && [ "$i" -lt 150 ]; do
        sleep 0.1
        i=$((i + 1))
      done
      : > "$root/signals/green-transient-done"
      exit 9
    fi
    attempt=$(basename "$(dirname "$task")")
    printf 'green attempt %s\n' "$attempt" >> impl.txt
    git add impl.txt
    git commit -m "green attempt $attempt" >/dev/null
    done_result "implemented attempt $attempt"
    ;;
  */steps/review/*/task.md)
    if [ -f "$root/review-verdict" ]; then
      verdict=$(cat "$root/review-verdict")
    else
      verdict=clean
    fi
    summary="${HERMON_REVIEW_SUMMARY:?}"
    printf '%s' "{\"schema\":1,\"status\":\"done\",\"summary\":\"$summary\",\"artifacts\":[],\"block\":null,\"verdict\":\"$verdict\",\"questions\":[]}" > "$result"
    ;;
  */steps/docs/*/task.md)
    done_result "documented the merge"
    ;;
  *)
    done_result "doctor probe complete"
    ;;
esac
"##;

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
            json!({"data":{"issue":{"id":"issue-150","comments":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}})
        } else if query.contains("viewer") {
            json!({"data":{"viewer":{"id":"central-app-150","name":"Synthetic app"}}})
        } else if query.contains("issue(") {
            json!({"data":{"issue":{"id":"issue-150","title":"Fixture issue","description":"Implement the fixture ticket."}}})
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

#[derive(Clone, Copy)]
enum Kind {
    Ci,
    CiNoPolicy,
    Legacy,
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
    config: Value,
}
pub fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        repo.display(),
        text(&out)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}
pub fn text(output: &Output) -> String {
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
pub fn check_run(id: u64, name: &str, status: &str, conclusion: Option<&str>, url: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "status": status,
        "conclusion": conclusion,
        "html_url": url
    })
}
pub fn passing_runs() -> Value {
    json!([
        check_run(
            11,
            JOB_BUILD,
            "completed",
            Some("success"),
            "https://github.com/fixture/project/actions/runs/11"
        ),
        check_run(
            12,
            JOB_LINT,
            "completed",
            Some("success"),
            "https://github.com/fixture/project/actions/runs/12"
        )
    ])
}
impl Fixture {
    pub fn new() -> Self {
        Self::open(Kind::Ci)
    }
    pub fn without_policy() -> Self {
        Self::open(Kind::CiNoPolicy)
    }
    pub fn legacy() -> Self {
        Self::open(Kind::Legacy)
    }
    fn open(kind: Kind) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("fixture with spaces");
        let repo = root.join("repo");
        let bare = root.join("remote.git");
        let bin = root.join("bin");
        let data = root.join("data");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(root.join("signals")).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.name", "CLI fixture"]);
        git(&repo, &["config", "user.email", "fixture@example.invalid"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        git(&repo, &["config", "tag.gpgsign", "false"]);
        fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "fixture base"]);
        git(&root, &["init", "--bare", bare.to_str().unwrap()]);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&repo, &["push", "origin", "main"]);
        let workflow_text = match kind {
            Kind::Legacy => LEGACY_WORKFLOW,
            Kind::Ci | Kind::CiNoPolicy => WORKFLOW,
        };
        let workflow = root.join("workflow.yaml");
        fs::write(&workflow, workflow_text).unwrap();
        fs::write(root.join("plan.md"), PLAN).unwrap();
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
                "fail": []
            }))
            .unwrap(),
        )
        .unwrap();
        script(&bin.join("gh"), GH_SCRIPT);
        script(&bin.join("git"), GIT_WRAPPER);
        script(&bin.join("probe-bot"), BOT_SCRIPT);
        script(
            &bin.join("project-check"),
            "#!/bin/sh\nprintf 'project_check\\n' >> \"$HERMON_PROJECT_CHECK_LOG\"\nexit 1\n",
        );
        let server = Server::new();
        let bot = |family: &str| {
            json!({"Command":{
                "start": ["probe-bot", "{task}", "{workdir}"],
                "resume": null,
                "task_stdin": false,
                "protocol": "ResultFile",
                "family": family
            }})
        };
        let mut config = json!({
            "repo": repo,
            "worktree_root": root.join("worktrees"),
            "branch_prefix": "flow",
            "base": "main",
            "start": if matches!(kind, Kind::Legacy) { "work" } else { "plan" },
            "env": {
                "PATH": format!("{}:/usr/bin:/bin", bin.display()),
                "HUB_ONLY": "yes",
                "HERMON_FAKE_GH_STATE": state_path,
                "HERMON_FAKE_GH_PROBE": probe_path,
                "HERMON_FAKE_GH_BARE": bare,
                "HERMON_FIXTURE_ROOT": root,
                "HERMON_PROJECT_CHECK_LOG": check_log,
                "HERMON_FAIL_URL": FAIL_URL,
                "HERMON_REVIEW_SUMMARY": REVIEW_SUMMARY
            },
            "bots": {},
            "commands": {},
            "tracker": {
                "endpoint": server.endpoint,
                "token_env": "HERMON_TEST_LINEAR_TOKEN",
                "owner_ids": ["owner-150"],
                "owner_mention": "[Owner](https://linear.app/fixture/profiles/owner-150)"
            },
            "code_host": {"repo": REPO, "remote": "origin"}
        });
        if matches!(kind, Kind::Legacy) {
            config["bots"]["worker"] = bot("worker-family");
        } else {
            config["bots"]["planner"] = bot("planner-family");
            config["bots"]["implementer"] = bot("implementer-family");
            config["bots"]["reviewer"] = bot("reviewer-family");
            config["bots"]["writer"] = bot("planner-family");
        }
        if matches!(kind, Kind::Ci) {
            config["code_host"]["ci"] = json!({
                "selected_checks": [JOB_BUILD, JOB_LINT],
                "poll_interval_seconds": 1,
                "no_checks_timeout_seconds": 30,
                "timeout_seconds": 60
            });
        }
        let project = root.join("project.json");
        let fixture = Self {
            _temp: temp,
            server,
            root,
            repo,
            bare,
            data,
            workflow,
            project,
            state_path,
            probe_path,
            check_log,
            config,
        };
        fixture.save_config();
        fixture
    }
    pub fn save_config(&self) {
        fs::write(
            &self.project,
            serde_json::to_vec_pretty(&self.config).unwrap(),
        )
        .unwrap();
    }
    pub fn set_ci_timeouts(&mut self, poll: u64, no_checks: u64, overall: u64) {
        self.config["code_host"]["ci"]["poll_interval_seconds"] = json!(poll);
        self.config["code_host"]["ci"]["no_checks_timeout_seconds"] = json!(no_checks);
        self.config["code_host"]["ci"]["timeout_seconds"] = json!(overall);
        self.save_config();
    }
    pub fn weaken_live_no_checks_timeout(&mut self, seconds: u64) {
        self.config["code_host"]["ci"]["no_checks_timeout_seconds"] = json!(seconds);
        self.save_config();
    }
    pub fn workflow_str(&self) -> &str {
        self.workflow.to_str().unwrap()
    }
    pub fn project_str(&self) -> &str {
        self.project.to_str().unwrap()
    }
    pub fn with_failing_project_check(&mut self) {
        self.config["commands"]["project_check"] =
            json!({"argv": ["project-check"], "timeout_seconds": 5});
        self.save_config();
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
    pub fn control(&self, args: &[&str]) -> Output {
        let mut args = args.to_vec();
        args.extend(["--data-dir", self.data.to_str().unwrap()]);
        self.cli(&args)
    }
    pub fn doctor(&self) -> Output {
        self.cli(&[
            "doctor",
            self.workflow.to_str().unwrap(),
            "--project",
            self.project.to_str().unwrap(),
            "--ticket",
            TICKET,
            "--json",
        ])
    }
    pub fn start(&self) {
        let out = self.cli(&[
            "run",
            self.workflow.to_str().unwrap(),
            TICKET,
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
        self.data.join("runs").join(RUN)
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
    pub fn git_pushes(&self) -> Vec<Vec<String>> {
        self.probes()
            .lines()
            .filter(|line| line.starts_with("git\tpush"))
            .map(|line| line.split('\t').skip(1).map(str::to_owned).collect())
            .collect()
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
    pub fn fail_command(&self, command: &str) {
        let mut state = self.gh_state();
        state["fail"].as_array_mut().unwrap().push(json!(command));
        self.write_state(&state);
    }
    pub fn mark_merged(&self, oid: &str) {
        let mut state = self.gh_state();
        state["prs"][0]["state"] = json!("MERGED");
        state["prs"][0]["isDraft"] = json!(false);
        state["prs"][0]["mergeCommit"] = json!({"oid": oid});
        self.write_state(&state);
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
    pub fn bare_refs(&self) -> String {
        git(&self.bare, &["for-each-ref", "--format=%(refname)"])
    }
    pub fn worktree(&self, record: &Value) -> PathBuf {
        PathBuf::from(record["worktree"].as_str().unwrap())
    }
    pub fn branch<'a>(&self, record: &'a Value) -> &'a str {
        record["branch"].as_str().unwrap()
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
    pub fn drive_to(&self, step: &str, status: &str) -> Value {
        self.until(
            |record| {
                let current = record["run"]["current"].as_str().unwrap();
                let state = record["run"]["status"].as_str().unwrap();
                if state == "Blocked" && !(current == step && status == "Blocked") {
                    panic!("blocked at {current}: {}", record["run"]["block"]);
                }
                current == step && state == status
            },
            Duration::from_secs(20),
        )
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
    pub fn release_transient(&self) {
        fs::write(self.root.join("signals/fail-green"), "go\n").unwrap();
    }
    pub fn wait_attempt_finished(&self, step: &str, number: u32) {
        let path = self
            .run_dir()
            .join("steps")
            .join(step)
            .join(number.to_string())
            .join("process.json");
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(body) = fs::read(&path)
                && let Ok(value) = serde_json::from_slice::<Value>(&body)
                && value["finished"].as_bool() == Some(true)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "worker {step}/{number} did not finish"
            );
            thread::sleep(Duration::from_millis(30));
        }
    }
    pub fn assert_slurp_rejected(&self) {
        let out = Command::new(self.root.join("bin/gh"))
            .args([
                "api",
                "--hostname",
                "github.com",
                "--method",
                "GET",
                "--slurp",
                "--jq",
                ".",
                "--paginate",
                "repos/fixture/project/commits/abc/check-runs?filter=all&per_page=100",
            ])
            .env("HERMON_FAKE_GH_STATE", &self.state_path)
            .env("HERMON_FAKE_GH_PROBE", self.root.join("slurp-probe.log"))
            .env("HERMON_FAKE_GH_BARE", &self.bare)
            .output()
            .unwrap();
        assert!(!out.status.success(), "slurp+jq must be rejected");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains(SLURP_MSG),
            "slurp rejection must match gh 2.102.0, got: {err}"
        );
    }
    pub fn gh_view(&self, number: u64) -> Value {
        let out = Command::new(self.root.join("bin/gh"))
            .args([
                "pr",
                "view",
                &number.to_string(),
                "--repo",
                REPO,
                "--json",
                "number,url,state,isDraft,baseRefName,headRefName,body,headRepositoryOwner,mergeCommit,headRefOid",
            ])
            .env("HERMON_FAKE_GH_STATE", &self.state_path)
            .env("HERMON_FAKE_GH_PROBE", &self.probe_path)
            .env("HERMON_FAKE_GH_BARE", &self.bare)
            .output()
            .unwrap();
        assert!(out.status.success(), "pr view: {}", text(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }
    pub fn owner_merge(&self, head: &str) -> String {
        let dest = self.root.join(format!("merge-{}", &head[..12]));
        let out = Command::new("/usr/bin/git")
            .args(["clone", self.bare.to_str().unwrap(), dest.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "clone: {}", text(&out));
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
    pub fn push_external_commit(&self, branch: &str) -> String {
        let dest = self.root.join("external-push");
        let out = Command::new("/usr/bin/git")
            .args(["clone", self.bare.to_str().unwrap(), dest.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "clone: {}", text(&out));
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
}

pub fn is_pr(argv: &[String], action: &str) -> bool {
    argv.windows(2)
        .any(|pair| pair[0] == "pr" && pair[1] == action)
}
pub fn routed(record: &Value, from: &str, to: &str) -> bool {
    record["run"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["kind"] == "route" && event["step"] == from && event["reason"] == to)
}
pub fn block_reason_before_route<'a>(record: &'a Value, from: &str, to: &str) -> Option<&'a str> {
    let events = record["run"]["events"].as_array().unwrap();
    events.windows(2).find_map(|pair| {
        (pair[0]["kind"] == "block"
            && pair[1]["kind"] == "route"
            && pair[1]["step"] == from
            && pair[1]["reason"] == to)
            .then(|| pair[0]["reason"].as_str().unwrap())
    })
}
pub fn assert_no_force(fixture: &Fixture) {
    let probe = fixture.probes();
    assert!(
        !probe.contains("--force"),
        "probe log used --force:\n{probe}"
    );
    for push in fixture.git_pushes() {
        for arg in &push {
            assert!(
                !arg.starts_with('+'),
                "push used a forced refspec {arg:?} in {push:?}"
            );
        }
    }
}
pub fn assert_no_pr_merge(fixture: &Fixture) {
    assert!(
        fixture.gh_argv().iter().all(|argv| !is_pr(argv, "merge")),
        "hub must not call pr merge:\n{}",
        fixture.probes()
    );
}
