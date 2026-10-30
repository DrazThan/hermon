//! Independent compatibility regression for installed Vitest 2.1.9 JSON output.
#![cfg(unix)]
#[path = "support/vitest_fixture.rs"]
mod fixture;
use serde_json::json;

#[test]
fn real_vitest_skipped_status_counts_as_pending_ignored_evidence() {
    let mut report = fixture::valid_report();
    report["numPendingTests"] = json!(1);
    report["numTotalTests"] = json!(3);
    report["testResults"][0]["assertionResults"]
        .as_array_mut()
        .unwrap()
        .push(json!({"fullName":"suite intentionally skipped case","status":"skipped"}));
    let record = fixture::exercise(
        report,
        1,
        json!({"new_tests":["suite new case"],"baseline_failures":[]}),
    );
    assert_eq!(
        record["run"]["status"], "Gate",
        "actual Vitest skipped assertions must not invalidate behavioral RED: {record}"
    );
    assert_eq!(
        record["fixture_evidence"]["commands"][0]["tests"],
        json!({"passed":1,"failed":1,"ignored":1,"failures":["suite new case"]})
    );
}
