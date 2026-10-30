//! Independently authored RED report rejection cases for issue #125.
#![cfg(unix)]
#[path = "support/vitest_fixture.rs"]
mod fixture;
use serde_json::{Value, json};

fn expectations() -> Value {
    json!({"new_tests":["suite new case"],"baseline_failures":[]})
}
fn rejects(report: Value, exit: i32, expected: Value) {
    let record = fixture::exercise(report, exit, expected);
    assert_eq!(
        record["run"]["status"], "Blocked",
        "invalid RED must produce an owner-visible block: {record}"
    );
    assert!(
        record["run"]["freezes"].get("red").is_none(),
        "invalid RED cannot freeze tests: {record}"
    );
    assert!(
        record["run"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "blocked" || event["kind"] == "check_failed"),
        "rejection must remain observable: {record}"
    );
}

#[test]
fn vitest_suite_error_cannot_pass_red_with_an_expected_failed_assertion() {
    let mut report = fixture::valid_report();
    report["testResults"][0]["message"] = json!("Error: beforeAll failed while loading fixture");
    rejects(report, 1, expectations());
}

#[test]
fn vitest_failed_suite_without_failed_assertions_cannot_pass_red() {
    let mut report = fixture::valid_report();
    report["numFailedTests"] = json!(0);
    report["numPassedTests"] = json!(0);
    report["numTotalTests"] = json!(0);
    report["testResults"][0]["assertionResults"] = json!([]);
    report["testResults"][0]["message"] = json!("Cannot find module imported by test");
    rejects(report, 1, expectations());
}

#[test]
fn vitest_inconsistent_assertion_counts_cannot_pass_red() {
    let mut report = fixture::valid_report();
    report["numFailedTests"] = json!(99);
    rejects(report, 1, expectations());
}

#[test]
fn vitest_success_exit_cannot_pass_red_even_when_json_contains_failures() {
    rejects(fixture::valid_report(), 0, expectations());
}

#[test]
fn vitest_failure_named_only_in_summary_cannot_pass_red() {
    let mut report = fixture::valid_report();
    report["testResults"][0]["assertionResults"][0]["fullName"] =
        json!("suite an unrelated old failure");
    rejects(report, 1, expectations());
}

#[test]
fn vitest_baseline_failure_cannot_be_reclassified_as_new_red() {
    rejects(
        fixture::valid_report(),
        1,
        json!({"new_tests":["suite new case"],"baseline_failures":["suite new case"]}),
    );
}

#[test]
fn vitest_pending_and_todo_counts_survive_into_red_evidence() {
    let mut report = fixture::valid_report();
    report["numPendingTests"] = json!(1);
    report["numTodoTests"] = json!(1);
    report["numTotalTests"] = json!(4);
    report["numTotalTestSuites"] = json!(3);
    report["numFailedTestSuites"] = json!(1);
    report["numPassedTestSuites"] = json!(2);
    report["numPendingTestSuites"] = json!(0);
    let assertions = report["testResults"][0]["assertionResults"]
        .as_array_mut()
        .unwrap();
    assertions.push(json!({"fullName":"suite skipped case","status":"pending"}));
    assertions.push(json!({"fullName":"suite future case","status":"todo"}));
    let record = fixture::exercise(report, 1, expectations());
    assert_eq!(
        record["run"]["status"], "Gate",
        "behavioral RED must be accepted: {record}"
    );
    assert_eq!(
        record["fixture_evidence"]["commands"][0]["tests"],
        json!({"passed":1,"failed":1,"ignored":2,"failures":["suite new case"]})
    );
}
