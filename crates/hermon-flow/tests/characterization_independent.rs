//! Independent acceptance for named passing characterization checks.
#![cfg(unix)]
use hermon_flow::{
    checks::{AttemptContext, CheckCommand, CheckConfig, GitChecks, TestReportFormat},
    hub::Git,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const FIRST: &str = "fixture suite preserves observable behavior";
const SECOND: &str = "fixture suite rejects malformed input";
const OTHER: &str = "fixture suite unrelated existing coverage";
fn required() -> BTreeSet<String> {
    [FIRST.into(), SECOND.into()].into()
}
fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "DrazThan")
        .env("GIT_COMMITTER_NAME", "DrazThan")
        .env(
            "GIT_AUTHOR_EMAIL",
            "90212909+DrazThan@users.noreply.github.com",
        )
        .env(
            "GIT_COMMITTER_EMAIL",
            "90212909+DrazThan@users.noreply.github.com",
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn report(assertions: &[(&str, &str)]) -> Value {
    let count = |status: &str| assertions.iter().filter(|(_, s)| *s == status).count();
    let failed = count("failed");
    json!({"numPassedTests":count("passed"),"numFailedTests":failed,
        "numPendingTests":count("pending")+count("skipped"),"numTodoTests":count("todo"),
        "numTotalTests":assertions.len(),"numRuntimeErrorTestSuites":0,
        "numFailedTestSuites":usize::from(failed>0),"success":failed==0,
        "testResults":[{"name":"fixture.test.ts","message":"","status":if failed>0{"failed"}else{"passed"},
            "assertionResults":assertions.iter().map(|(name,status)|json!({"fullName":name,"status":status})).collect::<Vec<_>>()}]})
}
fn passing() -> Value {
    report(&[(FIRST, "passed"), (SECOND, "passed"), (OTHER, "passed")])
}
struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    config: CheckConfig,
    context: AttemptContext,
    stdout: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("fixture repository");
        fs::create_dir(&repo).unwrap();
        let repo = fs::canonicalize(repo).unwrap();
        git(&repo, &["init", "-b", "fixture"]);
        fs::create_dir(repo.join("src")).unwrap();
        fs::write(repo.join("src/base.rs"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "public fixture baseline"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        fs::create_dir(repo.join("tests")).unwrap();
        fs::write(
            repo.join("tests/characterization.test.ts"),
            "fixture acceptance\n",
        )
        .unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "public fixture acceptance"]);
        let stdout = temporary.path().join("report.json");
        fs::write(&stdout, passing().to_string()).unwrap();
        let config = CheckConfig {
            workdir: repo.clone(),
            base,
            ticket: "FIXTURE-CHARACTERIZATION".into(),
            branch: "fixture".into(),
            log_dir: temporary.path().join("logs"),
            env: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            paths: BTreeMap::from([("tests".into(), vec!["tests/**".into()])]),
            commands: BTreeMap::new(),
        };
        let context = AttemptContext {
            number: 1,
            bot: "independent-fixture-author".into(),
            new_tests: required(),
            baseline_failures: BTreeSet::new(),
            ..Default::default()
        };
        let mut result = Self {
            _temp: temporary,
            repo,
            config,
            context,
            stdout,
        };
        result.output(passing(), "0");
        result
    }
    fn output(&mut self, output: Value, exit: &str) {
        self.raw_output(&output.to_string(), exit, TestReportFormat::VitestJson)
    }
    fn raw_output(&mut self, output: &str, exit: &str, format: TestReportFormat) {
        fs::write(&self.stdout, output).unwrap();
        let script = if exit == "signal" {
            "/bin/cat \"$1\"; kill -TERM $$"
        } else {
            "/bin/cat \"$1\"; exit \"$2\""
        };
        self.config.commands.insert(
            "test".into(),
            CheckCommand {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    script.into(),
                    "fixture".into(),
                    self.stdout.display().to_string(),
                    exit.into(),
                ],
                timeout_seconds: 3,
                report: Some(format),
            },
        );
    }
    fn adapter(&self) -> GitChecks {
        let mut adapter = GitChecks::new(self.config.clone()).unwrap();
        adapter.set_attempt(self.context.clone());
        adapter
    }
    fn check(&self) -> Result<(), hermon_flow::hub::Block> {
        self.adapter()
            .check("characterize", &["tests_pass".into()], &BTreeMap::new())
    }
    fn commit(&self, path: &str, content: &str) {
        fs::write(self.repo.join(path), content).unwrap();
        git(&self.repo, &["add", "."]);
        git(&self.repo, &["commit", "-m", "public fixture change"]);
    }
}
fn rejected(fixture: &Fixture) {
    assert!(
        fixture.check().is_err(),
        "invalid passing evidence must fail closed"
    );
    assert!(git(&fixture.repo, &["tag", "--list", "hermon/*"]).is_empty());
}

#[test]
fn accepted_named_passing_ids_are_recorded_without_unrelated_names() {
    let fixture = Fixture::new();
    let mut checks = fixture.adapter();
    checks
        .check(
            "characterize",
            &["only_paths_changed:tests".into(), "tests_pass".into()],
            &BTreeMap::new(),
        )
        .expect("every independently required ID occurs once and passes");
    let tag = checks.evidence("characterize", "characterized", 1).unwrap();
    let raw = git(&fixture.repo, &["notes", "--ref=hermon", "show", &tag]);
    let evidence: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        evidence["commands"][0]["required_passed_tests"],
        json!(required().into_iter().collect::<Vec<_>>())
    );
    assert_eq!(evidence["commands"][0]["tests"]["passed"], 3);
    assert_eq!(
        evidence["frozen_paths"],
        json!(["tests/characterization.test.ts"])
    );
    assert!(
        !raw.contains(OTHER),
        "unrelated passing names must not inflate the bounded note"
    );
    assert!(raw.len() < 8192);
}
#[test]
fn missing_expected_id_rejects_counts_that_otherwise_pass() {
    let mut f = Fixture::new();
    f.output(report(&[(FIRST, "passed"), (OTHER, "passed")]), "0");
    rejected(&f);
}
#[test]
fn skipped_pending_and_todo_expected_ids_never_establish_pass() {
    for status in ["skipped", "pending", "todo"] {
        let mut f = Fixture::new();
        f.output(report(&[(FIRST, "passed"), (SECOND, status)]), "0");
        rejected(&f);
    }
}
#[test]
fn failed_expected_id_never_establishes_pass_even_with_exit_zero() {
    let mut f = Fixture::new();
    f.output(report(&[(FIRST, "passed"), (SECOND, "failed")]), "0");
    rejected(&f);
}
#[test]
fn duplicate_expected_id_is_ambiguous_even_when_all_copies_pass() {
    let mut f = Fixture::new();
    f.output(
        report(&[(FIRST, "passed"), (FIRST, "passed"), (SECOND, "passed")]),
        "0",
    );
    rejected(&f);
}
#[test]
fn counts_only_and_incompatible_report_formats_cannot_prove_named_passes() {
    for format in [TestReportFormat::Json, TestReportFormat::Libtest] {
        let mut f = Fixture::new();
        let text = if matches!(format, TestReportFormat::Json) {
            "{\"passed\":3,\"failed\":0,\"ignored\":0,\"failures\":[]}"
        } else {
            "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n"
        };
        f.raw_output(text, "0", format);
        rejected(&f);
    }
}
#[test]
fn inconsistent_assertion_counts_cannot_prove_named_passes() {
    let mut f = Fixture::new();
    let mut r = passing();
    r["numPassedTests"] = json!(50);
    f.output(r, "0");
    rejected(&f);
}
#[test]
fn suite_and_runtime_errors_cannot_prove_named_passes() {
    for kind in ["message", "runtime", "failed-suite"] {
        let mut f = Fixture::new();
        let mut r = passing();
        match kind {
            "message" => r["testResults"][0]["message"] = json!("fixture collection error"),
            "runtime" => r["numRuntimeErrorTestSuites"] = json!(1),
            _ => {
                r["numFailedTestSuites"] = json!(1);
                r["testResults"][0]["status"] = json!("failed");
            }
        }
        f.output(r, "0");
        rejected(&f);
    }
}
#[test]
fn nonzero_and_signal_exit_cannot_prove_named_passes() {
    for exit in ["7", "signal"] {
        let mut f = Fixture::new();
        f.output(passing(), exit);
        rejected(&f);
    }
}
#[test]
fn empty_expected_ids_and_declared_baseline_failures_are_rejected() {
    let mut empty = Fixture::new();
    empty.context.new_tests.clear();
    rejected(&empty);
    let mut baseline = Fixture::new();
    baseline
        .context
        .baseline_failures
        .insert("fixture preexisting baseline failure".into());
    rejected(&baseline);
}
#[test]
fn characterization_freeze_forbids_test_edits_and_verification_forbids_production_edits() {
    for path in ["src/base.rs", "tests/characterization.test.ts"] {
        let f = Fixture::new();
        let mut checks = f.adapter();
        checks
            .check(
                "characterize",
                &["only_paths_changed:tests".into(), "tests_pass".into()],
                &BTreeMap::new(),
            )
            .unwrap();
        let tag = checks.evidence("characterize", "characterized", 1).unwrap();
        f.commit(path, "changed\n");
        assert!(
            f.adapter()
                .check(
                    "verify",
                    &[
                        "only_paths_changed:tests".into(),
                        "frozen_unchanged:characterized".into(),
                        "tests_pass".into()
                    ],
                    &BTreeMap::from([("characterized".into(), tag)])
                )
                .is_err()
        );
    }
}
#[test]
fn original_red_still_requires_real_expected_failures() {
    let mut f = Fixture::new();
    assert!(
        f.adapter()
            .check("red", &["tests_fail".into()], &BTreeMap::new())
            .is_err(),
        "passing tests must not satisfy RED"
    );
    f.output(report(&[(FIRST, "failed"), (SECOND, "failed")]), "1");
    f.adapter()
        .check("red", &["tests_fail".into()], &BTreeMap::new())
        .expect("strict RED still accepts actual expected assertion failures");
}
