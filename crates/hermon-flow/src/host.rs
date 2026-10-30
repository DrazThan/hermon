//! Persistent host for the deterministic hub. All hosts share a kernel lock;
//! each reconciliation also holds the same per-run lock as CLI controls.
use crate::{
    agent::{ProcessAgent, lock},
    checks::{AttemptContext, CheckCommand, CheckConfig, GitChecks, TestReportFormat},
    code_host::{GithubCodeHost, GithubConfig},
    control::{self, RunRecord},
    hub::{self, Agent, Block, BlockKind, CodeHost, Git, Poll, Settlement, StartRequest, Status},
    intake::{self, Project, identifier},
    templates::Templates,
    workflow::Workflow,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The file is never unlinked: a replacement inode would allow two owners.
/// Closing the descriptor (including process death) releases the kernel lock.
#[derive(Debug)]
pub struct HubLock {
    _file: File,
}
impl HubLock {
    pub fn acquire(data: &Path) -> Result<Option<Self>> {
        fs::create_dir_all(data)?;
        Ok(lock(&data.join("hub.lock"), true)?.map(|file| Self { _file: file }))
    }
}

fn ids(data: &Path) -> Result<Vec<String>> {
    let root = data.join("runs");
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut ids = vec![];
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with('.')
            && entry.file_type()?.is_dir()
            && entry.path().join("run.json").is_file()
        {
            ids.push(name);
        }
    }
    ids.sort();
    Ok(ids)
}

/// Read available snapshots without blocking transcript polling on an executing
/// check. A busy run keeps the UI's previous snapshot until its next refresh.
pub fn snapshots(data: &Path) -> Result<Vec<RunRecord>> {
    let mut records = vec![];
    for id in ids(data)? {
        let read = || -> Result<Option<RunRecord>> {
            let dir = control::directory(data, &id)?;
            let Some(_guard) = lock(&dir.join("run.lock"), true)? else {
                return Ok(None);
            };
            Ok(Some(control::read(&dir, &id)?))
        };
        match read() {
            Ok(Some(record)) => records.push(record),
            Ok(None) => {}
            Err(error) => eprintln!("hermon hub: {id}: {error:#}"),
        }
    }
    Ok(records)
}

pub struct Host {
    _lock: HubLock,
    data: PathBuf,
    worker: PathBuf,
    embedded: bool,
}
impl Host {
    pub fn acquire(data: &Path, worker: &Path) -> Result<Option<Self>> {
        let Some(guard) = HubLock::acquire(data)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            _lock: guard,
            data: fs::canonicalize(data)?,
            worker: worker.to_owned(),
            embedded: false,
        }))
    }
    pub fn embedded_worker(mut self) -> Self {
        self.embedded = true;
        self
    }

    /// Failures are isolated to one run. Even when an external effect fails,
    /// persist the hub's observations before the next tick can retry it.
    pub fn tick(&mut self) -> Result<Vec<RunRecord>> {
        let mut records = vec![];
        for id in ids(&self.data)? {
            match self.tick_run(&id) {
                Ok(Some(record)) => records.push(record),
                Ok(None) => {}
                Err(error) => eprintln!("hermon hub: {id}: {error:#}"),
            }
        }
        Ok(records)
    }
    fn tick_run(&self, id: &str) -> Result<Option<RunRecord>> {
        let dir = control::directory(&self.data, id)?;
        let Some(_guard) = lock(&dir.join("run.lock"), true)? else {
            return Ok(None);
        };
        let mut record = control::read(&dir, id)?;
        if let Err(error) = self.advance(&dir, &mut record) {
            let reason = format!("Host needs attention: {error:#}");
            if !record
                .run
                .events
                .last()
                .is_some_and(|e| e.kind == "host_error" && e.reason == reason)
            {
                record.run.events.push(hub::Event {
                    kind: "host_error".into(),
                    step: record.run.current.clone(),
                    reason: reason.clone(),
                });
            }
            eprintln!("hermon hub: {id}: {reason}");
        } else if record
            .run
            .events
            .last()
            .is_some_and(|e| e.kind == "host_error")
        {
            record.run.events.push(hub::Event {
                kind: "host_recovered".into(),
                step: record.run.current.clone(),
                reason: "Host resumed reconciliation".into(),
            });
        }
        control::save(&dir, &record)?;
        Ok(Some(record))
    }
    fn advance(&self, dir: &Path, record: &mut RunRecord) -> Result<()> {
        if record.cancelled || record.run.cancelled {
            if record.run.active.is_some() {
                control::request_stop(&self.data, dir, record)?;
            }
            return Ok(());
        }
        if record.paused || record.run.paused || record.run.status == Status::Done {
            return Ok(());
        }
        let workflow = Workflow::from_str("run workflow snapshot", &record.workflow_source)?;
        let project = Project::load(&record.project)?;
        if !record.started {
            intake::announce(dir, record)?;
            control::save(dir, record)?;
        }
        control::flush_notices(dir, record)?;
        let templates = Templates::load(None, Some(&dir.join("context/templates")))?;
        let mut tracker = project.tracker(dir.join("tracker"), &record.ticket, templates)?;
        let mut process = ProcessAgent::new(
            self.data.join("runs"),
            &record.worktree,
            &self.worker,
            project.bots.clone(),
            Duration::from_secs(u64::from(workflow.limits.attempt_timeout_minutes) * 60),
        )?
        .with_env(project.env.clone());
        if self.embedded {
            process = process.with_worker_subcommand("__flow-worker");
        }
        let mut agent = PreparedAgent {
            process,
            dir,
            record,
            workflow: &workflow,
        };
        let mut git = HostGit {
            project: &project,
            record,
            dir,
            checks: None,
        };
        let mut code_host = HostCodeHost {
            project: &project,
            record,
            dir,
            host: None,
        };
        // Adapters use immutable intake bindings while advance mutates the run.
        let mut run = record.run.clone();
        let result = hub::advance(
            &mut run,
            &workflow,
            &mut hub::Dependencies {
                agent: &mut agent,
                tracker: &mut tracker,
                code_host: &mut code_host,
                git: &mut git,
                clock: &WallClock,
            },
        );
        record.run = run;
        result
    }
}
struct WallClock;
impl hub::Clock for WallClock {
    fn now(&self) -> u64 {
        now()
    }
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn capability(error: impl std::fmt::Display) -> Block {
    Block::new(BlockKind::Capability, error.to_string())
}
fn attempt_dir(dir: &Path, step: &str, number: u32) -> Result<PathBuf> {
    identifier(step)?;
    ensure!(number > 0, "attempt must be positive");
    let path = dir.join("steps").join(step).join(number.to_string());
    fs::create_dir_all(&path)?;
    let path = fs::canonicalize(path)?;
    ensure!(
        path.starts_with(fs::canonicalize(dir)?),
        "attempt outside run directory"
    );
    Ok(path)
}
fn artifact(dir: &Path, name: &str) -> Result<PathBuf> {
    ensure!(
        !name.is_empty()
            && Path::new(name)
                .components()
                .all(|p| matches!(p, Component::Normal(_))),
        "invalid artifact path"
    );
    let path = fs::canonicalize(dir.join(name))?;
    ensure!(
        path.starts_with(fs::canonicalize(dir)?),
        "artifact outside attempt"
    );
    Ok(path)
}
fn latest(dir: &Path, step: &str) -> Result<PathBuf> {
    identifier(step)?;
    let number = fs::read_dir(dir.join("steps").join(step))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .max()
        .context("missing step attempt")?;
    attempt_dir(dir, step, number)
}
fn read_text(path: &Path) -> Result<String> {
    ensure!(
        fs::metadata(path)?.len() <= 1024 * 1024,
        "context artifact exceeds 1 MiB"
    );
    Ok(fs::read_to_string(path)?)
}
struct PreparedAgent<'a> {
    process: ProcessAgent,
    dir: &'a Path,
    record: &'a RunRecord,
    workflow: &'a Workflow,
}
impl PreparedAgent<'_> {
    fn prepare(&self, request: &StartRequest) -> Result<()> {
        let dir = attempt_dir(self.dir, &request.step, request.number)?;
        if dir.join("task.md").exists() {
            return Ok(());
        }
        let step = &self.workflow.steps[&request.step];
        let mut task = format!(
            "# {} / {}\n\nTicket: {}\nBot: {}\nAttempt: {}\nWorktree: {}\n\nComplete this workflow step. Commit repository changes before reporting done. Write result.json to {} with all keys: {{\"schema\":1,\"status\":\"done\",\"summary\":\"...\",\"artifacts\":[],\"block\":null,\"verdict\":null,\"questions\":[]}}. For a block use status block and block {{kind,reason}}. Review verdict must be clean or changes_needed. Save produced artifacts beside result.json.\n\nProduces: {:?}\nChecks: {:?}\n",
            self.record.run.id,
            request.step,
            self.record.ticket,
            request.bot,
            request.number,
            self.record.worktree.display(),
            dir.display(),
            step.produces,
            step.check
        );
        if let Ok(issue) = read_text(&self.dir.join("context/issue.md")) {
            task.push_str(&format!("\n## Ticket context\n{issue}\n"));
        }
        for input in &step.inputs {
            let body = if input == "block_reason" {
                self.record
                    .run
                    .block
                    .as_ref()
                    .map(|b| b.reason.clone())
                    .unwrap_or_default()
            } else if let Some(reference) = request.freezes.get(input) {
                reference.clone()
            } else if input.starts_with("diff:") || input == "merge" {
                format!(
                    "Resolve {input} from run evidence: {:?}; PR: {:?}",
                    request.freezes, self.record.run.pr
                )
            } else {
                let producer = self
                    .workflow
                    .steps
                    .iter()
                    .find(|(name, step)| {
                        step.produces.contains(input)
                            && self
                                .record
                                .run
                                .events
                                .iter()
                                .any(|e| e.step == **name && e.kind == "step_done")
                    })
                    .map(|(name, _)| name)
                    .with_context(|| format!("missing accepted input {input}"))?;
                read_text(&artifact(&latest(self.dir, producer)?, input)?)?
            };
            task.push_str(&format!("\n## Input: {input}\n{body}\n"));
        }
        let mut file = tempfile::NamedTempFile::new_in(&dir)?;
        file.write_all(task.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(dir.join("task.md"))?;
        Ok(())
    }
}
impl Agent for PreparedAgent<'_> {
    fn family(&self, bot: &str) -> String {
        self.process.family(bot)
    }
    fn available(&self, bot: &str) -> bool {
        self.process.available(bot)
    }
    fn start(&mut self, request: &StartRequest) -> std::result::Result<String, Block> {
        self.prepare(request).map_err(capability)?;
        self.process.start(request)
    }
    fn resume_after_answer(
        &mut self,
        previous: &str,
        request: &StartRequest,
        answer: &str,
    ) -> std::result::Result<String, Block> {
        self.prepare(request).map_err(capability)?;
        self.process.resume_after_answer(previous, request, answer)
    }
    fn poll(&mut self, handle: &str) -> std::result::Result<Poll, Block> {
        self.process.poll(handle)
    }
    fn stop(&mut self, handle: &str) -> std::result::Result<Settlement, Block> {
        self.process.stop(handle)
    }
}

struct HostGit<'a> {
    project: &'a Project,
    record: &'a RunRecord,
    dir: &'a Path,
    checks: Option<GitChecks>,
}
impl Git for HostGit<'_> {
    fn check(
        &mut self,
        step: &str,
        checks: &[String],
        freezes: &BTreeMap<String, String>,
    ) -> std::result::Result<(), Block> {
        // A workflow with neither checks nor an evidence tag needs no git adapter.
        let workflow = Workflow::from_str("workflow snapshot", &self.record.workflow_source)
            .map_err(capability)?;
        if checks.is_empty() && workflow.steps[step].tag.is_none() {
            return Ok(());
        }
        let dir = latest(self.dir, step).map_err(capability)?;
        let number = dir
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let bot = workflow.steps[step].bot.clone().unwrap_or_default();
        let mut git = GitChecks::new(CheckConfig {
            workdir: self.record.worktree.clone(),
            base: self.record.base_commit.clone(),
            ticket: self.record.ticket.clone(),
            branch: self.record.branch.clone(),
            log_dir: dir.join("checks"),
            env: self.project.env.clone(),
            paths: self.project.paths.clone(),
            commands: self
                .project
                .commands
                .iter()
                .map(|(name, c)| {
                    (
                        name.clone(),
                        CheckCommand {
                            argv: c.argv.clone(),
                            timeout_seconds: c.timeout_seconds,
                            report: (name == "test").then_some(TestReportFormat::Libtest),
                        },
                    )
                })
                .collect(),
        })?;
        // Trusted expected new test IDs are supplied separately from agent output.
        let mut context = AttemptContext {
            number,
            bot,
            ..AttemptContext::default()
        };
        let expected = self.dir.join("context/test-expectations.json");
        if expected.exists() {
            let values: serde_json::Value =
                serde_json::from_str(&read_text(&expected).map_err(capability)?)
                    .map_err(capability)?;
            context.new_tests =
                serde_json::from_value(values["new_tests"].clone()).map_err(capability)?;
            context.baseline_failures =
                serde_json::from_value(values["baseline_failures"].clone()).map_err(capability)?;
        }
        git.set_attempt(context);
        git.check(step, checks, freezes)?;
        self.checks = Some(git);
        Ok(())
    }
    fn evidence(
        &mut self,
        step: &str,
        tag: &str,
        revision: u32,
    ) -> std::result::Result<String, Block> {
        self.checks
            .as_mut()
            .ok_or_else(|| capability("missing successful check receipt"))?
            .evidence(step, tag, revision)
    }
}
struct HostCodeHost<'a> {
    project: &'a Project,
    record: &'a RunRecord,
    dir: &'a Path,
    host: Option<GithubCodeHost>,
}
impl HostCodeHost<'_> {
    fn get(&mut self) -> std::result::Result<&mut GithubCodeHost, Block> {
        if self.host.is_none() {
            let plan = latest(self.dir, "plan")
                .and_then(|dir| read_text(&artifact(&dir, "plan.md")?))
                .unwrap_or_else(|_| self.record.ticket.clone());
            let review = latest(self.dir, "review")
                .and_then(|dir| read_text(&dir.join("result.json")))
                .unwrap_or_else(|_| "Workflow review completed".into());
            self.host = Some(GithubCodeHost::new(
                GithubConfig {
                    workdir: self.record.worktree.clone(),
                    repo: self.project.code_host.repo.clone(),
                    remote: self.project.code_host.remote.clone(),
                    base: self.record.base.clone(),
                    branch: self.record.branch.clone(),
                    run_id: self.record.run.id.clone(),
                    title: format!("{}: {}", self.record.ticket, self.record.workflow_name),
                    plan,
                    review_summary: review,
                    env: self.project.env.clone(),
                },
                self.dir.join("code-host"),
            )?);
        }
        Ok(self.host.as_mut().unwrap())
    }
}
impl CodeHost for HostCodeHost<'_> {
    fn open_pr(&mut self, id: &str) -> std::result::Result<String, Block> {
        self.get()?.open_pr(id)
    }
    fn merged(&mut self, pr: &str) -> std::result::Result<bool, Block> {
        self.get()?.merged(pr)
    }
}
