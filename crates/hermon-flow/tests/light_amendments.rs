//! Frozen acceptance for the light RED guard and its amendment receipt.
//! New behavior is reached only through the check name `red_guard:red`.
#![cfg(unix)]
use hermon_flow::{
    checks::{AttemptContext, CheckCommand, CheckConfig, GitChecks, TestReportFormat},
    hub::{Block, BlockKind, Git},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const EXPECTED: &str = "light_guard_expected";
const CORRECTION: &str = "LIGHT-CORRECTION-ADDED";
const TICKET: &str = "LIGHT-GUARD";

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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn standard_paths() -> BTreeMap<String, Vec<String>> {
    BTreeMap::from([
        ("tests".into(), vec!["tests/**".into()]),
        ("production".into(), vec!["src/**".into()]),
    ])
}

fn overlap_paths() -> BTreeMap<String, Vec<String>> {
    BTreeMap::from([
        ("tests".into(), vec!["tests/**".into()]),
        (
            "production".into(),
            vec!["tests/**".into(), "src/**".into()],
        ),
    ])
}

struct Lab {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    log_dir: PathBuf,
    config: CheckConfig,
    report: PathBuf,
}

impl Lab {
    fn new(paths: BTreeMap<String, Vec<String>>) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("guard repo");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(repo.join("tests")).unwrap();
        let repo = fs::canonicalize(&repo).unwrap();
        git(&repo, &["init", "-b", "run"]);
        git(&repo, &["config", "user.name", "Light fixture"]);
        git(&repo, &["config", "user.email", "light@example.invalid"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        git(&repo, &["config", "tag.gpgsign", "false"]);
        fs::write(repo.join("src/app.rs"), "base\n").unwrap();
        fs::write(repo.join("tests/base.rs"), "base test\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        let log_dir = temporary.path().join("logs");
        let report = temporary.path().join("report.txt");
        let config = CheckConfig {
            workdir: repo.clone(),
            base,
            ticket: TICKET.into(),
            branch: "run".into(),
            log_dir: log_dir.clone(),
            env: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            paths,
            commands: BTreeMap::from([(
                "lint".into(),
                CheckCommand {
                    argv: vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
                    timeout_seconds: 5,
                    report: None,
                },
            )]),
        };
        let mut lab = Self {
            _temp: temporary,
            repo,
            log_dir,
            config,
            report,
        };
        lab.set_report(&failing_report(), "1", TestReportFormat::Json);
        lab
    }

    fn set_report(&mut self, body: &str, exit: &str, format: TestReportFormat) {
        fs::write(&self.report, body).unwrap();
        self.config.commands.insert(
            "test".into(),
            CheckCommand {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "/bin/cat \"$1\"; exit \"$2\"".into(),
                    "report".into(),
                    self.report.display().to_string(),
                    exit.into(),
                ],
                timeout_seconds: 5,
                report: Some(format),
            },
        );
    }

    fn context(bot: &str) -> AttemptContext {
        AttemptContext {
            number: 1,
            bot: bot.into(),
            new_tests: BTreeSet::from([EXPECTED.into()]),
            baseline_failures: BTreeSet::new(),
            ..Default::default()
        }
    }

    fn adapter(&self, bot: &str) -> GitChecks {
        let mut adapter = GitChecks::new(self.config.clone()).expect("guard fixture config");
        adapter.set_attempt(Self::context(bot));
        adapter
    }

    fn commit(&self, path: &str, content: &str, message: &str) {
        let file = self.repo.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
        git(&self.repo, &["add", "."]);
        git(&self.repo, &["commit", "-m", message]);
    }

    fn record_red(&mut self) -> (String, String, String) {
        self.commit(
            "tests/light_acceptance.rs",
            "failing acceptance\n",
            "red tests",
        );
        let mut adapter = self.adapter("light-test-writer");
        adapter
            .check(
                "red",
                &[
                    "only_paths_changed:tests".into(),
                    "lint".into(),
                    "tests_fail".into(),
                ],
                &BTreeMap::new(),
            )
            .expect("original tests-only RED must be provable on main");
        let reference = adapter.evidence("red", "red", 1).expect("RED evidence tag");
        let object = git(
            &self.repo,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/tags/{reference}^{{tag}}"),
            ],
        );
        let note = git(&self.repo, &["notes", "--ref=hermon", "show", &reference]);
        let annotation = tag_annotation(&self.repo, &reference);
        assert_eq!(
            annotation, note,
            "RED annotation and note start in agreement"
        );
        (reference, object, note)
    }

    fn guard(&self, red: &str) -> Result<(), Block> {
        let mut freezes = BTreeMap::new();
        freezes.insert("red".into(), red.to_owned());
        self.adapter("light-implementer").check(
            "green",
            &["red_guard:red".into(), "lint".into(), "tests_pass".into()],
            &freezes,
        )
    }
}

fn failing_report() -> String {
    json!({
        "passed": 0,
        "failed": 1,
        "ignored": 0,
        "failures": [EXPECTED]
    })
    .to_string()
}

fn passing_report() -> String {
    json!({
        "numPassedTests": 1,
        "numFailedTests": 0,
        "numPendingTests": 0,
        "numTodoTests": 0,
        "numTotalTests": 1,
        "numRuntimeErrorTestSuites": 0,
        "numFailedTestSuites": 0,
        "success": true,
        "testResults": [{
            "name": "tests/light_acceptance.rs",
            "message": "",
            "status": "passed",
            "assertionResults": [{"fullName": EXPECTED, "status": "passed"}]
        }]
    })
    .to_string()
}

fn tag_annotation(repo: &Path, reference: &str) -> String {
    git(
        repo,
        &[
            "for-each-ref",
            "--format=%(contents)",
            &format!("refs/tags/{reference}"),
        ],
    )
}

fn note_bytes(repo: &Path, reference: &str) -> String {
    git(repo, &["notes", "--ref=hermon", "show", reference])
}

fn hex_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text)
            if text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            out.push(text.clone());
        }
        Value::Array(items) => {
            for item in items {
                hex_strings(item, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                hex_strings(item, out);
            }
        }
        _ => {}
    }
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        let entries = fs::read_dir(&dir).unwrap();
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

fn bound_diff(note: &Value, log_dir: &Path, marker: &str) -> (String, Vec<u8>) {
    let mut digests = Vec::new();
    hex_strings(note, &mut digests);
    for path in files_under(log_dir) {
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        if !bytes
            .windows(marker.len())
            .any(|window| window == marker.as_bytes())
        {
            continue;
        }
        let digest = sha256(&bytes);
        if digests.iter().any(|item| item == &digest) {
            return (digest, bytes);
        }
    }
    panic!("GREEN note must bind the sha256 of a host diff containing {marker}; note={note}")
}

#[test]
fn light_guard_preserves_original_red_and_records_amendments() {
    let mut lab = Lab::new(standard_paths());
    let (red, object, note) = lab.record_red();
    lab.commit("src/app.rs", "production behavior\n", "production");
    lab.commit(
        "tests/light_acceptance.rs",
        &format!("failing acceptance\n{CORRECTION}\n"),
        "test-only correction",
    );
    lab.set_report(&passing_report(), "0", TestReportFormat::VitestJson);
    // A fresh adapter must recover the pin recorded by evidence(); the pin is not
    // an in-memory field of the RED adapter.
    lab.guard(&red)
        .expect("red_guard accepts a separate test-only amendment");
    assert_eq!(
        git(
            &lab.repo,
            &["rev-parse", "--verify", &format!("refs/tags/{red}^{{tag}}")]
        ),
        object,
        "original RED tag object must be unchanged"
    );
    assert_eq!(
        note_bytes(&lab.repo, &red),
        note,
        "original RED note must be byte-for-byte unchanged"
    );
    assert_eq!(tag_annotation(&lab.repo, &red), note);
    let mut green = lab.adapter("light-implementer");
    let mut freezes = BTreeMap::new();
    freezes.insert("red".into(), red.clone());
    green
        .check(
            "green",
            &["red_guard:red".into(), "lint".into(), "tests_pass".into()],
            &freezes,
        )
        .expect("red_guard receipt survives until evidence");
    let green_ref = green.evidence("green", "green", 1).unwrap();
    let raw = note_bytes(&lab.repo, &green_ref);
    assert!(raw.len() < 8192, "GREEN note must stay under 8192 bytes");
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    let (digest, bytes) = bound_diff(&parsed, &lab.log_dir, CORRECTION);
    assert!(
        bytes
            .windows(CORRECTION.len())
            .any(|window| window == CORRECTION.as_bytes()),
        "generated diff must include the correction line"
    );
    assert_eq!(digest.len(), 64);
    assert_ne!(
        git(&lab.repo, &["rev-parse", &format!("{red}^{{commit}}")]),
        git(&lab.repo, &["rev-parse", "HEAD"]),
        "the amendment commit must stay after the original RED commit"
    );
}

fn reject(case: &str) -> Block {
    if case == "wrong-digest" {
        return wrong_digest();
    }
    let mut lab = Lab::new(if case == "overlap" {
        overlap_paths()
    } else {
        standard_paths()
    });
    let (red, object, note) = lab.record_red();
    match case {
        "mixed" => {
            let tests = lab.repo.join("tests/light_acceptance.rs");
            let source = lab.repo.join("src/app.rs");
            fs::write(&tests, "mixed test edit\n").unwrap();
            fs::write(&source, "mixed production edit\n").unwrap();
            git(&lab.repo, &["add", "."]);
            git(&lab.repo, &["commit", "-m", "mixed test and production"]);
            let red_commit = git(&lab.repo, &["rev-parse", &format!("{red}^{{commit}}")]);
            git(
                &lab.repo,
                &[
                    "checkout",
                    &red_commit,
                    "--",
                    "tests/light_acceptance.rs",
                    "src/app.rs",
                ],
            );
            git(&lab.repo, &["commit", "-m", "revert mixed edit"]);
        }
        "merge" => {
            git(&lab.repo, &["checkout", "-b", "side"]);
            lab.commit("src/side.rs", "side production\n", "side production");
            git(&lab.repo, &["checkout", "run"]);
            git(
                &lab.repo,
                &["merge", "--no-ff", "-m", "merge side into run", "side"],
            );
        }
        "overlap" => {
            lab.commit(
                "tests/light_acceptance.rs",
                "overlap edit\n",
                "path matches tests and production",
            );
        }
        "moved-tag" => {
            let base = lab.config.base.clone();
            git(
                &lab.repo,
                &["update-ref", &format!("refs/tags/{red}"), &base],
            );
        }
        "rewritten-note" => {
            let elsewhere = lab.config.base.clone();
            let forged = note.replace(
                &git(&lab.repo, &["rev-parse", &format!("{red}^{{commit}}")]),
                &elsewhere,
            );
            let file = lab.log_dir.join("forged-note.json");
            fs::create_dir_all(&lab.log_dir).unwrap();
            fs::write(&file, &forged).unwrap();
            git(&lab.repo, &["tag", "-d", &red]);
            git(
                &lab.repo,
                &[
                    "tag",
                    "--no-sign",
                    "-a",
                    &red,
                    &elsewhere,
                    "-F",
                    file.to_str().unwrap(),
                ],
            );
            git(
                &lab.repo,
                &[
                    "notes",
                    "--ref=hermon",
                    "add",
                    "-f",
                    "-F",
                    file.to_str().unwrap(),
                    &red,
                ],
            );
            let _ = object;
        }
        other => panic!("unknown rejection case {other}"),
    }
    lab.set_report(&passing_report(), "0", TestReportFormat::VitestJson);
    lab.guard(&red)
        .expect_err("tampered or mixed history must not pass red_guard")
}

fn wrong_digest() -> Block {
    let mut lab = Lab::new(standard_paths());
    let (red, _, _) = lab.record_red();
    lab.commit("src/app.rs", "production behavior\n", "production");
    lab.commit(
        "tests/light_acceptance.rs",
        &format!("failing acceptance\n{CORRECTION}\n"),
        "test-only correction",
    );
    lab.set_report(&passing_report(), "0", TestReportFormat::VitestJson);
    lab.guard(&red)
        .expect("valid history must pass before the digest is tampered");
    let mut green = lab.adapter("light-implementer");
    let mut freezes = BTreeMap::new();
    freezes.insert("red".into(), red);
    green
        .check(
            "green",
            &["red_guard:red".into(), "lint".into(), "tests_pass".into()],
            &freezes,
        )
        .unwrap();
    let green_ref = green.evidence("green", "green", 1).unwrap();
    let raw = note_bytes(&lab.repo, &green_ref);
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    let (digest, _) = bound_diff(&parsed, &lab.log_dir, CORRECTION);
    let flipped = flip_hex(&digest);
    let tampered = raw.replace(&digest, &flipped);
    let file = lab.log_dir.join("tampered-green-note.json");
    fs::write(&file, &tampered).unwrap();
    let commit = git(
        &lab.repo,
        &["rev-parse", &format!("{green_ref}^{{commit}}")],
    );
    git(&lab.repo, &["tag", "-d", &green_ref]);
    git(
        &lab.repo,
        &[
            "tag",
            "--no-sign",
            "-a",
            &green_ref,
            &commit,
            "-F",
            file.to_str().unwrap(),
        ],
    );
    git(
        &lab.repo,
        &[
            "notes",
            "--ref=hermon",
            "add",
            "-f",
            "-F",
            file.to_str().unwrap(),
            &green_ref,
        ],
    );
    lab.adapter("light-implementer")
        .evidence("green", "green", 1)
        .expect_err("a rewritten GREEN digest must not recover")
}

fn flip_hex(hex: &str) -> String {
    let mut bytes = hex.as_bytes().to_vec();
    let last = bytes.len() - 1;
    bytes[last] = if bytes[last] == b'0' { b'1' } else { b'0' };
    String::from_utf8(bytes).unwrap()
}

fn strict_note_is_unchanged_without_the_light_guard() {
    let mut lab = Lab::new(standard_paths());
    let (red, _, note) = lab.record_red();
    assert!(
        note.len() < 8192,
        "strict RED note must stay under 8192 bytes"
    );
    lab.commit("src/app.rs", "strict production\n", "strict production");
    let mut later = lab.adapter("light-implementer");
    let mut freezes = BTreeMap::new();
    freezes.insert("red".into(), red.clone());
    later
        .check("green", &["frozen_unchanged:red".into()], &freezes)
        .expect("production edit preserves a strict freeze");
    assert_eq!(
        note_bytes(&lab.repo, &red),
        note,
        "unused strict note bytes must stay identical"
    );
    assert!(
        !note.contains("amendment") && !note.contains("diff_sha256"),
        "strict notes must not grow light receipt fields"
    );
}

#[test]
fn light_guard_rejects_mixed_history_and_tampered_proofs() {
    strict_note_is_unchanged_without_the_light_guard();
    for case in [
        "mixed",
        "merge",
        "overlap",
        "moved-tag",
        "rewritten-note",
        "wrong-digest",
    ] {
        let err = reject(case);
        assert!(
            !err.reason.contains("unknown check"),
            "case {case}: red_guard is missing: {err:?}"
        );
        assert_ne!(
            err.kind,
            BlockKind::Transient,
            "case {case}: tamper must not be retried as transport failure: {err:?}"
        );
    }
}
