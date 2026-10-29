use hermon_flow::templates::Templates;
use std::fs;

const NAMES: [&str; 7] = [
    "started.md",
    "question.md",
    "go_request.md",
    "answer_ack.md",
    "step_done.md",
    "alert.md",
    "outcome.md",
];

#[test]
fn templates_layer_lookup_order() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project/templates");
    let pack = root.path().join("pack/templates");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&pack).unwrap();
    fs::write(project.join("started.md"), "project started {{workflow}}").unwrap();
    fs::write(pack.join("started.md"), "pack started {{workflow}}").unwrap();
    fs::write(pack.join("alert.md"), "pack alert {{technical}}").unwrap();
    let defaults = Templates::load(None, None).unwrap();
    let templates = Templates::load(Some(&project), Some(&pack)).unwrap();
    assert_eq!(
        templates.get("started.md"),
        Some("project started {{workflow}}")
    );
    assert_eq!(templates.get("alert.md"), Some("pack alert {{technical}}"));
    assert_eq!(templates.get("outcome.md"), defaults.get("outcome.md"));
    fs::remove_file(project.join("started.md")).unwrap();
    let templates = Templates::load(Some(&project), Some(&pack)).unwrap();
    assert_eq!(
        templates.get("started.md"),
        Some("pack started {{workflow}}")
    );
    let absent = root.path().join("not-created");
    let templates = Templates::load(Some(&absent), Some(&absent)).unwrap();
    for name in NAMES {
        assert!(
            templates
                .get(name)
                .is_some_and(|body| !body.trim().is_empty()),
            "missing built-in {name}"
        );
        assert_eq!(templates.get(name), defaults.get(name));
    }
    assert!(templates.get("unrecognized.md").is_none());
    for name in ["question.md", "go_request.md"] {
        assert!(defaults.get(name).unwrap().contains("{{owner_mention}}"));
    }
}

#[test]
fn templates_reject_unknown_placeholder() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("question.md");
    fs::write(&path, "{{owner_mention}} {{definitely_unknown}}").unwrap();
    let error = Templates::load(None, Some(root.path()))
        .err()
        .expect("unknown placeholder must fail at load");
    let error = format!("{error:#}");
    assert!(error.contains(&path.display().to_string()), "{error}");
    assert!(error.contains("definitely_unknown"), "{error}");
}

#[test]
fn templates_require_owner_mention() {
    let root = tempfile::tempdir().unwrap();
    for name in ["question.md", "go_request.md"] {
        let path = root.path().join(name);
        fs::write(&path, "Please review this.\nTechnical: {{technical}}").unwrap();
        let error = Templates::load(Some(root.path()), None)
            .err()
            .expect("owner mention required");
        let error = format!("{error:#}");
        assert!(error.contains(&path.display().to_string()), "{error}");
        assert!(error.contains("owner_mention"), "{error}");
        fs::write(
            &path,
            "{{owner_mention}} Please review.\nTechnical: {{technical}}",
        )
        .unwrap();
        assert!(Templates::load(Some(root.path()), None).is_ok());
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn templates_placeholders_are_validated_per_kind() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("started.md");
    fs::write(&path, "{{question}}").unwrap();
    let error = Templates::load(Some(root.path()), None)
        .err()
        .expect("question placeholder is not valid in started");
    let error = format!("{error:#}");
    assert!(
        error.contains("question") && error.contains("started.md"),
        "{error}"
    );
}

#[test]
fn templates_invalid_override_does_not_fall_back() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let pack = root.path().join("pack");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&pack).unwrap();
    fs::write(project.join("started.md"), "{{unknown_override}}").unwrap();
    fs::write(pack.join("started.md"), "valid pack {{workflow}}").unwrap();
    let error = Templates::load(Some(&project), Some(&pack))
        .err()
        .expect("invalid selected override must fail at load");
    let error = format!("{error:#}");
    assert!(
        error.contains(&project.join("started.md").display().to_string()),
        "{error}"
    );
    assert!(error.contains("unknown_override"), "{error}");
}

#[test]
fn templates_reject_logic_and_unclosed_placeholders() {
    let root = tempfile::tempdir().unwrap();
    for body in [
        "{{#if owner_mention}}Hi{{/if}}",
        "{{owner_mention",
        "{{owner_mention}} {{question | escape}}",
    ] {
        fs::write(root.path().join("question.md"), body).unwrap();
        let error = Templates::load(None, Some(root.path()))
            .err()
            .expect("malformed placeholders must fail");
        assert!(format!("{error:#}").contains("question.md"));
    }
}

#[test]
fn templates_valid_plain_text_and_listed_placeholders_load() {
    let root = tempfile::tempdir().unwrap();
    let body = "{{owner_mention}} {{question}}\nTechnical: {{technical}}\n{{options}} {{bot}} {{step}} {{run_status_cmd}}";
    fs::write(root.path().join("question.md"), body).unwrap();
    let templates = Templates::load(Some(root.path()), None).unwrap();
    assert_eq!(templates.get("question.md"), Some(body));
}
