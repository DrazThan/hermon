//! Independent review regressions for Slice 7. Frozen separately.
#![cfg(unix)]
use hermon_flow::{
    code_host::{GithubCodeHost, GithubConfig},
    hub::{BlockKind, CodeHost},
};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture {
    _dir: tempfile::TempDir,
    config: GithubConfig,
    state: PathBuf,
    second_remote: PathBuf,
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
        let repo = dir.path().join("work");
        let remote = dir.path().join("remote.git");
        let second_remote = dir.path().join("another.git");
        let peer = dir.path().join("peer");
        let bin = dir.path().join("bin");
        let state = dir.path().join("state");
        for p in [&repo, &remote, &second_remote, &peer, &bin] {
            fs::create_dir_all(p).unwrap();
        }
        git(&remote, &["init", "--bare"]);
        git(&second_remote, &["init", "--bare"]);
        git(&repo, &["init", "-b", "qa"]);
        git(&repo, &["config", "user.email", "fixture@example.invalid"]);
        git(&repo, &["config", "user.name", "Fixture"]);
        fs::write(repo.join("base"), "base").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base"]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&repo, &["push", "origin", "qa"]);
        git(&repo, &["checkout", "-b", "flow/run-122"]);
        let gh = bin.join("gh");
        fs::write(&gh, include_str!("fixtures/gh.py")).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        let config = GithubConfig {
            workdir: repo,
            repo: "fixture/project".into(),
            remote: "origin".into(),
            base: "qa".into(),
            branch: "flow/run-122".into(),
            run_id: "RUN-122".into(),
            title: "Reviewed task".into(),
            plan: "Reviewed plan".into(),
            review_summary: "Approved".into(),
            env: BTreeMap::from([
                ("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())),
                ("GH_FIXTURE".into(), peer.to_str().unwrap().into()),
                ("GH_REMOTE".into(), remote.to_str().unwrap().into()),
                (
                    "GH_TOKEN".into(),
                    "fixture-secret-must-not-be-persisted".into(),
                ),
            ]),
        };
        Self {
            _dir: dir,
            config,
            state,
            second_remote,
        }
    }
    fn adapter(&self) -> GithubCodeHost {
        GithubCodeHost::new(self.config.clone(), &self.state).unwrap()
    }
}

#[test]
fn publication_rejects_changed_push_endpoint() {
    let f = Fixture::new();
    let mut host = f.adapter();
    git(
        &f.config.workdir,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            f.second_remote.to_str().unwrap(),
        ],
    );
    assert!(
        host.open_pr("RUN-122").is_err(),
        "publication must bind the push endpoint as well as the fetch endpoint"
    );
    let out = Command::new("/usr/bin/git")
        .current_dir(&f.second_remote)
        .args(["show-ref", "--verify", "refs/heads/flow/run-122"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "retargeted remote must receive no run branch"
    );
}

#[test]
fn merge_gate_rejects_changed_remote_after_restart() {
    let f = Fixture::new();
    let pr = f.adapter().open_pr("RUN-122").unwrap();
    git(
        &f.config.workdir,
        &[
            "remote",
            "set-url",
            "origin",
            f.second_remote.to_str().unwrap(),
        ],
    );
    match GithubCodeHost::new(f.config.clone(), &f.state) {
        Err(_) => {}
        Ok(mut host) => assert!(
            host.merged(&pr).is_err(),
            "retargeted remote cannot supply merge evidence"
        ),
    }
}

#[test]
fn missing_gh_uses_configured_path_and_blocks_capability() {
    let mut f = Fixture::new();
    let git_only = f._dir.path().join("git-only");
    fs::create_dir(&git_only).unwrap();
    std::os::unix::fs::symlink("/usr/bin/git", git_only.join("git")).unwrap();
    f.config
        .env
        .insert("PATH".into(), git_only.to_str().unwrap().into());
    let mut host = f.adapter();
    let error = host.open_pr("RUN-122").unwrap_err();
    assert_eq!(error.kind, BlockKind::Capability);
    assert!(!error.reason.trim().is_empty());
    assert!(
        !error
            .reason
            .contains("fixture-secret-must-not-be-persisted")
    );
}

#[test]
fn code_host_receipts_never_persist_authentication() {
    let f = Fixture::new();
    f.adapter().open_pr("RUN-122").unwrap();
    fn scan(path: &Path) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path);
            } else {
                let text = fs::read_to_string(&path).unwrap();
                assert!(!text.contains("fixture-secret-must-not-be-persisted"));
                assert!(!text.contains("GH_TOKEN"));
            }
        }
    }
    scan(&f.state);
}

#[test]
fn github_commands_bind_the_configured_repository_host() {
    let mut f = Fixture::new();
    let bin = f._dir.path().join("bin");
    let fake = include_str!("fixtures/gh.py").replace(
        "root = pathlib.Path(os.environ['GH_FIXTURE'])",
        "assert os.environ.get('GH_HOST') == 'github.com', 'GitHub.com repository requires GitHub.com CLI host'\nroot = pathlib.Path(os.environ['GH_FIXTURE'])",
    );
    fs::write(bin.join("gh"), fake).unwrap();
    f.config
        .env
        .insert("GH_HOST".into(), "unrelated.example.invalid".into());
    assert!(
        f.adapter().open_pr("RUN-122").is_ok(),
        "configured ambient GH_HOST must not redirect a github.com run"
    );
}

#[test]
fn publication_rejects_a_new_additional_push_endpoint() {
    let f = Fixture::new();
    let remote = git(&f.config.workdir, &["remote", "get-url", "origin"]);
    git(
        &f.config.workdir,
        &["remote", "set-url", "--push", "origin", &remote],
    );
    let mut host = f.adapter();
    git(
        &f.config.workdir,
        &[
            "remote",
            "set-url",
            "--push",
            "--add",
            "origin",
            f.second_remote.to_str().unwrap(),
        ],
    );
    assert!(host.open_pr("RUN-122").is_err());
    let out = Command::new("/usr/bin/git")
        .current_dir(&f.second_remote)
        .args(["show-ref", "--verify", "refs/heads/flow/run-122"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "all configured push URLs must be bound, not just the first"
    );
}
