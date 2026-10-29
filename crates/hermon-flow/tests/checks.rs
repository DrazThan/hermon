//! Independently authored Slice 5 acceptance tests. Frozen before implementation.
#![cfg(unix)]
use hermon_flow::{
    checks::*,
    hub::{BlockKind, Git},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/check.sh");

struct Fixture {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    config: CheckConfig,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("fixture work tree");
        fs::create_dir_all(&repo).unwrap();
        let repo = fs::canonicalize(repo).unwrap();
        git(&repo, &["init", "-b", "run"]);
        git(&repo, &["config", "user.email", "fixture@example.invalid"]);
        git(&repo, &["config", "user.name", "Fixture"]);
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(repo.join("tests")).unwrap();
        fs::write(repo.join("src/main.rs"), "base source\n").unwrap();
        fs::write(repo.join("tests/old.test.rs"), "base test\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        let config = CheckConfig {
            workdir: repo.clone(),
            base,
            ticket: "ISSUE-120".into(),
            branch: "run".into(),
            log_dir: dir.path().join("attempt logs"),
            env: BTreeMap::from([
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("HERM_TEST_ENV".into(), "configured value".into()),
            ]),
            paths: BTreeMap::from([(
                "tests".into(),
                vec!["**/*.test.*".into(), "tests/**".into()],
            )]),
            commands: BTreeMap::new(),
        };
        Self {
            _dir: dir,
            repo,
            config,
        }
    }
    fn commit(&self, path: &str, bytes: &str) {
        let path = self.repo.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        git(&self.repo, &["add", "."]);
        git(&self.repo, &["commit", "-m", "fixture change"]);
    }
    fn adapter(&self) -> GitChecks {
        let mut adapter = GitChecks::new(self.config.clone()).expect("fixture configuration");
        adapter.set_attempt(context());
        adapter
    }
    fn command(
        &mut self,
        name: &str,
        mode: &str,
        extra: &[&str],
        report: Option<TestReportFormat>,
    ) {
        self.config.commands.insert(
            name.into(),
            CheckCommand {
                argv: [
                    vec!["/bin/sh".into(), SCRIPT.into(), mode.into()],
                    extra.iter().map(|s| (*s).into()).collect(),
                ]
                .concat(),
                timeout_seconds: 3,
                report,
            },
        );
    }
}
fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn context() -> AttemptContext {
    AttemptContext {
        number: 2,
        bot: "independent-test-writer".into(),
        model: Some("fixture-model".into()),
        new_tests: BTreeSet::from(["new_case".into()]),
        baseline_failures: BTreeSet::from(["old_case".into()]),
    }
}
fn check(adapter: &mut GitChecks, checks: &[&str]) -> Result<(), hermon_flow::hub::Block> {
    adapter.check(
        "red",
        &checks.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
        &BTreeMap::new(),
    )
}
fn note(repo: &Path, tag: &str) -> (String, Value) {
    assert_eq!(
        git(repo, &["cat-file", "-t", tag]),
        "tag",
        "evidence must be annotated"
    );
    let annotation = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(contents)",
            &format!("refs/tags/{tag}"),
        ],
    );
    let notes = git(repo, &["notes", "--ref=hermon", "show", tag]);
    assert_eq!(annotation, notes, "annotation and explicit note must agree");
    let parsed = serde_json::from_str(&annotation).expect("JSON evidence annotation");
    (annotation, parsed)
}

#[test]
fn check_only_paths_changed() {
    let fixture = Fixture::new();
    fixture.commit("tests/new.test.rs", "new test\n");
    fixture.commit("nested/a.test.ts", "nested glob\n");
    let mut adapter = fixture.adapter();
    check(&mut adapter, &["only_paths_changed:tests"]).expect("all paths belong to named set");
    fixture.commit("src/main.rs", "forbidden source edit\n");
    assert!(check(&mut adapter, &["only_paths_changed:tests"]).is_err());
    assert!(check(&mut adapter, &["only_paths_changed:unknown"]).is_err());
}

#[test]
fn check_paths_include_deleted_and_renamed_sources() {
    let fixture = Fixture::new();
    git(&fixture.repo, &["mv", "src/main.rs", "tests/moved.test.rs"]);
    git(
        &fixture.repo,
        &["commit", "-m", "move source into allowed set"],
    );
    assert!(
        check(&mut fixture.adapter(), &["only_paths_changed:tests"]).is_err(),
        "rename must include removed source path"
    );
}

#[test]
fn check_frozen_unchanged_detects_edit() {
    let fixture = Fixture::new();
    fixture.commit("tests/new.test.rs", "frozen v1\n");
    let mut adapter = fixture.adapter();
    check(&mut adapter, &["only_paths_changed:tests"]).unwrap();
    let frozen = adapter.evidence("red", "red", 1).unwrap();
    let (_, evidence) = note(&fixture.repo, &frozen);
    assert_eq!(evidence["frozen_paths"], json!(["tests/new.test.rs"]));
    fixture.commit("src/main.rs", "implementation\n");
    let refs = BTreeMap::from([("red".into(), frozen)]);
    adapter
        .check("green", &["frozen_unchanged:red".into()], &refs)
        .expect("source edit preserves frozen tests");
    fixture.commit("tests/new.test.rs", "unauthorized weakening\n");
    assert!(
        adapter
            .check("green", &["frozen_unchanged:red".into()], &refs)
            .is_err()
    );
}

#[test]
fn check_frozen_missing_reference_fails_closed() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .adapter()
            .check("green", &["frozen_unchanged:red".into()], &BTreeMap::new())
            .is_err()
    );
}

#[test]
fn check_tests_fail_requires_new_failures() {
    let mut fixture = Fixture::new();
    fixture.commit("tests/new.test.rs", "new test\n");
    fixture.command(
        "test",
        "json",
        &[
            r#"{"passed":1,"failed":2,"ignored":0,"failures":["new_case","old_case"]}"#,
            "1",
        ],
        Some(TestReportFormat::Json),
    );
    check(&mut fixture.adapter(), &["tests_fail"]).expect("nonzero plus new test failure is RED");
    for (report, status) in [
        (
            r#"{"passed":1,"failed":1,"ignored":0,"failures":["old_case"]}"#,
            "1",
        ),
        (
            r#"{"passed":1,"failed":1,"ignored":0,"failures":["new_case"]}"#,
            "0",
        ),
        (r#"{"passed":1,"failed":0,"ignored":0,"failures":[]}"#, "1"),
        (
            r#"{"passed":1,"failed":2,"ignored":0,"failures":["new_case"]}"#,
            "1",
        ),
        (
            r#"{"passed":1,"failed":2,"ignored":0,"failures":["new_case","new_case"]}"#,
            "1",
        ),
        ("not a test report", "1"),
    ] {
        fixture.command(
            "test",
            "json",
            &[report, status],
            Some(TestReportFormat::Json),
        );
        assert!(
            check(&mut fixture.adapter(), &["tests_fail"]).is_err(),
            "invalid RED: {report} / {status}"
        );
    }
}

#[test]
fn check_tests_fail_requires_expected_test_identity() {
    let mut fixture = Fixture::new();
    fixture.command(
        "test",
        "json",
        &[
            r#"{"passed":0,"failed":1,"ignored":0,"failures":["new_case"]}"#,
            "1",
        ],
        Some(TestReportFormat::Json),
    );
    let mut adapter = fixture.adapter();
    let mut empty = context();
    empty.new_tests.clear();
    adapter.set_attempt(empty);
    assert!(
        check(&mut adapter, &["tests_fail"]).is_err(),
        "nonzero alone cannot establish RED"
    );
    let mut old = context();
    old.baseline_failures.insert("new_case".into());
    adapter.set_attempt(old);
    assert!(
        check(&mut adapter, &["tests_fail"]).is_err(),
        "a known existing failure is not new"
    );
    let mut two = context();
    two.new_tests.insert("second_new_case".into());
    adapter.set_attempt(two);
    assert!(
        check(&mut adapter, &["tests_fail"]).is_err(),
        "all expected new tests must fail"
    );
}

#[test]
fn check_tests_fail_supports_native_libtest() {
    let mut fixture = Fixture::new();
    let output = "running 2 tests\ntest old_case ... ok\ntest new_case ... FAILED\n\nfailures:\n\n---- new_case stdout ----\nassertion failed\n\nfailures:\n    new_case\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s";
    fixture.command(
        "test",
        "text",
        &[output, "101"],
        Some(TestReportFormat::Libtest),
    );
    check(&mut fixture.adapter(), &["tests_fail"])
        .expect("native Rust test result identifies the failing case");
    fixture.command(
        "test",
        "text",
        &["error: could not compile crate", "101"],
        Some(TestReportFormat::Libtest),
    );
    assert!(
        check(&mut fixture.adapter(), &["tests_fail"]).is_err(),
        "compiler error is not a new failing test"
    );
}

#[test]
fn check_missing_binary_is_capability() {
    let mut fixture = Fixture::new();
    fixture.config.env.insert(
        "PATH".into(),
        fixture.repo.join("empty-bin").display().to_string(),
    );
    for check_name in ["lint", "project_check", "tests_fail", "cmd:custom"] {
        let command_name = match check_name {
            "tests_fail" => "test",
            "cmd:custom" => "custom",
            other => other,
        };
        fixture.config.commands.insert(
            command_name.into(),
            CheckCommand {
                argv: vec!["sh".into(), "-c".into(), "exit 1".into()],
                timeout_seconds: 2,
                report: Some(TestReportFormat::Json),
            },
        );
        let block = check(&mut fixture.adapter(), &[check_name])
            .expect_err("configured PATH excludes host sh");
        assert_eq!(block.kind, BlockKind::Capability);
    }
    fixture.config.env.remove("PATH");
    assert_eq!(
        check(&mut fixture.adapter(), &["lint"]).unwrap_err().kind,
        BlockKind::Capability,
        "absent PATH must not use host search path"
    );
}

#[test]
fn check_command_builtins_pass_and_fail() {
    for (name, check_name) in [
        ("lint", "lint"),
        ("project_check", "project_check"),
        ("custom", "cmd:custom"),
    ] {
        let mut fixture = Fixture::new();
        fixture.command(name, "pass", &[], None);
        check(&mut fixture.adapter(), &[check_name]).unwrap();
        fixture.command(name, "fail", &[], None);
        assert!(check(&mut fixture.adapter(), &[check_name]).is_err());
    }
}

#[test]
fn check_commands_preserve_argv_cwd_and_environment() {
    let mut fixture = Fixture::new();
    let repo = fixture.repo.display().to_string();
    fixture.command(
        "lint",
        "literal",
        &["literal $(touch injected) ; * space", &repo],
        None,
    );
    check(&mut fixture.adapter(), &["lint"]).expect("direct argv and exact worktree/environment");
    assert!(!fixture.repo.join("injected").exists());
}

#[test]
fn check_timeout_is_bounded_and_blocks_evidence() {
    let mut fixture = Fixture::new();
    fixture.command("lint", "sleep", &[], None);
    fixture
        .config
        .commands
        .get_mut("lint")
        .unwrap()
        .timeout_seconds = 1;
    let mut adapter = fixture.adapter();
    let start = Instant::now();
    assert_eq!(
        check(&mut adapter, &["lint"]).unwrap_err().kind,
        BlockKind::Transient
    );
    assert!(start.elapsed() < Duration::from_secs(8));
    assert!(adapter.evidence("red", "red", 1).is_err());
    assert!(git(&fixture.repo, &["tag", "--list", "hermon/*"]).is_empty());
}

#[test]
fn evidence_tag_note_under_limit() {
    let mut fixture = Fixture::new();
    fixture.commit("tests/new.test.rs", "new test\n");
    fixture.command("lint", "noisy", &[], None);
    let mut adapter = fixture.adapter();
    check(&mut adapter, &["lint"]).unwrap();
    let tag = adapter.evidence("red", "red", 1).unwrap();
    assert_eq!(tag, "hermon/ISSUE-120/red");
    assert_eq!(
        git(&fixture.repo, &["rev-parse", &format!("{tag}^{{commit}}")]),
        git(&fixture.repo, &["rev-parse", "HEAD"])
    );
    let (raw, value) = note(&fixture.repo, &tag);
    assert!(raw.len() < 8192, "note size is a byte limit");
    assert_eq!(value["schema"], 1);
    assert_eq!(value["step"], "red");
    assert_eq!(value["attempt"], 2);
    assert_eq!(value["bot"], "independent-test-writer");
    assert_eq!(value["model"], "fixture-model");
    let commands = value["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command["exit_code"], 0);
    assert!(command["argv"].as_array().is_some());
    assert!(command["duration_ms"].as_u64().is_some());
    let log_path = command["log"]["path"].as_str().unwrap();
    let bytes = fs::read(log_path).expect("long output retained in an attempt log");
    assert!(bytes.len() > 150_000);
    assert!(String::from_utf8_lossy(&bytes).contains("stderr trailer"));
    let digest = Command::new("python3")
        .args([
            "-c",
            "import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest())",
            log_path,
        ])
        .output()
        .unwrap();
    assert!(digest.status.success());
    assert_eq!(
        command["log"]["sha256"],
        String::from_utf8(digest.stdout).unwrap().trim()
    );
}

#[test]
fn evidence_records_test_counts_and_is_idempotent_across_restart() {
    let mut fixture = Fixture::new();
    fixture.commit("tests/new.test.rs", "frozen test\n");
    fixture.command(
        "test",
        "json",
        &[
            r#"{"passed":3,"failed":1,"ignored":2,"failures":["new_case"]}"#,
            "1",
        ],
        Some(TestReportFormat::Json),
    );
    let mut adapter = fixture.adapter();
    check(&mut adapter, &["tests_fail"]).unwrap();
    let tag = adapter.evidence("red", "red", 1).unwrap();
    let (_, data) = note(&fixture.repo, &tag);
    assert_eq!(data["commands"][0]["tests"]["passed"], 3);
    assert_eq!(data["commands"][0]["tests"]["failed"], 1);
    assert_eq!(data["commands"][0]["tests"]["ignored"], 2);
    let original = git(&fixture.repo, &["rev-parse", &tag]);
    fixture.commit("tests/new.test.rs", "superseding test\n");
    let mut restarted = fixture.adapter();
    assert_eq!(restarted.evidence("red", "red", 1).unwrap(), tag);
    assert_eq!(
        git(&fixture.repo, &["rev-parse", &tag]),
        original,
        "retry must never move a prior evidence key"
    );
    restarted
        .check("red_fix", &["tests_fail".into()], &BTreeMap::new())
        .unwrap();
    let second = restarted.evidence("red_fix", "red", 2).unwrap();
    assert_eq!(second, "hermon/ISSUE-120/red.2");
    assert_ne!(git(&fixture.repo, &["rev-parse", &second]), original);
    assert_eq!(note(&fixture.repo, &second).1["step"], "red_fix");
}

#[test]
fn evidence_requires_successful_checks_at_current_head_and_branch() {
    let mut fixture = Fixture::new();
    fixture.command("lint", "pass", &[], None);
    let mut adapter = fixture.adapter();
    assert!(adapter.evidence("red", "red", 1).is_err());
    check(&mut adapter, &["lint"]).unwrap();
    fixture.commit("src/main.rs", "changed after check\n");
    assert!(
        adapter.evidence("red", "red", 1).is_err(),
        "new commit requires new check"
    );
    check(&mut adapter, &["lint"]).unwrap();
    git(&fixture.repo, &["switch", "-c", "unrelated"]);
    assert!(
        adapter.evidence("red", "red", 1).is_err(),
        "evidence belongs only on bound run branch"
    );
    assert!(git(&fixture.repo, &["tag", "--list", "hermon/*"]).is_empty());
}

#[test]
fn evidence_failed_recheck_revokes_success_receipt() {
    let mut fixture = Fixture::new();
    fixture.command("lint", "pass", &[], None);
    let mut adapter = fixture.adapter();
    check(&mut adapter, &["lint"]).unwrap();
    assert!(check(&mut adapter, &["only_paths_changed:missing"]).is_err());
    assert!(
        adapter.evidence("red", "red", 1).is_err(),
        "a previous pass cannot mask current check failure"
    );
}

#[test]
fn checks_and_evidence_reject_dirty_frozen_paths() {
    let fixture = Fixture::new();
    fixture.commit("tests/new.test.rs", "frozen\n");
    let mut adapter = fixture.adapter();
    check(&mut adapter, &["only_paths_changed:tests"]).unwrap();
    let tag = adapter.evidence("red", "red", 1).unwrap();
    fs::write(
        fixture.repo.join("tests/new.test.rs"),
        "uncommitted weakening\n",
    )
    .unwrap();
    let refs = BTreeMap::from([("red".into(), tag)]);
    assert!(
        adapter
            .check("green", &["frozen_unchanged:red".into()], &refs)
            .is_err(),
        "dirty edits cannot preserve a freeze"
    );
    assert!(adapter.evidence("green", "green", 1).is_err());
}

#[test]
fn evidence_metadata_overflow_never_publishes_partial_authority() {
    let fixture = Fixture::new();
    let mut adapter = fixture.adapter();
    let mut oversized = context();
    oversized.model = Some("m".repeat(9000));
    adapter.set_attempt(oversized);
    check(&mut adapter, &["only_paths_changed:tests"]).unwrap();
    assert!(
        adapter.evidence("red", "red", 1).is_err(),
        "metadata must fit without silently dropping frozen authority"
    );
    assert!(git(&fixture.repo, &["tag", "--list", "hermon/*"]).is_empty());
}
