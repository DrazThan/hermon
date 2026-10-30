//! Characterization uses accepted planner IDs without manufacturing RED.
#![cfg(unix)]
#[path = "support/characterization_fixture.rs"]
mod fixture;
use serde_json::json;

fn report() -> serde_json::Value {
    json!({"numPassedTests":2,"numFailedTests":0,"numPendingTests":1,"numTotalTests":3,
        "success":true,"testResults":[{"assertionResults":[
            {"fullName":"suite new case","status":"passed"},
            {"fullName":"baseline","status":"passed"},
            {"fullName":"old skipped","status":"pending"}]}]})
}

#[test]
fn doctor_requires_test_command_for_positive_gate() {
    let report = fixture::doctor_missing_test_command();
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["name"] == "command test" && finding["ok"] == false),
        "{report}"
    );
}

#[test]
fn doctor_rejects_counts_only_report_before_allocating_a_run() {
    let report = fixture::doctor_counts_only_report();
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["ok"] == false
                && finding["fix"]
                    .as_str()
                    .is_some_and(|fix| fix.contains("VitestJson"))),
        "{report}"
    );
}

#[test]
fn accepted_plan_drives_characterization_and_records_required_passing_ids() {
    let record = fixture::exercise(
        report(),
        0,
        json!({"new_tests":["suite new case"],"baseline_failures":[]}),
    );
    assert_eq!(record["run"]["status"], "Gate", "{record}");
    assert!(record["run"]["freezes"]["characterize"].is_string());
    assert_eq!(
        record["fixture_evidence"]["commands"][0]["required_passed_tests"],
        json!(["suite new case"])
    );
}

#[test]
fn newer_unaccepted_characterization_plan_cannot_replace_required_ids() {
    let record = fixture::exercise_with_unaccepted_plan(
        report(),
        0,
        json!({"new_tests":["suite new case"],"baseline_failures":[]}),
    );
    assert_eq!(record["run"]["status"], "Gate", "{record}");
    assert!(record["run"]["freezes"]["characterize"].is_string());
}

#[test]
fn passing_unrelated_tests_cannot_satisfy_accepted_characterization_plan() {
    let record = fixture::exercise(
        report(),
        0,
        json!({"new_tests":["missing approved test"],"baseline_failures":[]}),
    );
    assert_eq!(record["run"]["status"], "Blocked", "{record}");
    assert!(record["run"]["freezes"]["characterize"].is_null());
}
