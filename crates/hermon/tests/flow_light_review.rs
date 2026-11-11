//! Amendment review acceptance and host-generated test diffs.
#![cfg(unix)]

#[path = "support/light_fixture.rs"]
mod light_fixture;

use light_fixture::{
    CORRECTION, FORGED, Fixture, PASS_URL, PLAN_TOKEN, UNACCEPTED_ID, check_run, is_pr, no_force,
    sha256_file, text,
};
use serde_json::{Value, json};
use std::{fs, time::Duration};

fn pass_ci(fixture: &Fixture) {
    let record = fixture.until(
        |record| record["run"]["current"] == "ci" && record["run"]["status"] == "Gate",
        Duration::from_secs(30),
    );
    let sha = fixture.remote_sha(fixture.branch(&record));
    fixture.set_checks(
        &sha,
        json!([check_run(11, "completed", Some("success"), PASS_URL)]),
    );
}

#[test]
fn clean_review_requires_current_amendment_acceptance() {
    let negative = Fixture::enforce("REVIEW-NEG");
    negative.set_review_mode("negative");
    negative.start();
    pass_ci(&negative);
    let blocked = negative.until(
        |record| record["run"]["status"] == "Blocked",
        Duration::from_secs(40),
    );
    assert_eq!(
        blocked["run"]["current"], "review",
        "clean without a digest, then a wrong digest, must block at review: {blocked}"
    );
    assert!(
        negative.gh_argv().iter().all(|argv| !is_pr(argv, "ready")),
        "a generic clean or wrong digest must not ready the PR:\n{}",
        negative.probes()
    );
    let again = negative.tick();
    assert!(again.status.success(), "restarted hub: {}", text(&again));
    let persisted = negative.record();
    assert_eq!(persisted["run"]["status"], "Blocked", "{persisted}");
    assert_eq!(persisted["run"]["current"], "review", "{persisted}");
    assert!(
        persisted["run"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "check_retry"),
        "missing digest must retry once before blocking: {persisted}"
    );

    let positive = Fixture::enforce("REVIEW-OK");
    positive.set_review_mode("exact");
    positive.start();
    pass_ci(&positive);
    let ready = positive.until(
        |record| record["run"]["current"] == "merge" && record["run"]["status"] == "Gate",
        Duration::from_secs(40),
    );
    assert_eq!(ready["run"]["current"], "merge");
    let diff = positive.attempt_dir("review", 1).join("amendments.diff");
    let digest = sha256_file(&diff);
    let result: Value = serde_json::from_slice(
        &fs::read(positive.attempt_dir("review", 1).join("result.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(result["verdict"], "clean");
    assert_eq!(result["accepted_amendments"], digest);
    assert!(
        positive.gh_argv().iter().any(|argv| is_pr(argv, "ready")),
        "exact digest must authorize ready"
    );
    let host = positive.code_host_state().to_string();
    assert!(
        host.contains(&digest),
        "ready receipt must bind the accepted amendment digest {digest}: {host}"
    );
    assert!(positive.pr_body().contains(PLAN_TOKEN));
    no_force(&positive);
}

#[test]
fn reviewer_receives_host_generated_complete_test_diff() {
    let fixture = Fixture::materialize();
    fixture.start();
    let unaccepted = fixture.run_dir().join("steps/plan/2");
    fs::create_dir_all(&unaccepted).unwrap();
    fs::write(
        unaccepted.join("test-expectations.json"),
        serde_json::to_vec(&json!({
            "new_tests": [UNACCEPTED_ID],
            "baseline_failures": []
        }))
        .unwrap(),
    )
    .unwrap();
    let record = fixture.until(
        |record| {
            fixture
                .attempt_dir("review", 1)
                .join("amendments.diff")
                .is_file()
                || record["run"]["current"] == "review"
                || record["run"]["events"]
                    .as_array()
                    .is_some_and(|events| events.iter().any(|event| event["step"] == "review"))
        },
        Duration::from_secs(30),
    );
    let dir = fixture.attempt_dir("review", 1);
    let diff_path = dir.join("amendments.diff");
    assert!(
        diff_path.is_file(),
        "absent amendments.diff in {}",
        diff_path.display()
    );
    let diff = fs::read(&diff_path).unwrap();
    let digest = sha256_file(&diff_path);
    assert!(
        diff.windows(CORRECTION.len())
            .any(|window| window == CORRECTION.as_bytes()),
        "host diff must include the correction line"
    );
    assert!(
        diff.windows(FORGED.len())
            .any(|window| window == FORGED.as_bytes()),
        "host diff must be the complete test-path delta, including the worker-forged file"
    );
    let receipt: Value =
        serde_json::from_slice(&fs::read(dir.join("amendments.json")).unwrap()).unwrap();
    assert_eq!(receipt["diff_sha256"], digest);
    assert_eq!(receipt["empty"], false);
    let worktree = fixture.worktree(&record);
    let red = light_fixture::git(
        &worktree,
        &[
            "rev-parse",
            &format!("refs/tags/hermon/{}/red^{{commit}}", fixture.run_id),
        ],
    );
    let head = light_fixture::git(&worktree, &["rev-parse", "HEAD"]);
    assert_eq!(receipt["red_commit"], red);
    assert_eq!(receipt["candidate_sha"], head);
    let paths = receipt["paths"].as_array().expect("amendment paths");
    assert!(
        paths.iter().any(|path| path == "tests/light_acceptance.rs"),
        "paths must name the corrected test: {receipt}"
    );
    let task = fs::read_to_string(dir.join("task.md")).unwrap();
    assert!(
        task.contains(&digest),
        "task.md must contain the host digest:\n{task}"
    );
    assert!(
        task.contains(&fixture.expected_id),
        "task missing accepted ID:\n{task}"
    );
    assert!(
        !task.contains(UNACCEPTED_ID),
        "unaccepted planner directory must not redefine IDs:\n{task}"
    );
    let forged = fs::read(dir.join("worker-forged-amendments.diff")).unwrap_or_default();
    assert_ne!(
        sha256_file_bytes(&forged),
        digest,
        "a worker artifact must not replace the host diff"
    );
    assert!(
        fs::read_dir(&dir).unwrap().all(|entry| {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            !name.ends_with(".patch") && name != "patches"
        }),
        "review attempt must not grow a per-commit patch archive"
    );
    assert!(fixture.pr_body().contains(PLAN_TOKEN));
    no_force(&fixture);
}

fn sha256_file_bytes(bytes: &[u8]) -> String {
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), bytes).unwrap();
    sha256_file(file.path())
}
