//! Independent shipped SDLC pack acceptance for issue #125.
use hermon_flow::{
    agent::BotConfig,
    intake::{Project, TEMPLATE_NAMES},
    templates::Templates,
    workflow::Workflow,
};
use std::{fs, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn shipped_sdlc_pack_keeps_gates_freeze_and_bounded_correction() {
    let workflow = Workflow::load(&root().join("workflows/sdlc/workflow.yaml")).unwrap();
    assert_eq!(workflow.name, "sdlc");
    assert_eq!(workflow.steps.len(), 9);
    for (from, to) in [
        ("plan", "owner_go"),
        ("owner_go", "red"),
        ("red", "green"),
        ("green", "review"),
        ("review", "pr"),
        ("pr", "merge"),
        ("merge", "docs"),
        ("red_fix", "green"),
    ] {
        assert_eq!(workflow.steps[from].next.as_deref(), Some(to));
    }
    assert_eq!(workflow.steps["owner_go"].gate.as_deref(), Some("owner_go"));
    assert_eq!(workflow.steps["merge"].gate.as_deref(), Some("owner_merge"));
    assert_eq!(workflow.steps["pr"].action.as_deref(), Some("open_pr"));
    assert!(workflow.steps["docs"].next.is_none());
    assert_eq!(workflow.steps["plan"].publish, ["plan.md"]);
    assert_eq!(
        workflow.steps["plan"].produces,
        [
            "plan.md",
            "test-brief.md",
            "impl-brief.md",
            "test-expectations.json"
        ]
    );
    assert_eq!(workflow.steps["green"].on_block["test_defect"], "red_fix");
    assert_eq!(workflow.steps["green"].on_block["scope_change"], "plan");
    assert_eq!(
        workflow.steps["review"].on_verdict["changes_needed"],
        "green"
    );
    assert_eq!(
        workflow.steps["review"].require_different_family.as_deref(),
        Some("green")
    );
    assert_eq!(workflow.steps["review"].max_rounds, Some(2));
    assert_eq!(workflow.steps["red_fix"].max_runs, Some(2));
    for name in ["red", "red_fix"] {
        assert_eq!(
            workflow.steps[name].check,
            ["only_paths_changed:tests", "lint", "tests_fail"]
        );
        assert_eq!(workflow.steps[name].tag.as_deref(), Some("red"));
    }
    assert_eq!(
        workflow.steps["green"].check,
        ["frozen_unchanged:red", "project_check"]
    );
    assert_eq!(workflow.steps["green"].tag.as_deref(), Some("green"));
    assert_eq!(workflow.limits.attempt_timeout_minutes, 90);
    assert_eq!(workflow.limits.transient_retries, 2);
    assert_eq!(workflow.limits.stall_alert_minutes, 30);
    for (step, spec) in &workflow.steps {
        if spec.bot.is_some() {
            let role = fs::read_to_string(
                root()
                    .join("workflows/sdlc/roles")
                    .join(format!("{step}.md")),
            )
            .unwrap_or_else(|error| panic!("missing role for {step}: {error}"));
            assert!(
                role.trim().len() > 100,
                "{step} must provide substantive role guidance"
            );
        }
    }
}

#[test]
fn shipped_templates_render_from_pack_with_mentions_and_marker() {
    let pack = root().join("workflows/sdlc/templates");
    let templates = Templates::load(None, Some(&pack)).unwrap();
    for name in TEMPLATE_NAMES {
        let body = fs::read_to_string(pack.join(name)).unwrap();
        assert_eq!(
            templates.get(name),
            Some(body.as_str()),
            "pack must ship {name}, rather than falling through to compiled defaults"
        );
        assert!(
            body.contains("Technical:"),
            "{name} needs plain-language first line and technical details"
        );
        let rendered = templates.preview(name).unwrap();
        assert!(!rendered.contains("{{"), "unrendered placeholder in {name}");
        assert!(
            rendered.contains("<!-- hermon:"),
            "hub marker missing from {name}"
        );
        if ["question.md", "go_request.md"].contains(&name) {
            assert!(
                rendered.contains("[Sample owner]"),
                "{name} must mention the owner"
            );
        }
    }
}

#[test]
fn rummyrune_project_uses_supported_schema_and_honest_profile_bindings() {
    let source = fs::read_to_string(root().join("projects/rummyrune/project.yaml")).unwrap();
    // The shipped repository is owner-local. Redirect only repo to an isolated
    // directory so schema verification works on CI without the owner's checkout.
    let temporary = tempfile::tempdir().unwrap();
    let repo = temporary.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let mut replaced = false;
    let portable = source
        .lines()
        .map(|line| {
            if line.starts_with("repo:") {
                replaced = true;
                format!("repo: {}", serde_json::to_string(&repo).unwrap())
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(replaced, "project must explicitly configure its repo");
    let path = temporary.path().join("project.yaml");
    fs::write(&path, portable).unwrap();
    let project = Project::load(&path).expect("shipped project must match the runtime schema");
    assert_eq!(project.base, "qa");
    assert_eq!(project.start, "plan");
    assert_eq!(project.branch_prefix, "oztal000");
    assert_eq!(project.code_host.repo, "DrazThan/RummiRune");
    assert_eq!(project.tracker.endpoint, "https://api.linear.app/graphql");
    assert!(!project.tracker.token_env.is_empty());
    assert!(!project.tracker.owner_ids.is_empty());
    assert!(!project.tracker.owner_mention.is_empty());
    assert!(!project.env["PATH"].is_empty());
    assert!(project.paths["tests"].iter().any(|p| p.contains("test")));
    for (name, argv) in [
        ("lint", vec!["pnpm", "lint"]),
        (
            "test",
            vec![
                "pnpm",
                "exec",
                "vitest",
                "run",
                "--workspace",
                "tests/vitest.workspace.ts",
                "--no-file-parallelism",
                "--reporter=json",
            ],
        ),
        ("project_check", vec!["pnpm", "check"]),
    ] {
        assert_eq!(project.commands[name].argv, argv);
        assert!(project.commands[name].timeout_seconds >= 60);
    }
    assert!(
        project.commands["project_check"].timeout_seconds > 20 * 60,
        "full project check can exceed twenty minutes"
    );
    let workflow = Workflow::load(&root().join("workflows/sdlc/workflow.yaml")).unwrap();
    for spec in workflow.steps.values() {
        if let Some(bot) = &spec.bot {
            assert!(
                matches!(&project.bots[bot], BotConfig::Hermes { profile } if profile == bot),
                "preserve the named profile and its skills; doctor must surface the unsupported cancellation capability"
            );
        }
    }
}
