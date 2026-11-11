//! Draft publication, fast-forward updates, ready, and owner-merge head binding.
#![cfg(unix)]

#[path = "support/ci_fixture.rs"]
mod ci_fixture;

use ci_fixture::{
    Fixture, PLAN, REVIEW_SUMMARY, RUN, assert_no_force, assert_no_pr_merge, git, is_pr,
    passing_runs, routed, text,
};
use serde_json::Value;
use std::{fs, thread, time::Duration};

fn drive_to_ci(fixture: &Fixture) -> (Value, String) {
    fixture.start();
    let record = fixture.drive_to("ci", "Gate");
    let sha = fixture.remote_sha(fixture.branch(&record));
    (record, sha)
}

fn pr_body(fixture: &Fixture) -> String {
    fixture.gh_state()["prs"][0]["body"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn draft_pr_opens_after_first_green_with_plan_and_no_evidence_refs() {
    let fixture = Fixture::new();
    fixture.assert_slurp_rejected();
    let (record, remote_head) = drive_to_ci(&fixture);
    let worktree = fixture.worktree(&record);
    assert_eq!(
        remote_head,
        git(&worktree, &["rev-parse", "HEAD"]),
        "remote branch head must be the accepted GREEN commit"
    );
    let state = fixture.gh_state();
    assert_eq!(state["prs"].as_array().unwrap().len(), 1, "{state}");
    let creates: Vec<_> = fixture
        .gh_argv()
        .into_iter()
        .filter(|argv| is_pr(argv, "create"))
        .collect();
    assert_eq!(
        creates.len(),
        1,
        "exactly one draft create:\n{}",
        fixture.probes()
    );
    for flag in [
        "--draft",
        "--repo",
        "--base",
        "--head",
        "--title",
        "--body-file",
    ] {
        assert!(
            creates[0].iter().any(|arg| arg == flag),
            "pr create missing {flag}: {:?}",
            creates[0]
        );
    }
    let body = pr_body(&fixture);
    assert!(
        body.contains(PLAN),
        "draft body must contain the full plan text:\n{body}"
    );
    assert!(
        body.contains("Review pending"),
        "draft body must say Review pending:\n{body}"
    );
    assert!(
        !body.contains(REVIEW_SUMMARY),
        "draft must not include the review summary before review:\n{body}"
    );
    let refs = fixture.bare_refs();
    assert!(
        !refs
            .lines()
            .any(|r| r.starts_with("refs/tags/hermon") || r == "refs/notes/hermon"),
        "evidence tags and notes must stay local:\n{refs}"
    );
    assert!(
        fixture
            .gh_argv()
            .iter()
            .all(|argv| !is_pr(argv, "ready") && !is_pr(argv, "review")),
        "review/ready gh calls are too early:\n{}",
        fixture.probes()
    );
    assert_no_pr_merge(&fixture);
    assert_eq!(record["run"]["current"], "ci");
    let viewed = fixture.gh_view(1);
    assert_eq!(viewed["isDraft"], true);
    assert_eq!(viewed["headRefOid"], remote_head);
    assert_eq!(viewed["state"], "OPEN");
}

#[test]
fn ci_failure_fix_is_pushed_fast_forward_to_the_same_pr() {
    let fixture = Fixture::new();
    let (_record, old) = drive_to_ci(&fixture);
    fixture.set_checks(
        &old,
        serde_json::json!([
            ci_fixture::check_run(
                21,
                ci_fixture::JOB_BUILD,
                "completed",
                Some("failure"),
                ci_fixture::FAIL_URL
            ),
            ci_fixture::check_run(
                22,
                ci_fixture::JOB_LINT,
                "completed",
                Some("success"),
                "https://github.com/fixture/project/actions/runs/22"
            )
        ]),
    );
    fixture.release_transient();
    let record = fixture.until(
        |record| {
            let current = record["run"]["current"].as_str().unwrap();
            let state = record["run"]["status"].as_str().unwrap();
            if state == "Blocked" {
                panic!("blocked at {current}: {}", record["run"]["block"]);
            }
            current == "ci" && state == "Gate" && fixture.remote_sha(fixture.branch(record)) != old
        },
        Duration::from_secs(20),
    );
    let new = fixture.remote_sha(fixture.branch(&record));
    git(
        &fixture.worktree(&record),
        &["merge-base", "--is-ancestor", &old, &new],
    );
    assert_eq!(fixture.gh_state()["prs"].as_array().unwrap().len(), 1);
    assert_eq!(
        fixture
            .gh_argv()
            .iter()
            .filter(|argv| is_pr(argv, "create"))
            .count(),
        1,
        "the fix must update the same PR:\n{}",
        fixture.probes()
    );
    assert_no_force(&fixture);
    assert_no_pr_merge(&fixture);
}

#[test]
fn ready_updates_body_and_marks_ready_only_after_clean_review() {
    let fixture = Fixture::new();
    let (_record, sha) = drive_to_ci(&fixture);
    thread::sleep(Duration::from_millis(1100));
    fixture.set_checks(&sha, passing_runs());
    let record = fixture.drive_to("merge", "Gate");
    let argv = fixture.gh_argv();
    let edit = argv
        .iter()
        .position(|args| is_pr(args, "edit"))
        .expect("ready must update the body with pr edit");
    let ready = argv
        .iter()
        .position(|args| is_pr(args, "ready"))
        .expect("ready must call pr ready");
    assert!(edit < ready, "pr edit must precede pr ready:\n{argv:?}");
    for args in argv
        .iter()
        .filter(|args| is_pr(args, "edit") || is_pr(args, "ready"))
    {
        assert!(
            args.iter().any(|arg| arg == "--repo"),
            "missing --repo: {args:?}"
        );
    }
    assert!(
        argv.iter()
            .filter(|args| is_pr(args, "edit"))
            .any(|args| args.iter().any(|arg| arg == "--body-file")),
        "pr edit must use --body-file"
    );
    let body = pr_body(&fixture);
    assert!(
        body.contains(REVIEW_SUMMARY),
        "ready body must contain the review summary:\n{body}"
    );
    assert!(
        body.contains(PLAN),
        "ready body must still contain the plan:\n{body}"
    );
    let viewed = fixture.gh_view(1);
    assert_eq!(viewed["isDraft"], false, "{viewed}");
    assert_eq!(viewed["headRefOid"], sha);
    assert_no_pr_merge(&fixture);
    assert_eq!(record["run"]["current"], "merge");
    assert!(
        !routed(&record, "ready", "docs") && record["run"]["current"] != "docs",
        "ready must not skip owner_merge"
    );
}

#[test]
fn merge_gate_accepts_only_the_reviewed_head() {
    let accepted = Fixture::new();
    let (_record, reviewed) = drive_to_ci(&accepted);
    thread::sleep(Duration::from_millis(1100));
    accepted.set_checks(&reviewed, passing_runs());
    accepted.drive_to("merge", "Gate");
    let merge = accepted.owner_merge(&reviewed);
    accepted.mark_merged(&merge);
    let record = accepted.until(
        |record| {
            let status = record["run"]["status"].as_str().unwrap();
            status == "Done" || status == "Blocked"
        },
        Duration::from_secs(20),
    );
    assert_eq!(record["run"]["status"], "Done", "{record}");
    assert_eq!(record["run"]["current"], "docs");
    assert!(
        record["run"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["step"] == "docs" && event["kind"] == "step_done"),
        "docs must complete: {record}"
    );
    let receipt: Value = serde_json::from_slice(
        &fs::read(accepted.run_dir().join("context/merge.json")).expect("merge receipt"),
    )
    .unwrap();
    assert_eq!(receipt["merge_commit"], merge);
    assert_no_pr_merge(&accepted);

    let rejected = Fixture::new();
    let (waiting, reviewed) = drive_to_ci(&rejected);
    thread::sleep(Duration::from_millis(1100));
    rejected.set_checks(&reviewed, passing_runs());
    rejected.drive_to("merge", "Gate");
    let later = rejected.push_external_commit(rejected.branch(&waiting));
    assert_ne!(later, reviewed);
    let merge = rejected.owner_merge(&later);
    rejected.mark_merged(&merge);
    let record = rejected.until(
        |record| {
            let status = record["run"]["status"].as_str().unwrap();
            status == "Blocked" || status == "Done" || record["run"]["current"] == "docs"
        },
        Duration::from_secs(20),
    );
    assert_ne!(record["run"]["current"], "docs", "{record}");
    assert_ne!(record["run"]["status"], "Done", "{record}");
    assert_eq!(record["run"]["status"], "Blocked", "{record}");
    assert_eq!(record["run"]["block"]["kind"], "capability", "{record}");
    assert!(
        record["run"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["step"] != "docs"),
        "a later merged head must not start docs: {record}"
    );
}

#[test]
fn override_cannot_skip_draft_or_ready() {
    let fixture = Fixture::new();
    fixture.fail_command("pr ready");
    let (_record, sha) = drive_to_ci(&fixture);
    thread::sleep(Duration::from_millis(1100));
    fixture.set_checks(&sha, passing_runs());
    let record = fixture.until(
        |record| record["run"]["current"] == "ready" && record["run"]["status"] == "Blocked",
        Duration::from_secs(20),
    );
    assert_eq!(record["run"]["current"], "ready");
    let out = fixture.control(&[
        "override",
        RUN,
        "ready",
        "--reason",
        "owner tries to skip ready",
    ]);
    assert!(
        !out.status.success(),
        "override of ready must be refused: {}",
        text(&out)
    );
    let after = fixture.record();
    assert_eq!(after["run"]["current"], "ready", "{after}");
    assert_eq!(after["run"]["status"], "Blocked", "{after}");
    assert!(
        after["run"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["kind"] != "override"),
        "refused override must not be recorded: {after}"
    );
    assert_no_pr_merge(&fixture);
    assert!(!routed(&after, "ready", "merge"), "{after}");
}
