//! Named, independently expected passing assertions are the positive gate.
#![cfg(unix)]
use hermon_flow::{checks::*, hub::Git, workflow::Workflow};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, process::Command};

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("/usr/bin/git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn report(assertions: &[(&str, &str)]) -> Value {
    let count = |status| assertions.iter().filter(|(_, s)| *s == status).count();
    json!({"success":true,"numPassedTests":count("passed"),"numFailedTests":count("failed"),
        "numPendingTests":count("pending")+count("skipped"),"numTodoTests":count("todo"),
        "numTotalTests":assertions.len(),"testResults":[{"assertionResults":assertions.iter()
            .map(|(id,status)|json!({"fullName":id,"status":status})).collect::<Vec<_>>()}]})
}
fn check(
    report: Value,
    exit: i32,
    format: TestReportFormat,
    expected: &[&str],
    baseline: &[&str],
    gate: &str,
) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "run"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(repo.join("base"), "base").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "base"]);
    let output = dir.path().join("report.json");
    fs::write(&output, serde_json::to_vec(&report).unwrap()).unwrap();
    let mut checks = GitChecks::new(CheckConfig {
        workdir: repo.clone(),
        base: git(&repo, &["rev-parse", "HEAD"]),
        ticket: "SYNTHETIC-1".into(),
        branch: "run".into(),
        log_dir: dir.path().join("logs"),
        env: BTreeMap::from([
            ("REPORT".into(), output.to_str().unwrap().into()),
            ("TEST_EXIT".into(), exit.to_string()),
        ]),
        paths: BTreeMap::new(),
        commands: BTreeMap::from([(
            "test".into(),
            CheckCommand {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "/bin/cat \"$REPORT\"; exit \"$TEST_EXIT\"".into(),
                ],
                timeout_seconds: 3,
                report: Some(format),
            },
        )]),
    })
    .unwrap();
    checks.set_attempt(AttemptContext {
        number: 1,
        bot: "fixture".into(),
        new_tests: expected.iter().map(|s| (*s).into()).collect(),
        baseline_failures: baseline.iter().map(|s| (*s).into()).collect(),
        ..Default::default()
    });
    checks
        .check("characterize", &[gate.into()], &BTreeMap::new())
        .is_ok()
}
fn positive(report: Value, expected: &[&str]) -> bool {
    check(
        report,
        0,
        TestReportFormat::VitestJson,
        expected,
        &[],
        "tests_pass",
    )
}

#[test]
fn named_passes_accept_unrelated_skips_and_duplicate_baseline_names() {
    assert!(positive(
        report(&[
            ("required", "passed"),
            ("old", "pending"),
            ("baseline", "passed"),
            ("baseline", "passed")
        ]),
        &["required"]
    ));
}
#[test]
fn required_assertions_must_all_exist_pass_and_be_unambiguous() {
    for assertions in [
        vec![("other", "passed")],
        vec![("required", "pending")],
        vec![("required", "todo")],
        vec![("required", "skipped")],
        vec![("required", "failed")],
        vec![("required", "passed"), ("required", "passed")],
        vec![("required", "passed"), ("required", "pending")],
    ] {
        assert!(
            !positive(report(&assertions), &["required"]),
            "{assertions:?}"
        );
    }
    assert!(!positive(
        report(&[("required", "passed")]),
        &["required", "missing"]
    ));
}
#[test]
fn no_expectations_or_baseline_failures_cannot_authorize_characterization() {
    let r = report(&[("required", "passed")]);
    assert!(!positive(r.clone(), &[]));
    assert!(!check(
        r,
        0,
        TestReportFormat::VitestJson,
        &["required"],
        &["old failure"],
        "tests_pass"
    ));
}
#[test]
fn exit_and_report_success_are_both_required() {
    let r = report(&[("required", "passed")]);
    assert!(!check(
        r.clone(),
        1,
        TestReportFormat::VitestJson,
        &["required"],
        &[],
        "tests_pass"
    ));
    let mut failed = r.clone();
    failed["success"] = json!(false);
    assert!(!positive(failed, &["required"]));
    let mut missing = r;
    missing.as_object_mut().unwrap().remove("success");
    assert!(!positive(missing, &["required"]));
}
#[test]
fn runtime_collection_count_and_unrelated_failures_remain_blocks() {
    let r = report(&[("required", "passed")]);
    let mut runtime = r.clone();
    runtime["numRuntimeErrorTestSuites"] = json!(1);
    let mut collection = r.clone();
    collection["testResults"][0]["message"] = json!("collection failed");
    let mut counts = r;
    counts["numTotalTests"] = json!(2);
    for bad in [
        runtime,
        collection,
        counts,
        report(&[("required", "passed"), ("old", "failed")]),
    ] {
        assert!(!positive(bad, &["required"]));
    }
}
#[test]
fn counts_only_report_cannot_prove_named_passing_assertions() {
    assert!(!check(
        json!({"passed":1,"failed":0,"ignored":0,"failures":[]}),
        0,
        TestReportFormat::Json,
        &["required"],
        &[],
        "tests_pass"
    ));
}
#[test]
fn strict_red_still_rejects_characterization_and_accepts_expected_failures() {
    assert!(!check(
        report(&[("required", "passed")]),
        0,
        TestReportFormat::VitestJson,
        &["required"],
        &[],
        "tests_fail"
    ));
    assert!(check(
        report(&[("required", "failed")]),
        1,
        TestReportFormat::VitestJson,
        &["required"],
        &[],
        "tests_fail"
    ));
}
#[test]
fn characterization_pack_keeps_owner_gates_test_scope_freeze_and_independent_review() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let pack = root.join("workflows/sdlc-characterization");
    let w = Workflow::load(&pack.join("workflow.yaml")).unwrap();
    assert_eq!(w.steps["owner_go"].next.as_deref(), Some("characterize"));
    assert_eq!(w.steps["merge"].gate.as_deref(), Some("owner_merge"));
    assert_eq!(
        w.steps["characterize"].check,
        ["only_paths_changed:tests", "lint", "tests_pass"]
    );
    assert_eq!(
        w.steps["verify"].check,
        [
            "only_paths_changed:tests",
            "frozen_unchanged:characterized",
            "tests_pass",
            "project_check"
        ]
    );
    assert_eq!(
        w.steps["verify"].on_block["test_defect"],
        "characterize_fix"
    );
    assert_eq!(w.steps["characterize_fix"].max_runs, Some(2));
    assert_eq!(
        w.steps["review"].require_different_family.as_deref(),
        Some("verify")
    );
    assert_eq!(w.steps["review"].max_rounds, Some(2));
    for (step, spec) in &w.steps {
        if spec.bot.is_some() {
            assert!(
                fs::read_to_string(pack.join("roles").join(format!("{step}.md")))
                    .unwrap()
                    .len()
                    > 100
            );
        }
    }
}
