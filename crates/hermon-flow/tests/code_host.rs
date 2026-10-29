//! Independently authored Slice 7 acceptance. Frozen before implementation.
#![cfg(unix)]
use hermon_flow::{
    code_host::{GithubCodeHost, GithubConfig},
    hub::{
        self, Agent, Alert, Block, Clock, CodeHost, Git, Poll, Settlement, StartRequest, Status,
        Tracker,
    },
    workflow::Workflow,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

const PR: &str = "https://github.com/fixture/project/pull/42";
struct Fixture {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    remote: PathBuf,
    peer: PathBuf,
    state: PathBuf,
    config: GithubConfig,
}
fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("/usr/bin/git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("work tree");
        let remote = dir.path().join("remote.git");
        let peer = dir.path().join("gh peer");
        let bin = dir.path().join("bin");
        let state = dir.path().join("code host state");
        for p in [&repo, &remote, &peer, &bin] {
            fs::create_dir_all(p).unwrap();
        }
        git(&remote, &["init", "--bare"]);
        git(&repo, &["init", "-b", "qa"]);
        git(&repo, &["config", "user.email", "fixture@example.invalid"]);
        git(&repo, &["config", "user.name", "Fixture"]);
        fs::write(repo.join("base.txt"), "base").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base"]);
        git(&repo, &["branch", "main"]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&repo, &["push", "origin", "qa", "main"]);
        git(&repo, &["checkout", "-b", "flow/run-122"]);
        fs::write(repo.join("feature.txt"), "feature").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "feature"]);
        let fake = bin.join("gh");
        fs::write(&fake, include_str!("fixtures/gh.py")).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let config = GithubConfig {
            workdir: repo.clone(),
            repo: "fixture/project".into(),
            remote: "origin".into(),
            base: "qa".into(),
            branch: "flow/run-122".into(),
            run_id: "RUN-122".into(),
            title: "Implement the chosen plan".into(),
            plan: "Chosen plan\n\nSupport words like $HOME and `literal` without shell expansion."
                .into(),
            review_summary: "Independent review: approved after checking regression coverage."
                .into(),
            env: BTreeMap::from([
                ("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())),
                ("GH_FIXTURE".into(), peer.to_str().unwrap().into()),
                ("GH_REMOTE".into(), remote.to_str().unwrap().into()),
            ]),
        };
        Self {
            _dir: dir,
            repo,
            remote,
            peer,
            state,
            config,
        }
    }
    fn adapter(&self) -> GithubCodeHost {
        GithubCodeHost::new(self.config.clone(), &self.state).unwrap()
    }
    fn calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.peer.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn count(&self, action: &str) -> usize {
        self.calls()
            .iter()
            .filter(|args| {
                args.first().is_some_and(|s| s == "pr") && args.get(1).is_some_and(|s| s == action)
            })
            .count()
    }
    fn view(&self, state: &str, oid: Option<&str>) {
        self.view_value(json!({"number":42,"url":PR,"state":state,
            "mergeCommit":oid.map(|oid|json!({"oid":oid})),"baseRefName":"qa","headRefName":"flow/run-122",
            "headRepositoryOwner":{"login":"fixture"}}));
    }
    fn view_value(&self, mut value: Value) {
        if value.get("body").is_none() {
            let saved: Value =
                serde_json::from_str(&fs::read_to_string(self.peer.join("prs.json")).unwrap())
                    .unwrap();
            value["body"] = saved[0]["body"].clone();
        }
        fs::write(self.peer.join("view.json"), value.to_string()).unwrap();
    }
    fn owner_merges(&self) -> String {
        // Owner advances remote base while the host's local qa remains stale.
        git(&self.repo, &["push", "origin", "flow/run-122:qa"]);
        git(&self.repo, &["rev-parse", "HEAD"])
    }
}

#[test]
fn pr_open_body_and_base() {
    let f = Fixture::new();
    let mut host = f.adapter();
    assert_eq!(host.open_pr("RUN-122").unwrap(), PR);
    let remote_head = git(&f.remote, &["rev-parse", "refs/heads/flow/run-122"]);
    assert_eq!(remote_head, git(&f.repo, &["rev-parse", "HEAD"]));
    let body = fs::read_to_string(f.peer.join("created-body.txt")).unwrap();
    assert!(
        body.contains(&f.config.plan),
        "PR body must include the plan verbatim"
    );
    assert!(
        body.contains(&f.config.review_summary),
        "PR body must include independent review summary"
    );
    assert!(
        body.contains("RUN-122"),
        "PR needs a stable run marker for reconciliation"
    );
    assert_eq!(f.count("create"), 1);
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(f.peer.join("prs.json")).unwrap()).unwrap();
    assert_eq!(saved[0]["title"], f.config.title);
}

#[test]
fn pr_open_reconciles_retry_and_restart() {
    let f = Fixture::new();
    assert_eq!(f.adapter().open_pr("RUN-122").unwrap(), PR);
    assert_eq!(f.adapter().open_pr("RUN-122").unwrap(), PR);
    // Losing local receipts still finds the marked external PR.
    if f.state.exists() {
        fs::remove_dir_all(&f.state).unwrap();
    }
    assert_eq!(f.adapter().open_pr("RUN-122").unwrap(), PR);
    assert_eq!(f.count("create"), 1);
}

#[test]
fn pr_open_recovers_a_lost_create_response() {
    let f = Fixture::new();
    fs::write(f.peer.join("drop-create-response"), "yes").unwrap();
    let _ = f.adapter().open_pr("RUN-122");
    fs::remove_file(f.peer.join("drop-create-response")).unwrap();
    assert_eq!(f.adapter().open_pr("RUN-122").unwrap(), PR);
    assert_eq!(
        f.count("create"),
        1,
        "ambiguous creation must reconcile before retry"
    );
}

#[test]
fn pr_open_fails_closed_on_lookup_and_push_errors() {
    let f = Fixture::new();
    fs::write(f.peer.join("list-fail"), "yes").unwrap();
    assert!(f.adapter().open_pr("RUN-122").is_err());
    assert_eq!(f.count("create"), 0);
    fs::remove_file(f.peer.join("list-fail")).unwrap();
    git(
        &f.repo,
        &["remote", "set-url", "origin", "/nonexistent/hermon-fixture"],
    );
    assert!(f.adapter().open_pr("RUN-122").is_err());
    assert_eq!(f.count("create"), 0);
}

#[test]
fn pr_open_rejects_wrong_run_and_rebound_intake() {
    let f = Fixture::new();
    assert!(f.adapter().open_pr("OTHER-RUN").is_err());
    assert_eq!(f.count("create"), 0);
    assert_eq!(f.adapter().open_pr("RUN-122").unwrap(), PR);
    let mut rebound = f.config.clone();
    rebound.base = "main".into();
    match GithubCodeHost::new(rebound, &f.state) {
        Err(_) => {}
        Ok(mut host) => assert!(
            host.open_pr("RUN-122").is_err(),
            "durable state cannot redirect to another intake base"
        ),
    }
    assert_eq!(f.count("create"), 1);
}

#[test]
fn pr_open_does_not_reuse_a_closed_or_foreign_pr() {
    for field in ["state", "body"] {
        let f = Fixture::new();
        f.adapter().open_pr("RUN-122").unwrap();
        fs::remove_dir_all(&f.state).unwrap();
        let path = f.peer.join("prs.json");
        let mut prs: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        prs[0][field] = if field == "state" {
            "CLOSED".into()
        } else {
            "another run without this run marker".into()
        };
        fs::write(path, prs.to_string()).unwrap();
        assert!(
            f.adapter().open_pr("RUN-122").is_err(),
            "must not bind {field} mismatch"
        );
        assert_eq!(f.count("create"), 1, "do not duplicate a conflicting PR");
    }
}

#[test]
fn merge_gate_requires_merged_and_ancestor() {
    let f = Fixture::new();
    let mut host = f.adapter();
    host.open_pr("RUN-122").unwrap();
    let oid = git(&f.repo, &["rev-parse", "HEAD"]);
    for state in ["OPEN", "CLOSED"] {
        f.view(state, Some(&oid));
        assert!(
            !host.merged(PR).unwrap(),
            "{state} is never owner merge evidence"
        );
    }
    f.view("MERGED", None);
    assert!(!host.merged(PR).unwrap_or(false));
    f.view("MERGED", Some(&oid));
    assert!(
        !host.merged(PR).unwrap(),
        "merge must be on configured remote base"
    );
    f.owner_merges();
    assert!(
        host.merged(PR).unwrap(),
        "must fetch fresh base, not trust stale local qa"
    );
    assert_eq!(
        git(&f.repo, &["rev-parse", "qa"]),
        git(&f.repo, &["rev-parse", "main"]),
        "fixture keeps local qa stale"
    );
}

#[test]
fn merge_gate_rejects_mismatched_pr_identity_and_bad_oid() {
    let f = Fixture::new();
    let mut host = f.adapter();
    host.open_pr("RUN-122").unwrap();
    let oid = f.owner_merges();
    for (field, wrong) in [
        ("baseRefName", "main"),
        ("headRefName", "another-run"),
        ("url", "https://github.com/other/repo/pull/42"),
    ] {
        f.view("MERGED", Some(&oid));
        let mut value: Value =
            serde_json::from_str(&fs::read_to_string(f.peer.join("view.json")).unwrap()).unwrap();
        value[field] = wrong.into();
        f.view_value(value);
        assert!(
            !host.merged(PR).unwrap_or(false),
            "mismatched {field} cannot approve"
        );
    }
    f.view("MERGED", Some("--help"));
    assert!(!host.merged(PR).unwrap_or(false));
    assert!(
        !host
            .merged("https://github.com/other/repo/pull/42")
            .unwrap_or(false)
    );
}

struct NoAgent;
impl Agent for NoAgent {
    fn family(&self, _: &str) -> String {
        "unused".into()
    }
    fn available(&self, _: &str) -> bool {
        false
    }
    fn start(&mut self, _: &StartRequest) -> Result<String, Block> {
        panic!("gate must block successor")
    }
    fn poll(&mut self, _: &str) -> Result<Poll, Block> {
        panic!("no bot")
    }
    fn stop(&mut self, _: &str) -> Result<Settlement, Block> {
        panic!("no bot")
    }
}
struct NoTracker;
impl Tracker for NoTracker {
    fn gate_approved(&mut self, _: &str, _: &str, _: u32) -> Result<bool, Block> {
        panic!("merge gate belongs to code host")
    }
    fn alert(&mut self, _: &str, _: &Alert) -> anyhow::Result<()> {
        Ok(())
    }
}
struct NoGit;
impl Git for NoGit {
    fn check(&mut self, _: &str, _: &[String], _: &BTreeMap<String, String>) -> Result<(), Block> {
        panic!("no bot check")
    }
    fn evidence(&mut self, _: &str, _: &str, _: u32) -> Result<String, Block> {
        panic!("no evidence")
    }
}
struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> u64 {
        10
    }
}
#[test]
fn hub_never_invokes_merge() {
    let f = Fixture::new();
    let workflow = Workflow::from_str("slice7", "version: 1\nname: slice7\nsteps:\n  pr:\n    action: open_pr\n    next: merge\n  merge:\n    gate: owner_merge\n").unwrap();
    let mut run = hub::Run::new("RUN-122", "pr");
    let mut host = f.adapter();
    let (mut agent, mut tracker, mut git) = (NoAgent, NoTracker, NoGit);
    let mut tick = |run: &mut hub::Run, host: &mut GithubCodeHost| {
        hub::advance(
            run,
            &workflow,
            &mut hub::Dependencies {
                agent: &mut agent,
                tracker: &mut tracker,
                code_host: host,
                git: &mut git,
                clock: &FixedClock,
            },
        )
        .unwrap();
    };
    tick(&mut run, &mut host);
    assert_eq!(run.status, Status::Gate);
    assert_eq!(run.pr.as_deref(), Some(PR));
    for _ in 0..3 {
        tick(&mut run, &mut host);
        assert_eq!(run.status, Status::Gate);
    }
    let oid = f.owner_merges();
    f.view("MERGED", Some(&oid));
    // Real hub and adapter survive snapshot/restart before observing owner merge.
    let mut run: hub::Run = serde_json::from_str(&serde_json::to_string(&run).unwrap()).unwrap();
    let mut host = f.adapter();
    tick(&mut run, &mut host);
    assert_eq!(run.status, Status::Done);
    assert!(run.events.iter().any(|e| e.kind == "gate_passed"));
    assert_eq!(f.count("merge"), 0);
    assert_eq!(f.count("create"), 1);
}
