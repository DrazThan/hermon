//! End-to-end acceptance for the shipped sdlc-light pack.
#![cfg(unix)]

#[path = "support/light_fixture.rs"]
mod light_fixture;

use hermon::flow_ui::FlowAlerts;
use hermon::notify::NotifyCfg;
use hermon_flow::control::RunRecord;
use light_fixture::{
    CORRECTION, FAIL_URL, Fixture, JOB, PASS_URL, PLAN_TOKEN, check_run, is_pr, no_force,
    sha256_file, text,
};
use serde_json::json;
use std::{fs, thread, time::Duration};

fn at_ci(fixture: &Fixture) -> String {
    let record = fixture.until(
        |record| record["run"]["current"] == "ci" && record["run"]["status"] == "Gate",
        Duration::from_secs(40),
    );
    fixture.remote_sha(fixture.branch(&record))
}

fn no_pending_alert(fixture: &Fixture) {
    let record: RunRecord =
        serde_json::from_slice(&fs::read(fixture.run_dir().join("run.json")).unwrap()).unwrap();
    let fired = FlowAlerts::default()
        .poll(&record, u64::MAX / 4, &NotifyCfg::default(), false)
        .unwrap();
    assert!(
        fired.is_empty(),
        "pending CI must not desktop-alert: {fired:?}"
    );
    assert!(
        fixture
            .comment_bodies()
            .iter()
            .all(|body| { !body.contains("needs attention") && !body.contains("reply GO") }),
        "pending CI must not alert the owner: {:?}",
        fixture.comment_bodies()
    );
}

fn red_note(fixture: &Fixture, record: &serde_json::Value) -> (String, String) {
    let worktree = fixture.worktree(record);
    let reference = format!("refs/tags/hermon/{}/red", fixture.run_id);
    let object = light_fixture::git(
        &worktree,
        &["rev-parse", "--verify", &format!("{reference}^{{tag}}")],
    );
    let note = light_fixture::git(&worktree, &["notes", "--ref=hermon", "show", &reference]);
    (object, note)
}

#[test]
fn light_run_reaches_draft_retries_ci_reviews_amendment_and_observes_merge() {
    let fixture = Fixture::pack();
    fixture.set_review_mode("exact");
    fixture.start();
    let opened = fixture.until(
        |record| record["run"]["current"] == "ci" && record["run"]["status"] == "Gate",
        Duration::from_secs(40),
    );
    let first = fixture.remote_sha(fixture.branch(&opened));
    let body = fixture.pr_body();
    assert!(
        body.contains(PLAN_TOKEN),
        "draft body must contain the full plan:\n{body}"
    );
    assert!(
        body.contains("Review pending"),
        "draft must say Review pending before review:\n{body}"
    );
    assert!(
        body.to_ascii_lowercase().contains("amend")
            && !body.to_ascii_lowercase().contains("not applicable"),
        "a nonempty test amendment must be warned in the draft, not dismissed:\n{body}"
    );
    assert_eq!(fixture.gh_state()["prs"].as_array().unwrap().len(), 1);
    assert_eq!(
        fixture
            .gh_argv()
            .iter()
            .filter(|argv| is_pr(argv, "create"))
            .count(),
        1
    );
    assert!(
        !fixture.attempt_dir("review", 1).join("task.md").exists(),
        "draft must open before review"
    );
    let (red_object, red_bytes) = red_note(&fixture, &opened);
    let red_tree = light_fixture::git(
        &fixture.worktree(&opened),
        &[
            "show",
            &format!("hermon/{}/red:tests/light_acceptance.rs", fixture.run_id),
        ],
    );
    assert!(
        !red_tree.contains(CORRECTION),
        "original RED commit must not contain the later correction"
    );
    no_force(&fixture);
    assert!(
        fixture.check_log().is_empty(),
        "project_check must not run: {}",
        fixture.check_log()
    );

    fixture.set_checks(
        &first,
        json!([check_run(41, "completed", Some("failure"), FAIL_URL)]),
    );
    let retried = fixture.until(
        |record| {
            record["run"]["current"] == "ci"
                && record["run"]["status"] == "Gate"
                && fixture.remote_sha(fixture.branch(record)) != first
        },
        Duration::from_secs(40),
    );
    let second = fixture.remote_sha(fixture.branch(&retried));
    assert_ne!(second, first);
    let green_task = fixture.task("green", 2);
    assert!(
        green_task.contains(JOB) && green_task.contains(FAIL_URL),
        "CI failure must reach the next GREEN task:\n{green_task}"
    );
    no_force(&fixture);
    assert_eq!(fixture.gh_state()["prs"].as_array().unwrap().len(), 1);

    fixture.set_checks(
        &second,
        json!([check_run(51, "in_progress", None, PASS_URL)]),
    );
    let waiting = fixture.tick();
    assert!(waiting.status.success(), "hub: {}", text(&waiting));
    let waiting_record = fixture.record();
    assert_eq!(waiting_record["run"]["current"], "ci", "{waiting_record}");
    assert_eq!(waiting_record["run"]["status"], "Gate", "{waiting_record}");
    no_pending_alert(&fixture);
    thread::sleep(Duration::from_millis(1100));
    fixture.set_checks(
        &second,
        json!([check_run(52, "completed", Some("success"), PASS_URL)]),
    );
    let reviewed = fixture.until(
        |record| record["run"]["current"] == "merge" && record["run"]["status"] == "Gate",
        Duration::from_secs(40),
    );
    let diff = fixture.attempt_dir("review", 1).join("amendments.diff");
    let digest = sha256_file(&diff);
    let diff_bytes = fs::read(&diff).unwrap();
    assert!(
        diff_bytes
            .windows(CORRECTION.len())
            .any(|window| window == CORRECTION.as_bytes()),
        "review must see the correction in the host diff"
    );
    let task = fixture.task("review", 1);
    assert!(
        task.contains(&digest),
        "review task missing digest:\n{task}"
    );
    let result: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.attempt_dir("review", 1).join("result.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(result["accepted_amendments"], digest);
    assert!(
        fixture.gh_argv().iter().any(|argv| is_pr(argv, "ready")),
        "accepted digest must ready the PR"
    );
    assert!(
        fixture.code_host_state().to_string().contains(&digest),
        "ready receipt must bind {digest}"
    );
    assert_eq!(fixture.gh_state()["prs"][0]["isDraft"], false);
    let (later_object, later_note) = red_note(&fixture, &reviewed);
    assert_eq!(later_object, red_object);
    assert_eq!(later_note, red_bytes);
    no_force(&fixture);
    assert!(fixture.gh_argv().iter().all(|argv| !is_pr(argv, "merge")));
    assert!(
        fixture
            .events()
            .iter()
            .all(|event| event["step"] != "owner_go" && event["reason"] != "owner_go"),
        "light run must not wait for Owner GO"
    );

    let head = fixture.remote_sha(fixture.branch(&reviewed));
    assert_eq!(head, second, "ready must still authorize the reviewed head");
    let merge = fixture.owner_merge(&head);
    fixture.mark_merged(&merge);
    let done = fixture.until(
        |record| record["run"]["status"] == "Done",
        Duration::from_secs(30),
    );
    assert!(
        done["run"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["step"] == "docs" && event["kind"] == "step_done"),
        "docs must complete: {done}"
    );
    let receipt: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.run_dir().join("context/merge.json")).expect("merge receipt"),
    )
    .unwrap();
    assert_eq!(receipt["merge_commit"], merge);
    let outcomes: Vec<_> = fixture
        .comment_bodies()
        .into_iter()
        .filter(|body| body.contains("Merge commit:"))
        .collect();
    assert_eq!(outcomes.len(), 1, "exactly one outcome: {outcomes:?}");
    assert!(fixture.check_log().is_empty());
    assert_subsequence(&fixture.events());
}

fn assert_subsequence(events: &[serde_json::Value]) {
    let pairs: Vec<(&str, &str)> = events
        .iter()
        .map(|event| {
            (
                event["kind"].as_str().unwrap_or(""),
                event["step"].as_str().unwrap_or(""),
            )
        })
        .collect();
    let expected = [
        ("step_done", "plan"),
        ("step_done", "red"),
        ("step_done", "green"),
        ("step_done", "draft_pr"),
        ("route", "ci"),
        ("step_done", "green"),
        ("step_done", "review"),
        ("step_done", "ready"),
        ("step_done", "docs"),
    ];
    let mut index = 0;
    for (kind, step) in &pairs {
        if index < expected.len() && *kind == expected[index].0 && *step == expected[index].1 {
            index += 1;
        }
    }
    assert_eq!(
        index,
        expected.len(),
        "event order missing {:?} after {index} items: {pairs:?}",
        expected.get(index)
    );
    assert!(
        events.iter().any(|event| {
            event["kind"] == "route" && event["step"] == "ci" && event["reason"] == "green"
        }),
        "CI failure must route to green"
    );
}

#[test]
fn light_lost_ready_response_recovers_once() {
    let fixture = Fixture::pack();
    fixture.set_review_mode("exact");
    fixture.arm_drop_ready();
    fixture.start();
    let first = at_ci(&fixture);
    fixture.set_checks(
        &first,
        json!([check_run(61, "completed", Some("failure"), FAIL_URL)]),
    );
    let second = fixture.until(
        |record| {
            record["run"]["current"] == "ci"
                && record["run"]["status"] == "Gate"
                && fixture.remote_sha(fixture.branch(record)) != first
        },
        Duration::from_secs(40),
    );
    let sha = fixture.remote_sha(fixture.branch(&second));
    fixture.set_checks(
        &sha,
        json!([check_run(62, "completed", Some("success"), PASS_URL)]),
    );
    let waiting = fixture.until(
        |record| {
            let current = record["run"]["current"].as_str().unwrap();
            let status = record["run"]["status"].as_str().unwrap();
            (current == "merge" && status == "Gate") || (current == "ready" && status == "Blocked")
        },
        Duration::from_secs(40),
    );
    if waiting["run"]["current"] == "ready" {
        let recovered = fixture.tick();
        assert!(
            recovered.status.success(),
            "restarted host must reconcile the lost ready response: {}",
            text(&recovered)
        );
    }
    let extra = fixture.tick();
    assert!(
        extra.status.success(),
        "second host restart: {}",
        text(&extra)
    );
    let record = fixture.record();
    assert_eq!(record["run"]["current"], "merge", "{record}");
    assert_eq!(record["run"]["status"], "Gate", "{record}");
    let state = fixture.gh_state();
    assert_eq!(state["prs"].as_array().unwrap().len(), 1);
    assert_eq!(state["prs"][0]["isDraft"], false);
    let ready_calls = fixture
        .gh_argv()
        .iter()
        .filter(|argv| is_pr(argv, "ready"))
        .count();
    assert!(
        (1..=2).contains(&ready_calls),
        "lost ready must reconcile without a second PR or repeated readiness effect: {ready_calls}\n{}",
        fixture.probes()
    );
    let receipt = fixture.code_host_state().to_string();
    let again = fixture.tick();
    assert!(again.status.success(), "idempotent tick: {}", text(&again));
    assert_eq!(fixture.code_host_state().to_string(), receipt);
    assert_eq!(fixture.gh_state()["prs"].as_array().unwrap().len(), 1);
    let head = fixture.remote_sha(fixture.branch(&record));
    let merge = fixture.owner_merge(&head);
    fixture.mark_merged(&merge);
    let done = fixture.until(
        |record| record["run"]["status"] == "Done",
        Duration::from_secs(30),
    );
    assert_eq!(done["run"]["status"], "Done");
    assert!(fixture.gh_argv().iter().all(|argv| !is_pr(argv, "merge")));
}

#[test]
fn light_post_review_head_change_cannot_authorize_merge() {
    let fixture = Fixture::pack();
    fixture.set_review_mode("exact");
    fixture.start();
    let first = at_ci(&fixture);
    fixture.set_checks(
        &first,
        json!([check_run(71, "completed", Some("success"), PASS_URL)]),
    );
    let waiting = fixture.until(
        |record| record["run"]["current"] == "merge" && record["run"]["status"] == "Gate",
        Duration::from_secs(40),
    );
    let authorized = fixture.remote_sha(fixture.branch(&waiting));
    let later = fixture.push_external(fixture.branch(&waiting));
    assert_ne!(later, authorized);
    let merge = fixture.owner_merge(&later);
    fixture.mark_merged(&merge);
    let record = fixture.until(
        |record| {
            let status = record["run"]["status"].as_str().unwrap();
            status == "Blocked" || status == "Done" || record["run"]["current"] == "docs"
        },
        Duration::from_secs(30),
    );
    assert_eq!(record["run"]["status"], "Blocked", "{record}");
    assert_ne!(record["run"]["current"], "docs", "{record}");
    assert_eq!(record["run"]["block"]["kind"], "capability", "{record}");
    assert!(
        record["run"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["step"] != "docs"),
        "a later merged head must not start docs: {record}"
    );
    assert!(
        fixture
            .comment_bodies()
            .iter()
            .all(|body| !body.contains("Merge commit:")),
        "a changed head must not post a completed outcome"
    );
}
