//! Independent synthetic host acceptance for trusted characterization IDs.
#![cfg(unix)]
#[path = "support/characterization_independent_fixture.rs"]
mod fixture;
use serde_json::{Value, json};
const FIRST: &str = "fixture suite preserves observable behavior";
const SECOND: &str = "fixture suite rejects malformed input";
fn report(names: &[&str]) -> Value {
    json!({"numPassedTests":names.len(),"numFailedTests":0,"numPendingTests":0,"numTotalTests":names.len(),"numFailedTestSuites":0,"numRuntimeErrorTestSuites":0,"success":true,
    "testResults":[{"name":"fixture.test.js","status":"passed","message":"","assertionResults":names.iter().map(|name|json!({"fullName":name,"status":"passed"})).collect::<Vec<_>>()}]})
}
fn expectations() -> Value {
    json!({"new_tests":[FIRST,SECOND],"baseline_failures":[]})
}
#[test]
fn tests_pass_uses_accepted_planner_ids_without_manual_run_edits() {
    let record = fixture::exercise(report(&[FIRST, SECOND]), 0, expectations());
    assert_eq!(
        record["run"]["status"], "Gate",
        "accepted independently planned IDs must permit characterization: {record}"
    );
    assert!(record["run"]["freezes"]["characterized"].is_string());
    assert_eq!(
        record["fixture_evidence"]["commands"][0]["required_passed_tests"],
        json!([FIRST, SECOND])
    );
}
#[test]
fn newer_unaccepted_planner_directory_cannot_replace_required_passing_ids() {
    let record =
        fixture::exercise_with_unaccepted_plan(report(&[FIRST, SECOND]), 0, expectations());
    assert_eq!(
        record["run"]["status"], "Gate",
        "an unaccepted directory must not replace accepted planner expectations: {record}"
    );
    assert!(record["run"]["freezes"]["characterized"].is_string());
}
#[test]
fn passing_unrelated_ids_cannot_replace_missing_planner_acceptance() {
    let record = fixture::exercise(
        report(&[FIRST, "fixture unrelated passing case"]),
        0,
        expectations(),
    );
    assert_eq!(
        record["run"]["status"], "Blocked",
        "positive counts cannot replace missing accepted planner IDs: {record}"
    );
    assert!(record["run"]["freezes"]["characterized"].is_null());
}
