//! Run-bound project checks and immutable, bounded git evidence.
use crate::{
    agent::OwnedChild,
    hub::{Block, BlockKind, Git},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub enum TestReportFormat {
    /// Last nonempty stdout line: {passed, failed, ignored, failures: [test IDs]}.
    Json,
    /// Rust's ordinary libtest output, including each `test ID ... FAILED` line.
    Libtest,
}
#[derive(Debug, Clone)]
pub struct CheckCommand {
    pub argv: Vec<String>,
    pub timeout_seconds: u64,
    pub report: Option<TestReportFormat>,
}
#[derive(Debug, Clone)]
pub struct CheckConfig {
    pub workdir: PathBuf,
    pub base: String,
    pub ticket: String,
    pub branch: String,
    pub log_dir: PathBuf,
    /// Complete command environment; executables never use the host's PATH.
    pub env: BTreeMap<String, String>,
    /// Slash-separated globs: `*` and `?` match within a component, `**`
    /// matches zero or more complete components.
    pub paths: BTreeMap<String, Vec<String>>,
    pub commands: BTreeMap<String, CheckCommand>,
}
#[derive(Debug, Clone, Default)]
pub struct AttemptContext {
    pub number: u32,
    pub bot: String,
    pub model: Option<String>,
    /// Independently supplied expected new test IDs, never inferred from exit status.
    pub new_tests: BTreeSet<String>,
    pub baseline_failures: BTreeSet<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestReport {
    passed: u64,
    failed: u64,
    ignored: u64,
    failures: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LogRef {
    path: PathBuf,
    sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommandEvidence {
    name: String,
    argv: Vec<String>,
    exit_code: Option<i32>,
    duration_ms: u64,
    tests: Option<TestReport>,
    log: LogRef,
}
#[derive(Debug, Serialize, Deserialize)]
struct EvidenceNote {
    schema: u32,
    ticket: String,
    step: String,
    tag: String,
    revision: u32,
    attempt: u32,
    bot: String,
    model: Option<String>,
    branch: String,
    base: String,
    commit: String,
    commands: Vec<CommandEvidence>,
    frozen_paths: Vec<String>,
}
struct Receipt {
    step: String,
    head: String,
    commands: Vec<CommandEvidence>,
    paths: Vec<String>,
}
pub struct GitChecks {
    config: CheckConfig,
    context: AttemptContext,
    receipt: Option<Receipt>,
}
impl GitChecks {
    pub fn new(mut config: CheckConfig) -> std::result::Result<Self, Block> {
        let prepare = || -> Result<CheckConfig> {
            config.workdir = fs::canonicalize(&config.workdir)?;
            ensure!(config.workdir.is_dir(), "worktree must be a directory");
            fs::create_dir_all(&config.log_dir)?;
            config.log_dir = fs::canonicalize(&config.log_dir)?;
            ensure!(!config.ticket.is_empty(), "ticket must not be empty");
            for (key, value) in &config.env {
                ensure!(
                    !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0'),
                    "invalid command environment"
                );
            }
            for patterns in config.paths.values() {
                for pattern in patterns {
                    ensure!(
                        !pattern.is_empty()
                            && !pattern.starts_with('/')
                            && !pattern.contains(['[', ']', '\\', '\0'])
                            && !pattern.split('/').any(|p| p == ".." || p == "."),
                        "unsupported path glob {pattern:?}"
                    );
                }
            }
            git_at(
                &config.workdir,
                &["check-ref-format", &format!("refs/heads/{}", config.branch)],
            )?;
            config.base = git_text(
                &config.workdir,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("{}^{{commit}}", config.base),
                ],
            )?;
            Ok(config)
        };
        Ok(Self {
            config: prepare().map_err(|e| block(BlockKind::Capability, e))?,
            context: AttemptContext::default(),
            receipt: None,
        })
    }
    /// The host binds metadata and independent test expectations before advancing
    /// this attempt. Changing context revokes any prior successful check receipt.
    pub fn set_attempt(&mut self, context: AttemptContext) {
        self.context = context;
        self.receipt = None;
    }
    fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        git_at(&self.config.workdir, args)
    }
    fn text(&self, args: &[&str]) -> Result<String> {
        git_text(&self.config.workdir, args)
    }
    fn bound_head(&self) -> Result<String> {
        ensure!(
            self.context.number > 0 && !self.context.bot.trim().is_empty(),
            "attempt number and bot are required"
        );
        ensure!(
            self.text(&["symbolic-ref", "--quiet", "--short", "HEAD"])? == self.config.branch,
            "HEAD is not on the configured run branch"
        );
        // Evidence describes committed work only. Dirty changes cannot inherit a
        // commit's successful checks, including edits to frozen tests.
        ensure!(
            self.git(&["status", "--porcelain", "--untracked-files=all"])?
                .is_empty(),
            "worktree has uncommitted changes"
        );
        let head = self.text(&["rev-parse", "--verify", "HEAD^{commit}"])?;
        self.git(&["merge-base", "--is-ancestor", &self.config.base, &head])?;
        Ok(head)
    }
    fn paths_between(&self, from: &str, to: &str) -> Result<Vec<String>> {
        let bytes = self.git(&["diff", "--name-only", "--no-renames", "-z", from, to, "--"])?;
        bytes
            .split(|b| *b == 0)
            .filter(|b| !b.is_empty())
            .map(|b| String::from_utf8(b.to_vec()).context("git path must be UTF-8"))
            .collect()
    }
    fn frozen(&self, kind: &str, reference: &str, head: &str) -> Result<()> {
        let tag_ref = format!("refs/tags/{reference}");
        self.git(&["check-ref-format", &tag_ref])?;
        let object = self.text(&["rev-parse", "--verify", &format!("{tag_ref}^{{tag}}")])?;
        let note = self.load_note(&object)?;
        ensure!(
            note.schema == 1
                && note.ticket == self.config.ticket
                && note.base == self.config.base
                && note.branch == self.config.branch
                && note.tag == kind,
            "freeze belongs to a different run or base"
        );
        ensure!(
            self.text(&["notes", "--ref=hermon", "show", &object])? == self.annotation(&object)?,
            "freeze annotation and explicit note disagree"
        );
        let commit = self.text(&["rev-parse", "--verify", &format!("{tag_ref}^{{commit}}")])?;
        ensure!(note.commit == commit, "freeze target differs from its note");
        ensure!(
            note.frozen_paths == self.paths_between(&self.config.base, &commit)?,
            "freeze paths differ from the recorded RED delta"
        );
        self.git(&["merge-base", "--is-ancestor", &commit, head])?;
        // Comparing the complete delta avoids constructing pathspecs from file
        // names (which may themselves contain Git pathspec metacharacters).
        let changed = self.paths_between(&commit, head)?;
        ensure!(
            !changed.iter().any(|path| note.frozen_paths.contains(path)),
            "frozen paths changed since {reference}"
        );
        Ok(())
    }
    fn load_note(&self, object: &str) -> Result<EvidenceNote> {
        let annotation = self.annotation(object)?;
        ensure!(annotation.len() < 8192, "oversized evidence note");
        Ok(serde_json::from_str(&annotation)?)
    }
    fn annotation(&self, object: &str) -> Result<String> {
        let raw = self.text(&["cat-file", "tag", object])?;
        raw.split_once("\n\n")
            .map(|(_, note)| note.trim().to_owned())
            .context("missing tag annotation")
    }
    fn run_command(&self, name: &str) -> std::result::Result<CommandEvidence, Block> {
        let config = self.config.commands.get(name).ok_or_else(|| {
            Block::new(
                BlockKind::Capability,
                format!("missing project command {name}"),
            )
        })?;
        if config.argv.is_empty()
            || config.timeout_seconds == 0
            || config.argv.iter().any(|s| s.contains('\0'))
        {
            return Err(Block::new(
                BlockKind::Capability,
                format!("invalid argv or timeout for {name}"),
            ));
        }
        let executable = resolve_executable(&config.argv[0], &self.config)
            .map_err(|e| block(BlockKind::Capability, e))?;
        let execute = || -> Result<CommandEvidence> {
            let directory = tempfile::Builder::new()
                .prefix("check-")
                .tempdir_in(&self.config.log_dir)?
                .keep();
            let stdout_path = directory.join("stdout");
            let stderr_path = directory.join("stderr");
            let mut command = Command::new(executable);
            command
                .args(&config.argv[1..])
                .current_dir(&self.config.workdir)
                .env_clear()
                .envs(&self.config.env)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(File::create(&stdout_path)?)
                .stderr(File::create(&stderr_path)?);
            let started = Instant::now();
            let child = command.spawn().context("spawn project command")?;
            let mut owned = OwnedChild::new(child);
            let timeout = Duration::from_secs(config.timeout_seconds);
            let mut timed_out = false;
            loop {
                if started.elapsed() >= timeout {
                    timed_out = true;
                    owned.signal(libc::SIGTERM)?;
                    thread::sleep(Duration::from_millis(100));
                    break;
                }
                if owned.exited()? {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            let status = owned.finish()?;
            let log_path = directory.join("output.log");
            let mut log = File::create(&log_path)?;
            writeln!(log, "stdout:")?;
            std::io::copy(&mut File::open(&stdout_path)?, &mut log)?;
            writeln!(log, "\nstderr:")?;
            std::io::copy(&mut File::open(&stderr_path)?, &mut log)?;
            writeln!(log, "\nexit: {:?}; timeout: {timed_out}", status.code())?;
            log.sync_all()?;
            ensure!(
                !timed_out,
                "{name} exceeded its check timeout; log: {}",
                log_path.display()
            );
            let tests = config
                .report
                .as_ref()
                .map(|format| {
                    parse_report(&stdout_path, format)
                        .with_context(|| format!("parse test report; log: {}", log_path.display()))
                })
                .transpose()?;
            Ok(CommandEvidence {
                name: name.into(),
                argv: config.argv.clone(),
                exit_code: status.code(),
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                tests,
                log: LogRef {
                    sha256: hash_file(&log_path)?,
                    path: log_path,
                },
            })
        };
        execute().map_err(|e| {
            let reason = format!("{e:#}");
            let kind = if reason.contains("exceeded its check timeout") {
                BlockKind::Transient
            } else if reason.contains("spawn project command") {
                BlockKind::Capability
            } else {
                BlockKind::Unknown
            };
            Block::new(kind, reason)
        })
    }
    fn write_note(&self, object: &str, text: &str) -> Result<()> {
        let path = self.note_file(text)?;
        self.git(&[
            "notes",
            "--ref=hermon",
            "add",
            "-F",
            path.path().to_str().context("note path must be UTF-8")?,
            object,
        ])?;
        Ok(())
    }
    fn note_file(&self, text: &str) -> Result<tempfile::NamedTempFile> {
        let mut file = tempfile::NamedTempFile::new_in(&self.config.log_dir)?;
        file.write_all(text.as_bytes())?;
        file.as_file().sync_all()?;
        Ok(file)
    }
    fn evidence_inner(&mut self, step: &str, tag: &str, revision: u32) -> Result<String> {
        ensure!(
            revision > 0 && !tag.is_empty() && !step.is_empty(),
            "invalid evidence key"
        );
        let reference = format!(
            "hermon/{}/{tag}{}",
            self.config.ticket,
            if revision == 1 {
                String::new()
            } else {
                format!(".{revision}")
            }
        );
        let full_ref = format!("refs/tags/{reference}");
        self.git(&["check-ref-format", &full_ref])?;
        // Recover a tag created before the hub persisted its snapshot or before
        // the explicit git note was written. Never move an existing key.
        if let Ok(object) = self.text(&["rev-parse", "--verify", &full_ref]) {
            let note = self.load_note(&object)?;
            ensure!(
                note.schema == 1
                    && note.ticket == self.config.ticket
                    && note.step == step
                    && note.tag == tag
                    && note.revision == revision
                    && note.attempt == self.context.number
                    && note.bot == self.context.bot
                    && note.model == self.context.model
                    && note.base == self.config.base
                    && note.branch == self.config.branch,
                "existing evidence key has a different identity"
            );
            ensure!(
                self.text(&["rev-parse", &format!("{full_ref}^{{commit}}")])? == note.commit,
                "existing evidence target mismatch"
            );
            let annotation = self.annotation(&object)?;
            match self.text(&["notes", "--ref=hermon", "show", &object]) {
                Ok(existing) => ensure!(
                    existing == annotation,
                    "existing git note differs from annotation"
                ),
                Err(_) => self.write_note(&object, &annotation)?,
            }
            return Ok(reference);
        }
        let head = self.bound_head()?;
        let receipt = self
            .receipt
            .as_ref()
            .context("successful checks are required before evidence")?;
        ensure!(
            receipt.head == head && receipt.step == step,
            "checks do not cover the current step and HEAD"
        );
        for command in &receipt.commands {
            ensure!(
                hash_file(&command.log.path)? == command.log.sha256,
                "check log changed before evidence: {}",
                command.log.path.display()
            );
        }
        let note = EvidenceNote {
            schema: 1,
            ticket: self.config.ticket.clone(),
            step: step.into(),
            tag: tag.into(),
            revision,
            attempt: self.context.number,
            bot: self.context.bot.clone(),
            model: self.context.model.clone(),
            branch: self.config.branch.clone(),
            base: self.config.base.clone(),
            commit: head,
            commands: receipt.commands.clone(),
            frozen_paths: receipt.paths.clone(),
        };
        let text = serde_json::to_string(&note)?;
        ensure!(
            text.len() < 8192,
            "evidence metadata exceeds the 8 KB note limit"
        );
        let file = self.note_file(&text)?;
        self.git(&[
            "tag",
            "--no-sign",
            "-a",
            &reference,
            &receipt.head,
            "-F",
            file.path().to_str().context("note path must be UTF-8")?,
        ])?;
        let object = self.text(&["rev-parse", "--verify", &full_ref])?;
        self.write_note(&object, &text)?;
        Ok(reference)
    }
}
impl Git for GitChecks {
    fn check(
        &mut self,
        step: &str,
        checks: &[String],
        freezes: &BTreeMap<String, String>,
    ) -> std::result::Result<(), Block> {
        self.receipt = None;
        let head = self
            .bound_head()
            .map_err(|e| block(BlockKind::Unknown, e))?;
        let paths = self
            .paths_between(&self.config.base, &head)
            .map_err(|e| block(BlockKind::Unknown, e))?;
        let mut commands = Vec::new();
        for check in checks {
            if let Some(set) = check.strip_prefix("only_paths_changed:") {
                let patterns = self.config.paths.get(set).ok_or_else(|| {
                    Block::new(BlockKind::Capability, format!("unknown path set {set}"))
                })?;
                if let Some(path) = paths
                    .iter()
                    .find(|path| !patterns.iter().any(|pattern| glob_matches(pattern, path)))
                {
                    return Err(Block::new(
                        BlockKind::Unknown,
                        format!("path {path:?} is outside {set}"),
                    ));
                }
            } else if let Some(tag) = check.strip_prefix("frozen_unchanged:") {
                let reference = freezes.get(tag).ok_or_else(|| {
                    Block::new(BlockKind::Unknown, format!("missing freeze {tag}"))
                })?;
                self.frozen(tag, reference, &head)
                    .map_err(|e| block(BlockKind::Unknown, e))?;
            } else {
                let name = match check.as_str() {
                    "lint" => "lint",
                    "project_check" => "project_check",
                    "tests_fail" => "test",
                    other => other
                        .strip_prefix("cmd:")
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            Block::new(BlockKind::Capability, format!("unknown check {other}"))
                        })?,
                };
                let command = self.run_command(name)?;
                if check == "tests_fail" {
                    let report = command.tests.as_ref().ok_or_else(|| {
                        Block::new(
                            BlockKind::Capability,
                            "tests_fail requires a configured test report format",
                        )
                    })?;
                    let failures: BTreeSet<_> = report.failures.iter().cloned().collect();
                    if !command.exit_code.is_some_and(|code| code != 0)
                        || report.failed == 0
                        || self.context.new_tests.is_empty()
                        || !self.context.new_tests.is_subset(&failures)
                        || !self
                            .context
                            .new_tests
                            .is_disjoint(&self.context.baseline_failures)
                    {
                        return Err(Block::new(
                            BlockKind::Unknown,
                            format!(
                                "tests_fail did not establish all expected new failures; log: {}",
                                command.log.path.display()
                            ),
                        ));
                    }
                } else if command.exit_code != Some(0) {
                    return Err(Block::new(
                        BlockKind::Unknown,
                        format!(
                            "{name} failed with exit {:?}; log: {}",
                            command.exit_code,
                            command.log.path.display()
                        ),
                    ));
                }
                commands.push(command);
            }
        }
        let after = self
            .bound_head()
            .map_err(|e| block(BlockKind::Unknown, e))?;
        if after != head {
            return Err(Block::new(
                BlockKind::Unknown,
                "HEAD changed while checks ran",
            ));
        }
        self.receipt = Some(Receipt {
            step: step.into(),
            head,
            commands,
            paths,
        });
        Ok(())
    }
    fn evidence(
        &mut self,
        step: &str,
        tag: &str,
        revision: u32,
    ) -> std::result::Result<String, Block> {
        self.evidence_inner(step, tag, revision)
            .map_err(|e| block(BlockKind::Unknown, e))
    }
}
fn block(kind: BlockKind, error: anyhow::Error) -> Block {
    Block::new(kind, format!("{error:#}"))
}
fn git_at(workdir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("/usr/bin/git");
    command
        .current_dir(workdir)
        .args([
            "-c",
            "user.name=Hermon",
            "-c",
            "user.email=hermon@localhost",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(args)
        .stdin(Stdio::null());
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(key);
    }
    let output = command.output().context("run git")?;
    ensure!(
        output.status.success(),
        "git {}: {}",
        args.first().unwrap_or(&""),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}
fn git_text(workdir: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git_at(workdir, args)?)?.trim().into())
}
fn resolve_executable(binary: &str, config: &CheckConfig) -> Result<PathBuf> {
    let paths = if binary.contains('/') {
        vec![config.workdir.join(binary)]
    } else {
        let path = config
            .env
            .get("PATH")
            .context("configured environment has no PATH")?;
        std::env::split_paths(path)
            .map(|p| config.workdir.join(p).join(binary))
            .collect()
    };
    paths
        .into_iter()
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
        .with_context(|| {
            format!("command {binary:?} is not executable under the configured environment")
        })
}
fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn parse_report(path: &Path, format: &TestReportFormat) -> Result<TestReport> {
    const MAX_LINE: u64 = 1024 * 1024;
    let mut file = File::open(path)?;
    let report: TestReport = match format {
        TestReportFormat::Json => {
            // JSON reports occupy the final nonempty line; earlier output is
            // diagnostic. Read only a bounded tail, never half of a report.
            let start = file.metadata()?.len().saturating_sub(MAX_LINE);
            file.seek(SeekFrom::Start(start))?;
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            let text = if start > 0 {
                text.split_once('\n')
                    .map(|(_, tail)| tail)
                    .context("test report exceeds parsing limit")?
            } else {
                &text
            };
            serde_json::from_str(
                text.lines()
                    .rfind(|l| !l.trim().is_empty())
                    .context("missing test report")?,
            )?
        }
        TestReportFormat::Libtest => {
            // Every target's summary contributes to evidence. Streaming the
            // whole log prevents a bounded tail from silently dropping targets.
            let mut reader = BufReader::new(file);
            let mut report = TestReport {
                passed: 0,
                failed: 0,
                ignored: 0,
                failures: vec![],
            };
            let mut summaries = 0;
            let mut failure_bytes = 0;
            let mut line = String::new();
            loop {
                line.clear();
                let bytes = Read::by_ref(&mut reader)
                    .take(MAX_LINE + 1)
                    .read_line(&mut line)?;
                ensure!(
                    u64::try_from(bytes)? <= MAX_LINE,
                    "libtest line exceeds parsing limit"
                );
                if bytes == 0 {
                    break;
                }
                let line = line.trim_end_matches(['\r', '\n']);
                if let Some(id) = line
                    .strip_prefix("test ")
                    .and_then(|l| l.strip_suffix(" ... FAILED"))
                {
                    failure_bytes += id.len();
                    ensure!(
                        u64::try_from(failure_bytes)? <= MAX_LINE,
                        "test failure IDs exceed parsing limit"
                    );
                    report.failures.push(id.into());
                    ensure!(
                        report.failures.len() <= 100_000,
                        "too many test failure IDs"
                    );
                }
                if let Some(summary) = line.strip_prefix("test result: ") {
                    let (_, counts) = summary
                        .split_once(". ")
                        .context("invalid libtest summary")?;
                    let fields: Vec<_> = counts.split(';').collect();
                    for (index, suffix, count) in [
                        (0, " passed", &mut report.passed),
                        (1, " failed", &mut report.failed),
                        (2, " ignored", &mut report.ignored),
                    ] {
                        let n: u64 = fields
                            .get(index)
                            .context("missing libtest counts")?
                            .trim()
                            .strip_suffix(suffix)
                            .context("invalid libtest count")?
                            .parse()?;
                        *count = count.checked_add(n).context("test count overflow")?;
                    }
                    summaries += 1;
                }
            }
            ensure!(summaries > 0, "missing libtest result summary");
            report
        }
    };
    let unique: BTreeSet<_> = report.failures.iter().collect();
    ensure!(
        unique.len() == report.failures.len()
            && u64::try_from(unique.len())? == report.failed
            && unique.iter().all(|s| !s.trim().is_empty()),
        "test counts and unique failure IDs disagree"
    );
    Ok(report)
}
fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<_> = pattern.split('/').collect();
    let path: Vec<_> = path.split('/').collect();
    let mut previous = vec![false; path.len() + 1];
    previous[0] = true;
    for part in pattern {
        let mut next = vec![false; path.len() + 1];
        if part == "**" {
            next[0] = previous[0];
        }
        for i in 1..=path.len() {
            next[i] = if part == "**" {
                previous[i] || next[i - 1]
            } else {
                previous[i - 1] && component_matches(part.as_bytes(), path[i - 1].as_bytes())
            };
        }
        previous = next;
    }
    previous[path.len()]
}
fn component_matches(pattern: &[u8], value: &[u8]) -> bool {
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for c in pattern {
        let mut next = vec![false; value.len() + 1];
        if *c == b'*' {
            next[0] = previous[0];
        }
        for i in 1..=value.len() {
            next[i] = if *c == b'*' {
                previous[i] || next[i - 1]
            } else {
                previous[i - 1] && (*c == b'?' || *c == value[i - 1])
            };
        }
        previous = next;
    }
    previous[value.len()]
}
