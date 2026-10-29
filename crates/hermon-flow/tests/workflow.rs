use hermon_flow::workflow::Workflow;

const SDLC: &str = include_str!("fixtures/sdlc.yaml");
const LABEL: &str = "packs/example/workflow.yaml";

fn rejected(yaml: &str, detail: &str) {
    let error = Workflow::from_str(LABEL, yaml).expect_err("invalid workflow must fail at load");
    let error = format!("{error:#}");
    assert!(error.contains(LABEL), "error must identify file: {error}");
    assert!(
        error.to_lowercase().contains(&detail.to_lowercase()),
        "error should identify {detail:?}: {error}"
    );
}

#[test]
fn flow_workflow_valid_sdlc_pack() {
    let workflow = Workflow::from_str(LABEL, SDLC).unwrap();
    assert_eq!(workflow.version, 1);
    assert_eq!(workflow.name, "sdlc");
    assert_eq!(workflow.steps.len(), 9);
    assert_eq!(
        workflow.steps["green"].bot.as_deref(),
        Some("sdlc-implementer")
    );
    assert_eq!(workflow.steps["owner_go"].gate.as_deref(), Some("owner_go"));
    assert_eq!(workflow.steps["pr"].action.as_deref(), Some("open_pr"));
    assert_eq!(workflow.steps["green"].next.as_deref(), Some("review"));
    assert_eq!(workflow.steps["red_fix"].max_runs, Some(2));
    assert_eq!(workflow.steps["review"].max_rounds, Some(2));
    assert_eq!(
        workflow.steps["red"].check,
        ["only_paths_changed:tests", "lint", "tests_fail"]
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workflow.yaml");
    std::fs::write(&path, SDLC).unwrap();
    assert_eq!(Workflow::load(&path).unwrap().name, "sdlc");
}

#[test]
fn flow_workflow_rejects_unknown_key() {
    rejected(&format!("{SDLC}\nsurprise: true\n"), "surprise");
    rejected(
        &SDLC.replace("bot: sdlc-planner", "bot: sdlc-planner\n    surprise: true"),
        "surprise",
    );
    rejected(
        &SDLC.replace(
            "transient_retries: 2",
            "transient_retries: 2\n  surprise: true",
        ),
        "surprise",
    );
}

#[test]
fn flow_workflow_rejects_duplicate_key() {
    for yaml in [
        format!("{SDLC}\nname: overwritten\n"),
        SDLC.replace(
            "bot: sdlc-planner",
            "bot: sdlc-planner\n    bot: overwritten",
        ),
        SDLC.replace(
            "transient_retries: 2",
            "transient_retries: 2\n  transient_retries: 3",
        ),
        SDLC.replace("  owner_go:\n", "  plan: {bot: impostor}\n  owner_go:\n"),
        SDLC.replace(
            "test_defect: red_fix,",
            "test_defect: red_fix, test_defect: review,",
        ),
    ] {
        rejected(&yaml, "duplicate");
    }
}

#[test]
fn flow_workflow_rejects_yaml_alias() {
    rejected(
        &SDLC.replace("bot: sdlc-planner", "bot: &planner sdlc-planner"),
        "anchor",
    );
    rejected(&SDLC.replace("bot: sdlc-planner", "bot: *planner"), "alias");
    rejected(
        &SDLC.replace("bot: sdlc-planner", "bot: !agent sdlc-planner"),
        "tag",
    );
    rejected(
        &SDLC.replace(
            "bot: sdlc-planner",
            "bot: sdlc-planner\n    <<: {next: docs}",
        ),
        "<<",
    );
}

#[test]
fn flow_workflow_rejects_dangling_next() {
    rejected(
        &SDLC.replace("next: owner_go", "next: absent_step"),
        "absent_step",
    );
    rejected(
        &SDLC.replace("test_defect: red_fix", "test_defect: absent_step"),
        "absent_step",
    );
    rejected(
        &SDLC.replace("changes_needed: green", "changes_needed: absent_step"),
        "absent_step",
    );
}

#[test]
fn flow_workflow_rejects_unbounded_cycle() {
    rejected(
        "version: 1\nname: loop\nsteps:\n  a: {bot: worker, next: a}\n",
        "cycle",
    );
    rejected(
        "version: 1\nname: loop\nsteps:\n  a: {bot: worker, next: b}\n  b: {bot: worker, next: a}\n",
        "cycle",
    );
    rejected(&SDLC.replace("    max_runs: 2\n", ""), "cycle");
    rejected(&SDLC.replace("    max_rounds: 2\n", ""), "cycle");
}

#[test]
fn flow_workflow_cycle_bound_must_cover_every_automatic_cycle() {
    // A bounded node elsewhere in one strongly-connected component is insufficient:
    // a <-> b still loops forever without visiting the bounded c node.
    rejected(
        "version: 1\nname: loop\nsteps:\n  a: {bot: worker, next: b}\n  b: {bot: worker, next: a, on_block: {transient: c}}\n  c: {bot: worker, next: a, max_runs: 2}\n",
        "cycle",
    );
    let bounded = "version: 1\nname: loop\nsteps:\n  a: {bot: worker, next: b}\n  b: {bot: worker, next: a, max_runs: 2}\n";
    assert!(Workflow::from_str(LABEL, bounded).is_ok());
}

#[test]
fn flow_workflow_rejects_unknown_checks_and_empty_parameters() {
    for check in [
        "mystery",
        "lint:extra",
        "only_paths_changed:",
        "frozen_unchanged:",
        "cmd:",
    ] {
        rejected(
            &SDLC.replace("project_check", &format!("\"{check}\"")),
            "check",
        );
    }
    assert!(Workflow::from_str(LABEL, &SDLC.replace("project_check", "cmd:custom_check")).is_ok());
}

#[test]
fn flow_workflow_rejects_invalid_model_and_limits() {
    for yaml in [
        SDLC.replace("version: 1", "version: 2"),
        SDLC.replace("bot: sdlc-planner", "bot: sdlc-planner\n    gate: owner_go"),
        SDLC.replace("    bot: sdlc-planner\n", ""),
        SDLC.replace("max_runs: 2", "max_runs: 0"),
        SDLC.replace("max_rounds: 2", "max_rounds: 0"),
        SDLC.replace("attempt_timeout_minutes: 90", "attempt_timeout_minutes: 0"),
        SDLC.replace("stall_alert_minutes: 30", "stall_alert_minutes: 0"),
        "version: 1\nname: empty\nsteps: {}\n".to_owned(),
    ] {
        rejected(&yaml, "");
    }
    assert!(
        Workflow::from_str(
            LABEL,
            &SDLC.replace("transient_retries: 2", "transient_retries: 0")
        )
        .is_ok()
    );
}

#[test]
fn flow_workflow_strict_yaml_does_not_reject_literal_punctuation() {
    let yaml = SDLC.replace(
        "name: sdlc",
        "name: 'sdlc &literal *literal !literal'\n# &comment *comment !comment",
    );
    let workflow = Workflow::from_str(LABEL, &yaml).unwrap();
    assert_eq!(workflow.name, "sdlc &literal *literal !literal");
}

#[test]
fn flow_workflow_rejects_multiple_yaml_documents() {
    rejected(
        &format!("{SDLC}\n---\nversion: 1\nname: hidden\nsteps: {{a: {{bot: worker}}}}\n"),
        "",
    );
}
