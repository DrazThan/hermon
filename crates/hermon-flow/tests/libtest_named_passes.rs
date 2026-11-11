//! Frozen acceptance: tests_pass proves named libtest IDs, not summary counts.
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

const FIRST: &str = "light_named_one";
const SECOND: &str = "light_named_two";

fn required() -> BTreeSet<String> {
    [FIRST.into(), SECOND.into()].into()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn summary(passed: u64, failed: u64, ignored: u64) -> String {
    format!(
        "test result: ok. {passed} passed; {failed} failed; {ignored} ignored; 0 measured; 0 filtered out; finished in 0.01s\n"
    )
}

fn failed_summary(failed: u64) -> String {
    format!(
        "test result: FAILED. 0 passed; {failed} failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n"
    )
}

fn named_ok(id: &str) -> String {
    format!("test {id} ... ok\n")
}

struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    config: CheckConfig,
    stdout: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("libtest repo");
        fs::create_dir_all(&repo).unwrap();
        let repo = fs::canonicalize(repo).unwrap();
        git(&repo, &["init", "-b", "fixture"]);
        git(&repo, &["config", "user.name", "Libtest fixture"]);
        git(&repo, &["config", "user.email", "libtest@example.invalid"]);
        fs::write(repo.join("lib.rs"), "fn base() {}\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        let stdout = temporary.path().join("libtest.out");
        let config = CheckConfig {
            workdir: repo.clone(),
            base,
            ticket: "LIGHT-LIBTEST".into(),
            branch: "fixture".into(),
            log_dir: temporary.path().join("logs"),
            env: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            paths: BTreeMap::new(),
            commands: BTreeMap::new(),
        };
        let mut fixture = Self {
            _temp: temporary,
            repo,
            config,
            stdout,
        };
        fixture.output(&valid_single(), "0");
        fixture
    }

    fn output(&mut self, text: &str, exit: &str) {
        fs::write(&self.stdout, text).unwrap();
        self.config.commands.insert(
            "test".into(),
            CheckCommand {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "/bin/cat \"$1\"; exit \"$2\"".into(),
                    "libtest".into(),
                    self.stdout.display().to_string(),
                    exit.into(),
                ],
                timeout_seconds: 5,
                report: Some(TestReportFormat::Libtest),
            },
        );
    }

    fn adapter(&self) -> GitChecks {
        let mut adapter = GitChecks::new(self.config.clone()).unwrap();
        adapter.set_attempt(AttemptContext {
            number: 1,
            bot: "light-libtest".into(),
            new_tests: required(),
            baseline_failures: BTreeSet::new(),
            ..Default::default()
        });
        adapter
    }

    fn check(&self) -> Result<(), hermon_flow::hub::Block> {
        self.adapter()
            .check("green", &["tests_pass".into()], &BTreeMap::new())
    }
}

fn valid_single() -> String {
    format!(
        "running 2 tests\n{}{}\n{}",
        named_ok(FIRST),
        named_ok(SECOND),
        summary(2, 0, 0)
    )
}

fn valid_multi_target() -> String {
    format!(
        "running 1 test\n{}\n{}\nrunning 1 test\n{}\n{}",
        named_ok(FIRST),
        summary(1, 0, 0),
        named_ok(SECOND),
        summary(1, 0, 0)
    )
}

fn valid_unrelated_ignored() -> String {
    format!(
        "running 3 tests\n{}{}test light_unrelated ... ignored\n\n{}",
        named_ok(FIRST),
        named_ok(SECOND),
        summary(2, 0, 1)
    )
}

fn rejected(fixture: &Fixture) {
    assert!(
        fixture.check().is_err(),
        "incomplete libtest output must not prove named passes"
    );
    assert!(git(&fixture.repo, &["tag", "--list", "hermon/*"]).is_empty());
}

fn prove(text: &str) -> Value {
    let mut fixture = Fixture::new();
    fixture.output(text, "0");
    let mut checks = fixture.adapter();
    checks
        .check("green", &["tests_pass".into()], &BTreeMap::new())
        .expect("Libtest named passes must satisfy tests_pass");
    let tag = checks.evidence("green", "green", 1).unwrap();
    let raw = git(&fixture.repo, &["notes", "--ref=hermon", "show", &tag]);
    let evidence: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        evidence["commands"][0]["required_passed_tests"],
        json!(required().into_iter().collect::<Vec<_>>())
    );
    evidence
}

#[test]
fn libtest_proves_expected_ids_not_counts() {
    prove(&valid_single());
    prove(&valid_multi_target());
    prove(&valid_unrelated_ignored());

    let mut missing = Fixture::new();
    missing.output(
        &format!("running 1 test\n{}\n{}", named_ok(FIRST), summary(1, 0, 0)),
        "0",
    );
    rejected(&missing);

    let mut renamed = Fixture::new();
    renamed.output(
        &format!(
            "{}{}\n{}",
            named_ok(FIRST),
            named_ok("light_renamed"),
            summary(2, 0, 0)
        ),
        "0",
    );
    rejected(&renamed);

    let mut ignored = Fixture::new();
    ignored.output(
        &format!(
            "{}test {SECOND} ... ignored\n\n{}",
            named_ok(FIRST),
            summary(1, 0, 1)
        ),
        "0",
    );
    rejected(&ignored);

    let mut duplicate = Fixture::new();
    duplicate.output(
        &format!(
            "{}{}{}\n{}",
            named_ok(FIRST),
            named_ok(FIRST),
            named_ok(SECOND),
            summary(3, 0, 0)
        ),
        "0",
    );
    rejected(&duplicate);

    let mut counts_only = Fixture::new();
    counts_only.output(
        "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
        "0",
    );
    rejected(&counts_only);

    let mut quiet = Fixture::new();
    quiet.output(&format!("running 2 tests\n..\n\n{}", summary(2, 0, 0)), "0");
    rejected(&quiet);

    let mut ansi = Fixture::new();
    ansi.output(
        &format!(
            "\u{1b}[32m{}{}\u{1b}[0m\n{}",
            named_ok(FIRST),
            named_ok(SECOND),
            summary(2, 0, 0)
        ),
        "0",
    );
    rejected(&ansi);

    let mut mismatched = Fixture::new();
    mismatched.output(
        &format!(
            "{}{}\n{}",
            named_ok(FIRST),
            named_ok(SECOND),
            summary(3, 0, 0)
        ),
        "0",
    );
    rejected(&mismatched);

    let mut malformed = Fixture::new();
    malformed.output("test result: ok. not-a-count\n", "0");
    rejected(&malformed);

    let mut nonzero = Fixture::new();
    nonzero.output(&valid_single(), "7");
    rejected(&nonzero);

    let mut later = Fixture::new();
    later.output(
        &format!(
            "{}{}\n{}test other_target ... FAILED\n{}",
            named_ok(FIRST),
            named_ok(SECOND),
            summary(2, 0, 0),
            failed_summary(1)
        ),
        "1",
    );
    rejected(&later);
}
