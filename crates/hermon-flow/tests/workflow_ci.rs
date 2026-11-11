//! Schema acceptance for the draft-PR actions and the GitHub Actions gate.
//! These tests load workflows only. They do not call hub or code-host APIs.

use hermon_flow::workflow::Workflow;
use std::path::PathBuf;

const LABEL: &str = "ci-fixture.yaml";

/// Same graph the CLI fixtures run. A cycle through `github_ci` is not an owner gate.
const FIXTURE: &str = "\
version: 1
name: ci-fixture
steps:
  plan:
    bot: planner
    produces: [plan.md]
    next: green
  green:
    bot: implementer
    inputs: [plan.md, block_reason]
    tag: green
    max_runs: 3
    next: draft_pr
  draft_pr:
    action: open_draft_pr
    next: ci
  ci:
    gate: github_ci
    next: review
    on_block: {ci_failed: green}
  review:
    bot: reviewer
    require_different_family: green
    on_verdict: {changes_needed: green}
    max_rounds: 2
    next: ready
  ready:
    action: ready_pr
    on_block: {ci_failed: green}
    next: merge
  merge:
    gate: owner_merge
    next: docs
  docs:
    bot: writer
    inputs: [merge]
";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn load(yaml: &str) -> Result<Workflow, String> {
    Workflow::from_str(LABEL, yaml).map_err(|error| format!("{error:#}"))
}
fn must_load(yaml: &str) -> Workflow {
    load(yaml).unwrap_or_else(|error| panic!("workflow must load: {error}"))
}
fn rejection(yaml: &str) -> String {
    load(yaml).expect_err("invalid workflow must fail at load")
}

#[test]
fn ci_vocabulary_loads_and_strict_pack_is_unchanged() {
    let strict = Workflow::load(&root().join("workflows/sdlc/workflow.yaml"))
        .expect("shipped strict pack must still load");
    assert_eq!(strict.name, "sdlc");
    assert_eq!(strict.steps.len(), 9);
    assert_eq!(strict.steps["pr"].action.as_deref(), Some("open_pr"));
    assert_eq!(strict.steps["owner_go"].gate.as_deref(), Some("owner_go"));
    assert_eq!(strict.steps["merge"].gate.as_deref(), Some("owner_merge"));
    assert_eq!(strict.steps["green"].next.as_deref(), Some("review"));
    assert!(strict.steps.values().all(|step| {
        step.gate.as_deref() != Some("github_ci")
            && step.action.as_deref() != Some("open_draft_pr")
            && step.action.as_deref() != Some("ready_pr")
            && !step.on_block.contains_key("ci_failed")
    }));

    let workflow = must_load(FIXTURE);
    assert_eq!(workflow.name, "ci-fixture");
    assert_eq!(
        workflow.steps["draft_pr"].action.as_deref(),
        Some("open_draft_pr")
    );
    assert_eq!(workflow.steps["ci"].gate.as_deref(), Some("github_ci"));
    assert_eq!(workflow.steps["ci"].on_block["ci_failed"], "green");
    assert_eq!(workflow.steps["ready"].action.as_deref(), Some("ready_pr"));
    assert_eq!(workflow.steps["ready"].on_block["ci_failed"], "green");
    assert_eq!(workflow.steps["merge"].gate.as_deref(), Some("owner_merge"));
    assert_eq!(workflow.steps["green"].max_runs, Some(3));
    assert_eq!(workflow.steps["review"].max_rounds, Some(2));
    assert_eq!(
        workflow.steps["review"].require_different_family.as_deref(),
        Some("green")
    );
}

#[test]
fn ci_correction_cycle_must_be_bounded() {
    // Control: an owner gate still breaks an otherwise unbounded automatic cycle.
    must_load(
        "version: 1\nname: owner-breaks\nsteps:\n  work:\n    bot: worker\n    next: merge\n  merge:\n    gate: owner_merge\n    next: work\n",
    );

    let through_gate = "\
version: 1
name: gate-does-not-break
steps:
  green:
    bot: implementer
    next: ci
  ci:
    gate: github_ci
    next: green
    on_block: {ci_failed: green}
";
    let error = rejection(through_gate);
    assert!(
        error.contains("unbounded automatic cycle"),
        "a github_ci cycle is not broken by the gate the way owner_merge is: {error}"
    );
    must_load(&through_gate.replace(
        "    bot: implementer\n",
        "    bot: implementer\n    max_runs: 2\n",
    ));

    let unbounded = FIXTURE.replace("    max_runs: 3\n", "");
    let error = rejection(&unbounded);
    assert!(
        error.contains("unbounded automatic cycle"),
        "green -> draft_pr -> ci -> green without max_runs must be rejected: {error}"
    );
    must_load(FIXTURE);
}

#[test]
fn ci_failed_is_a_routable_block_kind_and_unknown_names_fail() {
    let action = rejection(
        "version: 1\nname: bad-action\nsteps:\n  publish:\n    action: open_draft_prr\n    next: done\n  done:\n    gate: owner_merge\n",
    );
    assert!(
        action.contains("open_draft_prr") && action.to_lowercase().contains("unknown"),
        "{action}"
    );
    let gate = rejection(
        "version: 1\nname: bad-gate\nsteps:\n  wait:\n    gate: not_a_gate\n    next: done\n  done:\n    bot: worker\n",
    );
    assert!(
        gate.contains("not_a_gate") && gate.to_lowercase().contains("unknown"),
        "{gate}"
    );
    let kind = rejection(
        "version: 1\nname: bad-kind\nsteps:\n  wait:\n    gate: owner_merge\n    on_block: {not_a_kind: work}\n    next: work\n  work:\n    bot: worker\n",
    );
    assert!(
        kind.contains("not_a_kind") && kind.to_lowercase().contains("unknown"),
        "{kind}"
    );

    let workflow = must_load(
        "version: 1\nname: ci-failed-route\nsteps:\n  fix:\n    bot: implementer\n    max_runs: 2\n    next: ci\n  ci:\n    gate: github_ci\n    next: done\n    on_block: {ci_failed: fix}\n  done:\n    bot: writer\n",
    );
    assert_eq!(workflow.steps["ci"].on_block["ci_failed"], "fix");
}
