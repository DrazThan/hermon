//! Independent CLI acceptance for Slice 6 template preview.
use std::{fs, process::Command};
#[test]
fn templates_preview_cli() {
    let root = tempfile::tempdir().unwrap();
    let pack = root.path().join("pack");
    let templates = pack.join("templates");
    fs::create_dir_all(&templates).unwrap();
    let workflow = pack.join("workflow.yaml");
    fs::write(
        &workflow,
        "version: 1\nname: preview-test\ntemplates: templates/\nsteps:\n  work:\n    bot: worker\n",
    )
    .unwrap();
    fs::write(
        templates.join("question.md"),
        "Custom pack: {{owner_mention}} {{question}}\n{{technical}}\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hermon"))
        .args(["templates", "preview"])
        .arg(&workflow)
        .arg("question.md")
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("Custom pack:"), "{text}");
    assert!(!text.contains("{{"), "{text}");
    assert!(text.contains("<!-- hermon:run="), "{text}");
    assert!(text.contains("request="), "{text}");
    let overrides = root.path().join("overrides");
    fs::create_dir_all(&overrides).unwrap();
    fs::write(
        overrides.join("question.md"),
        "Project override {{owner_mention}} {{question}}",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hermon"))
        .args(["templates", "preview"])
        .arg(&workflow)
        .arg("question.md")
        .arg("--project-templates")
        .arg(&overrides)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("Project override")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_hermon"))
        .args(["templates", "preview"])
        .arg(&workflow)
        .arg("missing.md")
        .output()
        .unwrap();
    assert!(!output.status.success());
}
