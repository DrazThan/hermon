//! Independent acceptance for the separate test-only characterization pack.
use hermon_flow::{templates::Templates, workflow::Workflow};
use std::{fs, path::PathBuf};
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
#[test]
fn shipped_characterization_pack_keeps_owner_gates_and_bounded_independent_review() {
    let pack = root().join("workflows/sdlc-characterization");
    let workflow = Workflow::load(&pack.join("workflow.yaml"))
        .expect("separate characterization workflow must load");
    assert_eq!(workflow.name, "sdlc-characterization");
    assert_eq!(workflow.steps.len(), 9);
    for (from, to) in [
        ("plan", "owner_go"),
        ("owner_go", "characterize"),
        ("characterize", "verify"),
        ("verify", "review"),
        ("review", "pr"),
        ("pr", "merge"),
        ("merge", "docs"),
        ("characterize_fix", "verify"),
    ] {
        assert_eq!(
            workflow.steps[from].next.as_deref(),
            Some(to),
            "{from} must route through {to}"
        );
    }
    assert_eq!(workflow.steps["owner_go"].gate.as_deref(), Some("owner_go"));
    assert_eq!(workflow.steps["merge"].gate.as_deref(), Some("owner_merge"));
    assert_eq!(workflow.steps["pr"].action.as_deref(), Some("open_pr"));
    assert!(workflow.steps["docs"].next.is_none());
    assert!(
        workflow.steps["plan"]
            .produces
            .contains(&"test-expectations.json".into())
    );
    assert!(workflow.steps["plan"].publish.contains(&"plan.md".into()));
    for name in ["characterize", "characterize_fix"] {
        let step = &workflow.steps[name];
        for check in ["only_paths_changed:tests", "lint", "tests_pass"] {
            assert!(
                step.check.contains(&check.into()),
                "{name} must enforce {check}"
            );
        }
        assert!(
            !step.check.contains(&"tests_fail".into()),
            "characterization must not pretend passing tests are RED"
        );
        assert_eq!(step.tag.as_deref(), Some("characterized"));
    }
    let verify = &workflow.steps["verify"];
    for check in [
        "only_paths_changed:tests",
        "frozen_unchanged:characterized",
        "tests_pass",
        "project_check",
    ] {
        assert!(
            verify.check.contains(&check.into()),
            "verify must enforce {check}"
        );
    }
    assert_eq!(verify.tag.as_deref(), Some("verified"));
    assert_eq!(verify.on_block["test_defect"], "characterize_fix");
    assert_eq!(verify.on_block["scope_change"], "plan");
    assert_eq!(workflow.steps["characterize_fix"].max_runs, Some(2));
    let review = &workflow.steps["review"];
    assert_eq!(review.require_different_family.as_deref(), Some("verify"));
    assert_eq!(review.max_rounds, Some(2));
    assert_eq!(review.on_verdict["changes_needed"], "verify");
    for (step, bot) in [
        ("plan", "sdlc-planner"),
        ("characterize", "sdlc-test-writer"),
        ("characterize_fix", "sdlc-test-writer"),
        ("verify", "sdlc-implementer"),
        ("review", "sdlc-reviewer"),
        ("docs", "sdlc-docs"),
    ] {
        assert_eq!(workflow.steps[step].bot.as_deref(), Some(bot));
        let role = fs::read_to_string(pack.join("roles").join(format!("{step}.md")))
            .expect("every bot step must ship role guidance");
        assert!(
            role.trim().len() > 100,
            "role for {step} must contain substantive guidance"
        );
    }
    let template_dir = pack.join(
        workflow
            .templates
            .as_ref()
            .expect("pack must name its comment templates"),
    );
    let templates = Templates::load(None, Some(&template_dir)).unwrap();
    for name in [
        "started.md",
        "question.md",
        "go_request.md",
        "answer_ack.md",
        "step_done.md",
        "alert.md",
        "outcome.md",
    ] {
        let rendered = templates.preview(name).unwrap();
        assert!(rendered.contains("<!-- hermon:"));
        if ["question.md", "go_request.md"].contains(&name) {
            assert!(rendered.contains("[Sample owner]"));
        }
    }
}
#[test]
fn shipped_strict_sdlc_pack_still_requires_behavioral_red() {
    let strict = Workflow::load(&root().join("workflows/sdlc/workflow.yaml")).unwrap();
    assert_eq!(strict.name, "sdlc");
    for name in ["red", "red_fix"] {
        assert!(strict.steps[name].check.contains(&"tests_fail".into()));
        assert!(!strict.steps[name].check.contains(&"tests_pass".into()));
        assert_eq!(strict.steps[name].tag.as_deref(), Some("red"));
    }
    assert!(
        strict.steps["green"]
            .check
            .contains(&"frozen_unchanged:red".into())
    );
}
