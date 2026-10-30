//! Independent end-to-end Vitest report and accepted plan artifact acceptance.
#![cfg(unix)]
#[path = "support/vitest_fixture.rs"]
mod fixture;
use serde_json::json;

#[test]
fn accepted_plan_test_expectations_drive_vitest_red_without_manual_run_edits() {
    let record = fixture::exercise(
        json!({"numPassedTests":1,"numFailedTests":1,"numPendingTests":1,"numTotalTests":3,
        "success":false,
        "testResults":[{"name":"red.test.js","status":"failed","assertionResults":[
            {"fullName":"suite new case","title":"new case","status":"failed"},
            {"fullName":"suite old passing case","status":"passed"},
            {"fullName":"suite ignored case","status":"pending"}
        ]}]}),
        1,
        json!({"new_tests":["suite new case"],"baseline_failures":[]}),
    );
    assert_eq!(
        record["run"]["status"], "Gate",
        "accepted planner artifact must supply trusted expected IDs: {record}"
    );
    assert_eq!(record["run"]["current"], "owner_go");
    assert!(
        record["run"]["freezes"]["red"].is_string(),
        "RED evidence must be tagged"
    );
    let tests = &record["fixture_evidence"]["commands"][0]["tests"];
    assert_eq!(tests["passed"], 1);
    assert_eq!(tests["failed"], 1);
    assert_eq!(tests["ignored"], 1);
    assert_eq!(tests["failures"], json!(["suite new case"]));
}

#[test]
fn planner_expected_ids_cannot_be_replaced_by_unrelated_vitest_failure() {
    let record = fixture::exercise(
        json!({"numPassedTests":0,"numFailedTests":1,"numPendingTests":0,"numTotalTests":1,
            "testResults":[{"assertionResults":[{"fullName":"unrelated old case","status":"failed"}]}]}),
        1,
        json!({"new_tests":["suite new case"],"baseline_failures":[]}),
    );
    assert_eq!(
        record["run"]["status"], "Blocked",
        "unrelated failure must not satisfy planner acceptance: {record}"
    );
    assert!(record["run"]["freezes"]["red"].is_null());
}

#[test]
fn newer_unaccepted_plan_directory_cannot_replace_accepted_test_expectations() {
    let record = fixture::exercise_with_unaccepted_plan(
        fixture::valid_report(),
        1,
        json!({"new_tests":["suite new case"],"baseline_failures":[]}),
    );
    assert_eq!(
        record["run"]["status"], "Gate",
        "only accepted plan artifacts may supply expected test IDs: {record}"
    );
    assert!(record["run"]["freezes"]["red"].is_string());
}
