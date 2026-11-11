//! Frozen acceptance for the shipped sdlc-light pack.
use hermon_flow::{intake::TEMPLATE_NAMES, templates::Templates, workflow::Workflow};
use std::{fs, path::PathBuf};
use yaml_rust2::YamlLoader;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn pack() -> PathBuf {
    root().join("workflows/sdlc-light")
}

#[test]
fn shipped_light_pack_has_bounded_ci_and_explicit_amendment_review() {
    let path = pack().join("workflow.yaml");
    let text = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("missing pack: {}: {error}", path.display());
    });
    let workflow = Workflow::load(&path).unwrap_or_else(|error| {
        panic!("missing pack or invalid light workflow: {path:?}: {error:#}");
    });
    assert_eq!(workflow.version, 1);
    assert_eq!(workflow.name, "sdlc-light");
    assert_eq!(workflow.templates.as_deref(), Some("templates/"));
    assert_eq!(
        workflow
            .steps
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "ci", "docs", "draft_pr", "green", "merge", "plan", "ready", "red", "review"
        ]
    );
    assert!(!workflow.steps.contains_key("owner_go"));
    assert!(!workflow.steps.contains_key("red_fix"));
    for (from, to) in [
        ("plan", "red"),
        ("red", "green"),
        ("green", "draft_pr"),
        ("draft_pr", "ci"),
        ("ci", "review"),
        ("review", "ready"),
        ("ready", "merge"),
        ("merge", "docs"),
    ] {
        assert_eq!(
            workflow.steps[from].next.as_deref(),
            Some(to),
            "{from} must advance to {to}"
        );
    }
    assert!(workflow.steps["docs"].next.is_none());
    assert_eq!(workflow.steps["plan"].bot.as_deref(), Some("sdlc-planner"));
    assert_eq!(workflow.steps["plan"].inputs, ["block_reason"]);
    assert_eq!(
        workflow.steps["plan"].produces,
        [
            "plan.md",
            "test-brief.md",
            "impl-brief.md",
            "test-expectations.json"
        ]
    );
    assert!(workflow.steps["plan"].publish.is_empty());
    assert_eq!(workflow.steps["plan"].max_runs, Some(2));
    assert_eq!(
        workflow.steps["red"].bot.as_deref(),
        Some("sdlc-test-writer")
    );
    assert_eq!(
        workflow.steps["red"].inputs,
        ["plan.md", "test-brief.md", "block_reason"]
    );
    assert_eq!(
        workflow.steps["red"].check,
        ["only_paths_changed:tests", "lint", "tests_fail"]
    );
    assert_eq!(workflow.steps["red"].tag.as_deref(), Some("red"));
    assert_eq!(workflow.steps["red"].on_block["scope_change"], "plan");
    assert_eq!(
        workflow.steps["green"].bot.as_deref(),
        Some("sdlc-implementer")
    );
    assert_eq!(
        workflow.steps["green"].inputs,
        ["plan.md", "impl-brief.md", "red", "block_reason"]
    );
    assert_eq!(
        workflow.steps["green"].check,
        ["red_guard:red", "lint", "tests_pass"]
    );
    assert_eq!(workflow.steps["green"].tag.as_deref(), Some("green"));
    assert_eq!(workflow.steps["green"].max_runs, Some(4));
    assert!(workflow.steps["green"].on_block.is_empty());
    assert_eq!(
        workflow.steps["draft_pr"].action.as_deref(),
        Some("open_draft_pr")
    );
    assert_eq!(workflow.steps["ci"].gate.as_deref(), Some("github_ci"));
    assert_eq!(workflow.steps["ci"].on_block["ci_failed"], "green");
    assert_eq!(
        workflow.steps["review"].bot.as_deref(),
        Some("sdlc-reviewer")
    );
    assert_eq!(
        workflow.steps["review"].inputs,
        ["plan.md", "diff:red..green", "amendments"]
    );
    assert_eq!(
        workflow.steps["review"].require_different_family.as_deref(),
        Some("green")
    );
    assert_eq!(workflow.steps["review"].max_rounds, Some(2));
    assert_eq!(
        workflow.steps["review"].on_verdict["changes_needed"],
        "green"
    );
    assert_eq!(workflow.steps["ready"].action.as_deref(), Some("ready_pr"));
    assert_eq!(workflow.steps["ready"].on_block["ci_failed"], "green");
    assert_eq!(workflow.steps["merge"].gate.as_deref(), Some("owner_merge"));
    assert_eq!(workflow.steps["docs"].bot.as_deref(), Some("sdlc-docs"));
    assert_eq!(workflow.steps["docs"].inputs, ["plan.md", "merge"]);
    assert_eq!(workflow.limits.attempt_timeout_minutes, 90);
    assert_eq!(workflow.limits.transient_retries, 2);
    assert_eq!(workflow.limits.stall_alert_minutes, 30);
    for step in workflow.steps.values() {
        assert_ne!(step.gate.as_deref(), Some("owner_go"));
        assert_ne!(step.action.as_deref(), Some("open_pr"));
        assert!(
            step.check.iter().all(|check| check != "project_check"),
            "light pack must not run local project_check"
        );
    }
    let docs = YamlLoader::load_from_str(&text).expect("pack workflow YAML");
    let amendments = &docs[0]["steps"]["review"]["review_amendments"];
    assert_eq!(amendments["red"].as_str(), Some("red"));
    assert_eq!(amendments["green"].as_str(), Some("green"));

    let strict = root().join("workflows/sdlc");
    for role in ["plan", "red", "green", "review", "docs"] {
        let body = fs::read_to_string(pack().join("roles").join(format!("{role}.md")))
            .unwrap_or_else(|error| panic!("missing role {role}: {error}"));
        assert!(body.trim().len() > 100, "{role} needs substantive guidance");
        assert!(
            !body.contains("approved by GO"),
            "{role} must not require Owner GO"
        );
    }
    let plan = fs::read_to_string(pack().join("roles/plan.md")).unwrap();
    assert!(!plan.contains("Owner GO"));
    assert!(plan.to_ascii_lowercase().contains("red"));
    assert!(plan.to_ascii_lowercase().contains("draft"));
    let red = fs::read_to_string(pack().join("roles/red.md")).unwrap();
    let red_lower = red.to_ascii_lowercase();
    assert!(
        red_lower.contains("id"),
        "RED role must keep expected IDs immutable"
    );
    let green = fs::read_to_string(pack().join("roles/green.md")).unwrap();
    assert!(!green.contains("project_check"));
    let green_lower = green.to_ascii_lowercase();
    assert!(
        green_lower.contains("commit")
            && (green_lower.contains("test") || green_lower.contains("amend")),
        "GREEN role must require dedicated test and production commits"
    );
    let review = fs::read_to_string(pack().join("roles/review.md")).unwrap();
    let review_lower = review.to_ascii_lowercase();
    assert!(review_lower.contains("digest"));
    assert!(
        review_lower.contains("do not modify")
            || review_lower.contains("do not edit")
            || review_lower.contains("must not edit")
            || review_lower.contains("must not modify")
            || review_lower.contains("cannot edit"),
        "review role must forbid editing the candidate"
    );
    assert_eq!(
        fs::read(pack().join("roles/docs.md")).unwrap(),
        fs::read(strict.join("roles/docs.md")).unwrap(),
        "docs.md must be copied verbatim from strict"
    );
    assert!(!pack().join("profile-skills").exists());
    assert!(!pack().join("roles/red_fix.md").exists());
    assert!(!pack().join("templates/go_request.md").exists());
    for name in [
        "question.md",
        "answer_ack.md",
        "step_done.md",
        "alert.md",
        "outcome.md",
    ] {
        assert_eq!(
            fs::read(pack().join("templates").join(name)).unwrap(),
            fs::read(strict.join("templates").join(name)).unwrap(),
            "{name} must be copied verbatim"
        );
    }
    let started = fs::read_to_string(pack().join("templates/started.md")).unwrap();
    assert_ne!(
        started,
        fs::read_to_string(strict.join("templates/started.md")).unwrap()
    );
    let started_lower = started.to_ascii_lowercase();
    assert!(started_lower.contains("draft"));
    assert!(started_lower.contains("plan"));
    assert!(!started.contains("reply GO"));
    assert!(!started.contains("Owner GO"));

    let loaded = Templates::load(None, Some(&pack().join("templates")))
        .expect("light templates must load, falling back for omitted names");
    let builtin = Templates::load(None, None).unwrap();
    for name in TEMPLATE_NAMES {
        assert!(
            loaded.get(name).is_some(),
            "missing resolved template {name}"
        );
        let rendered = loaded.preview(name).unwrap();
        assert!(!rendered.contains("{{"), "unrendered placeholder in {name}");
    }
    assert_eq!(
        loaded.get("go_request.md"),
        builtin.get("go_request.md"),
        "omitted go_request.md must resolve through the compiled fallback"
    );
    assert_ne!(loaded.get("started.md"), builtin.get("started.md"));

    let guide = fs::read_to_string(root().join("docs/flow/sdlc-light.md"))
        .expect("missing docs/flow/sdlc-light.md");
    assert!(guide.len() > 400, "light guide must be substantive");
    assert!(guide.contains("sdlc-light"));
    assert!(guide.to_ascii_lowercase().contains("amend"));
    assert!(
        guide.contains("github_ci") || guide.to_ascii_lowercase().contains("github actions"),
        "light guide must describe the CI gate"
    );
    for doc in ["docs/flow/code-host.md", "docs/flow/workflow-files.md"] {
        let body = fs::read_to_string(root().join(doc)).unwrap();
        assert!(
            body.contains("sdlc-light"),
            "{doc} must cross-reference the light pack"
        );
    }
}
