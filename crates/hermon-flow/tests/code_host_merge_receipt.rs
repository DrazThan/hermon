//! Independent issue #125 regression: verified merge metadata survives restart.
#![cfg(unix)]
use hermon_flow::{
    code_host::{GithubCodeHost, GithubConfig},
    hub::CodeHost,
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
            peer,
            state,
            config,
        }
    }
    fn adapter(&self) -> GithubCodeHost {
        GithubCodeHost::new(self.config.clone(), &self.state).unwrap()
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

fn receipt(fixture: &Fixture) -> Value {
    serde_json::from_slice(&fs::read(fixture.state.join("run.json")).unwrap()).unwrap()
}

#[test]
fn verified_owner_merge_persists_exact_commit_for_docs_and_outcome() {
    let fixture = Fixture::new();
    let mut host = fixture.adapter();
    assert_eq!(host.open_pr("RUN-122").unwrap(), PR);
    let merge_commit = fixture.owner_merges();
    fixture.view("MERGED", Some(&merge_commit));
    assert!(
        host.merged(PR).unwrap(),
        "owner merge must pass ancestry verification"
    );
    assert_eq!(
        receipt(&fixture)["merge_commit"],
        merge_commit,
        "Docs and final outcome need the verified merge SHA, not only a boolean gate result"
    );
    drop(host);
    let _restarted = fixture.adapter();
    assert_eq!(
        receipt(&fixture)["merge_commit"],
        merge_commit,
        "restart must retain the accepted merge identity for later steps"
    );
}

#[test]
fn unverified_merge_does_not_persist_a_commit_receipt() {
    let fixture = Fixture::new();
    let mut host = fixture.adapter();
    host.open_pr("RUN-122").unwrap();
    let candidate = git(&fixture.repo, &["rev-parse", "HEAD"]);
    fixture.view("OPEN", Some(&candidate));
    assert!(!host.merged(PR).unwrap());
    assert!(receipt(&fixture)["merge_commit"].is_null());
    fixture.view("MERGED", Some(&candidate));
    assert!(
        !host.merged(PR).unwrap(),
        "candidate has not reached the selected remote base"
    );
    assert!(
        receipt(&fixture)["merge_commit"].is_null(),
        "unverified ancestry must not supply Docs a merge identity"
    );
}
