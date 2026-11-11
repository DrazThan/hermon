//! Exact-head CI waiting, failure routing, selection, and doctor policy.
#![cfg(unix)]

#[path = "support/ci_fixture.rs"]
mod ci_fixture;

use ci_fixture::{
    FAIL_URL, Fixture, JOB_BUILD, JOB_LINT, JOB_UNSELECTED, RUN, SKIP_URL, UNSELECTED_URL,
    block_reason_before_route, check_run, passing_runs, routed, text,
};
use serde_json::json;
use std::{
    thread,
    time::{Duration, Instant},
};

fn drive_to_ci(fixture: &Fixture) -> String {
    fixture.start();
    let record = fixture.drive_to("ci", "Gate");
    fixture.remote_sha(fixture.branch(&record))
}

fn names_policy(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("code_host.ci")
        || lower.contains("selected_checks")
        || (lower.contains("ci") && lower.contains("policy"))
}

#[test]
fn ci_waits_silently_for_the_exact_pushed_head() {
    let fixture = Fixture::new();
    fixture.assert_slurp_rejected();
    let head = drive_to_ci(&fixture);
    let parent = ci_fixture::git(
        &fixture.worktree(&fixture.record()),
        &["rev-parse", "HEAD^"],
    );
    assert_ne!(parent, head);
    fixture.set_checks(&parent, passing_runs());
    fixture.set_checks(
        &head,
        json!([
            check_run(
                31,
                JOB_BUILD,
                "in_progress",
                None,
                "https://github.com/fixture/project/actions/runs/31"
            ),
            check_run(
                32,
                JOB_LINT,
                "queued",
                None,
                "https://github.com/fixture/project/actions/runs/32"
            )
        ]),
    );
    thread::sleep(Duration::from_millis(1100));
    for _ in 0..3 {
        let out = fixture.tick();
        assert!(out.status.success(), "hub: {}", text(&out));
        let record = fixture.record();
        assert_eq!(record["run"]["current"], "ci", "{record}");
        assert_eq!(record["run"]["status"], "Gate", "{record}");
    }
    assert!(
        fixture
            .comment_bodies()
            .iter()
            .all(|body| { !body.contains("needs attention") && !body.contains("reply GO") }),
        "pending CI must not alert the owner: {:?}",
        fixture.comment_bodies()
    );
    assert!(
        fixture.record()["run"]["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|alert| alert["step"] != "ci"),
        "pending CI must not record an owner alert"
    );
    let queries: Vec<_> = fixture
        .gh_argv()
        .into_iter()
        .filter(|argv| argv.iter().any(|arg| arg.contains("check-runs")))
        .collect();
    assert!(
        !queries.is_empty(),
        "CI must query check runs:\n{}",
        fixture.probes()
    );
    assert!(
        queries.iter().any(|argv| argv
            .iter()
            .any(|arg| arg.contains(&head) && arg.contains("filter=all"))),
        "check-runs query must address the pushed head with filter=all: {queries:?}"
    );
    assert!(
        queries.iter().all(|argv| {
            argv.iter().any(|arg| arg == "--paginate")
                && argv.iter().any(|arg| arg == "--jq")
                && argv
                    .windows(2)
                    .any(|pair| pair[0] == "--method" && pair[1] == "GET")
                && argv
                    .windows(2)
                    .any(|pair| pair[0] == "--hostname" && pair[1] == "github.com")
                && argv.iter().all(|arg| arg != "--slurp")
        }),
        "check-runs invocation must match the per-page contract: {queries:?}"
    );
    thread::sleep(Duration::from_millis(1100));
    fixture.set_checks(&head, passing_runs());
    let record = fixture.drive_to("review", "Running");
    assert_eq!(record["run"]["current"], "review");
    assert!(routed(&record, "ci", "review"), "{record}");
    assert!(!routed(&record, "ci", "green"), "{record}");
}

#[test]
fn ci_failure_routes_to_green_with_job_names_and_links() {
    let fixture = Fixture::new();
    let head = drive_to_ci(&fixture);
    thread::sleep(Duration::from_millis(1100));
    fixture.set_checks(
        &head,
        json!([
            check_run(41, JOB_BUILD, "completed", Some("failure"), FAIL_URL),
            check_run(
                42,
                JOB_LINT,
                "completed",
                Some("success"),
                "https://github.com/fixture/project/actions/runs/42"
            )
        ]),
    );
    let record = fixture.until(
        |record| record["run"]["current"] == "green" && record["run"]["status"] == "Running",
        Duration::from_secs(20),
    );
    let number = record["run"]["active"]["number"].as_u64().unwrap() as u32;
    assert!(
        number >= 2,
        "CI failure must start a new green attempt: {record}"
    );
    let task = fixture.task("green", number);
    assert!(task.contains(JOB_BUILD), "task missing job name:\n{task}");
    assert!(task.contains(FAIL_URL), "task missing check link:\n{task}");
    let reason =
        block_reason_before_route(&record, "ci", "green").expect("ci_failed must route to green");
    assert!(
        reason.contains(JOB_BUILD) && reason.contains(FAIL_URL),
        "{reason}"
    );
    fixture.release_transient();
    fixture.wait_attempt_finished("green", number);
    let out = fixture.tick();
    assert!(out.status.success(), "restarted hub: {}", text(&out));
    let next = fixture.task("green", number + 1);
    assert!(
        next.contains(JOB_BUILD) && next.contains(FAIL_URL),
        "CI reason must survive a transient worker failure and a new hub process:\n{next}"
    );
}

#[test]
fn ci_selection_uses_latest_run_per_name_and_rejects_skips() {
    let waiting = Fixture::new();
    let head = drive_to_ci(&waiting);
    thread::sleep(Duration::from_millis(1100));
    waiting.set_checks(
        &head,
        json!([
            check_run(
                1,
                JOB_BUILD,
                "completed",
                Some("success"),
                "https://github.com/fixture/project/actions/runs/1"
            ),
            check_run(
                5,
                JOB_BUILD,
                "in_progress",
                None,
                "https://github.com/fixture/project/actions/runs/5"
            ),
            check_run(
                3,
                JOB_LINT,
                "completed",
                Some("success"),
                "https://github.com/fixture/project/actions/runs/3"
            )
        ]),
    );
    for _ in 0..3 {
        let out = waiting.tick();
        assert!(out.status.success(), "{}", text(&out));
        let record = waiting.record();
        assert_eq!(
            record["run"]["current"], "ci",
            "higher-id pending rerun must wait: {record}"
        );
        assert_eq!(record["run"]["status"], "Gate");
    }
    thread::sleep(Duration::from_millis(1100));
    waiting.set_checks(
        &head,
        json!([
            check_run(1, JOB_BUILD, "completed", Some("failure"), FAIL_URL),
            check_run(
                9,
                JOB_BUILD,
                "completed",
                Some("success"),
                "https://github.com/fixture/project/actions/runs/9"
            ),
            check_run(
                3,
                JOB_LINT,
                "completed",
                Some("success"),
                "https://github.com/fixture/project/actions/runs/3"
            )
        ]),
    );
    let record = waiting.until(
        |record| {
            let current = record["run"]["current"].as_str().unwrap();
            current == "review" || current == "ready" || current == "merge"
        },
        Duration::from_secs(20),
    );
    assert!(
        routed(&record, "ci", "review"),
        "newest success must pass despite an older failure: {record}"
    );
    assert!(!routed(&record, "ci", "green"), "{record}");

    let skipped = Fixture::new();
    let head = drive_to_ci(&skipped);
    thread::sleep(Duration::from_millis(1100));
    skipped.set_checks(
        &head,
        json!([
            check_run(4, JOB_BUILD, "completed", Some("skipped"), SKIP_URL),
            check_run(
                5,
                JOB_LINT,
                "completed",
                Some("success"),
                "https://github.com/fixture/project/actions/runs/5"
            )
        ]),
    );
    let record = skipped.until(
        |record| routed(record, "ci", "green") || record["run"]["status"] == "Blocked",
        Duration::from_secs(20),
    );
    let reason = block_reason_before_route(&record, "ci", "green")
        .expect("skipped selected job must route as ci_failed");
    assert!(reason.contains(JOB_BUILD), "{reason}");
    assert!(reason.to_ascii_lowercase().contains("skipped"), "{reason}");
    assert!(reason.contains(SKIP_URL), "{reason}");
    assert_ne!(record["run"]["current"], "review");

    let unselected = Fixture::new();
    let head = drive_to_ci(&unselected);
    thread::sleep(Duration::from_millis(1100));
    unselected.set_checks(
        &head,
        json!([
            check_run(
                6,
                JOB_BUILD,
                "completed",
                Some("success"),
                "https://github.com/fixture/project/actions/runs/6"
            ),
            check_run(
                7,
                JOB_LINT,
                "completed",
                Some("success"),
                "https://github.com/fixture/project/actions/runs/7"
            ),
            check_run(
                8,
                JOB_UNSELECTED,
                "completed",
                Some("failure"),
                UNSELECTED_URL
            )
        ]),
    );
    let record = unselected.until(
        |record| {
            let current = record["run"]["current"].as_str().unwrap();
            current == "review"
                || current == "ready"
                || current == "merge"
                || record["run"]["status"] == "Blocked"
        },
        Duration::from_secs(20),
    );
    assert!(
        routed(&record, "ci", "review"),
        "an unselected failing job must not block: {record}"
    );
    assert!(!routed(&record, "ci", "green"), "{record}");
}

#[test]
fn ci_missing_checks_block_after_deadline() {
    let mut fixture = Fixture::new();
    fixture.set_ci_timeouts(1, 2, 8);
    fixture.start();
    let record = fixture.drive_to("ci", "Gate");
    fixture.weaken_live_no_checks_timeout(600);
    let pin = fs_read_policy(&fixture);
    assert!(
        pin.contains(JOB_BUILD) && pin.contains(JOB_LINT),
        "pinned CI policy must name the selected jobs: {pin}"
    );
    assert!(
        pin.contains("no_checks_timeout_seconds"),
        "pinned CI policy must record the no-checks deadline: {pin}"
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut saw_wait = record["run"]["status"] == "Gate";
    let record = loop {
        let current = fixture.record();
        if current["run"]["status"] == "Blocked" {
            break current;
        }
        assert_eq!(
            current["run"]["current"], "ci",
            "missing checks must not advance: {current}"
        );
        assert_eq!(current["run"]["status"], "Gate", "{current}");
        saw_wait = true;
        assert!(
            Instant::now() < deadline,
            "missing checks did not capability-block within the seconds deadline"
        );
        thread::sleep(Duration::from_millis(400));
        let out = fixture.tick();
        assert!(out.status.success(), "hub: {}", text(&out));
    };
    assert!(
        saw_wait,
        "the run must wait before the missing-check deadline"
    );
    assert_eq!(record["run"]["current"], "ci", "{record}");
    assert_eq!(record["run"]["block"]["kind"], "capability", "{record}");
    let reason = record["run"]["block"]["reason"].as_str().unwrap();
    assert!(reason.contains(JOB_BUILD), "{reason}");
    assert!(reason.contains(JOB_LINT), "{reason}");
    assert!(!routed(&record, "ci", "review"), "{record}");
    assert!(!routed(&record, "ci", "green"), "{record}");
}

fn fs_read_policy(fixture: &Fixture) -> String {
    std::fs::read_to_string(fixture.run_dir().join("context/ci-policy.json"))
        .unwrap_or_else(|error| panic!("intake must pin context/ci-policy.json: {error}"))
}

#[test]
fn ci_gate_cannot_be_approved_or_overridden_and_status_shows_wait() {
    let fixture = Fixture::new();
    let sha = drive_to_ci(&fixture);
    let url = fixture.record()["run"]["pr"]
        .as_str()
        .expect("draft PR URL must be recorded")
        .to_owned();
    assert!(
        url.contains("https://github.com/fixture/project/pull/"),
        "{url}"
    );
    let status = fixture.control(&["status", RUN]);
    assert!(status.status.success(), "{}", text(&status));
    let shown = text(&status);
    let lower = shown.to_ascii_lowercase();
    assert!(
        lower.contains("wait"),
        "status must show the CI wait:\n{shown}"
    );
    assert!(
        shown.contains(&sha),
        "status must show the expected head:\n{shown}"
    );
    assert!(
        shown.contains(&url),
        "status must show the PR URL:\n{shown}"
    );
    let json_status = fixture.control(&["status", RUN, "--json"]);
    assert!(json_status.status.success(), "{}", text(&json_status));
    let body = text(&json_status);
    assert!(
        body.contains(&sha),
        "JSON status must include the head:\n{body}"
    );
    assert!(
        body.contains(&url),
        "JSON status must include the PR URL:\n{body}"
    );
    let approve = fixture.control(&["approve", RUN]);
    assert!(
        !approve.status.success(),
        "approve must be refused at CI: {}",
        text(&approve)
    );
    let over = fixture.control(&["override", RUN, "ci", "--reason", "owner tries to skip CI"]);
    assert!(
        !over.status.success(),
        "override must be refused at CI: {}",
        text(&over)
    );
    let record = fixture.record();
    assert_eq!(record["run"]["current"], "ci", "{record}");
    assert_eq!(record["run"]["status"], "Gate", "{record}");
    assert!(
        record["run"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["kind"] != "override" && event["kind"] != "approve"),
        "{record}"
    );
}

#[test]
fn ci_project_needs_policy_and_doctor_skips_unused_project_check() {
    let mut legacy = Fixture::legacy();
    legacy.with_failing_project_check();
    let doctor = legacy.doctor();
    assert!(
        !doctor.status.success(),
        "legacy doctor must still run the failing project_check: {}",
        text(&doctor)
    );
    let output = text(&doctor);
    assert!(
        output.contains("project_check"),
        "legacy doctor failure must name project_check: {output}"
    );
    assert!(
        legacy.check_log().contains("project_check"),
        "legacy doctor must invoke project_check, log was: {}",
        legacy.check_log()
    );

    let missing = Fixture::without_policy();
    let doctor = missing.doctor();
    assert!(
        !doctor.status.success(),
        "a CI workflow without code_host.ci must fail doctor"
    );
    let output = text(&doctor);
    assert!(
        names_policy(&output),
        "doctor must name the CI policy, got: {output}"
    );
    let workflow = missing.workflow_str().to_owned();
    let project = missing.project_str().to_owned();
    let data = missing.data.to_str().unwrap().to_owned();
    let intake = missing.cli(&[
        "run",
        &workflow,
        ci_fixture::TICKET,
        "--project",
        &project,
        "--data-dir",
        &data,
        "--confirm",
        "--run-id",
        RUN,
    ]);
    assert!(
        !intake.status.success(),
        "intake must reject a CI workflow without a policy"
    );
    assert!(
        names_policy(&text(&intake)),
        "intake must name the CI policy, got: {}",
        text(&intake)
    );

    let mut ready = Fixture::new();
    ready.with_failing_project_check();
    let doctor = ready.doctor();
    assert!(
        doctor.status.success(),
        "unused project_check must not fail CI doctor: {}",
        text(&doctor)
    );
    assert!(
        !ready.check_log().contains("project_check"),
        "CI doctor must not invoke an unused project_check: {}",
        ready.check_log()
    );
    ready.start();
    assert!(
        !ready.check_log().contains("project_check"),
        "CI intake must not invoke an unused project_check: {}",
        ready.check_log()
    );
}
