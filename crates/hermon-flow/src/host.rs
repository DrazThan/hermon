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
    tracker::LinearTracker,
    workflow::Workflow,
};
use anyhow::{Context, Result, ensure};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Component, Path, PathBuf},
    rc::Rc,
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
        if record.paused || record.run.paused {
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
        if record.run.status == Status::Done {
            return publish_outcome(&mut tracker, dir, record);
        }
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
        let observations = Rc::new(RefCell::new(Observations::default()));
        let mut agent = PreparedAgent {
            process,
            dir,
            record,
            workflow: &workflow,
            observations: observations.clone(),
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
            observations: observations.clone(),
        };
        let mut host_tracker = HostTracker {
            inner: &mut tracker,
            dir,
            record,
            workflow: &workflow,
            observations,
        };
        // Adapters use immutable intake bindings while advance mutates the run.
        let mut run = record.run.clone();
        let result = hub::advance(
            &mut run,
            &workflow,
            &mut hub::Dependencies {
                agent: &mut agent,
                tracker: &mut host_tracker,
                code_host: &mut code_host,
                git: &mut git,
                clock: &WallClock,
            },
        );
        record.run = run;
        result?;
        if record.run.status == Status::Done {
            publish_outcome(&mut tracker, dir, record)?;
        }
        Ok(())
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
/// Numeric directories and worker results do not establish acceptance. Match
/// ordered hub start/completion events to the recorded attempts instead.
fn accepted(dir: &Path, record: &RunRecord, step: &str) -> Result<PathBuf> {
    let mut attempts = record.run.attempts.iter().filter(|a| a.step == step);
    let mut current = None;
    let mut accepted = None;
    for event in record.run.events.iter().filter(|e| e.step == step) {
        if event.kind == "started" {
            current = attempts.next();
        } else if event.kind == "step_done" {
            accepted = current;
        }
    }
    let attempt = accepted.context("missing accepted artifact attempt")?;
    attempt_dir(dir, step, attempt.number)
}
fn read_text(path: &Path) -> Result<String> {
    ensure!(
        fs::metadata(path)?.len() <= 1024 * 1024,
        "context artifact exceeds 1 MiB"
    );
    Ok(fs::read_to_string(path)?)
}
#[derive(Default)]
struct Observations {
    completed: BTreeMap<String, PathBuf>,
    block_reason: Option<String>,
}
fn producer_dir(
    dir: &Path,
    record: &RunRecord,
    observations: &Observations,
    step: &str,
) -> Result<PathBuf> {
    match observations.completed.get(step) {
        Some(directory) => Ok(directory.clone()),
        None => accepted(dir, record, step),
    }
}
struct PreparedAgent<'a> {
    process: ProcessAgent,
    dir: &'a Path,
    record: &'a RunRecord,
    workflow: &'a Workflow,
    // A successor can start within the same advance call, before its producer's
    // step_done event has been copied back into the durable run snapshot.
    observations: Rc<RefCell<Observations>>,
}
impl PreparedAgent<'_> {
    fn prepare(&self, request: &StartRequest) -> Result<()> {
        let dir = attempt_dir(self.dir, &request.step, request.number)?;
        if dir.join("task.md").exists() {
            return Ok(());
        }
        let step = &self.workflow.steps[&request.step];
        if step
            .check
            .iter()
            .any(|check| check.starts_with("only_paths_changed:"))
            && step
                .tag
                .as_ref()
                .is_some_and(|tag| request.freezes.contains_key(tag))
        {
            let base = dir.join("base_commit");
            if !base.exists() {
                let head = crate::checks::worktree_head(&self.record.worktree)?;
                let mut file = tempfile::NamedTempFile::new_in(&dir)?;
                file.write_all(head.as_bytes())?;
                file.as_file().sync_all()?;
                file.persist(base)?;
            }
        }
        let mut task = format!(
            "# {} / {}\n\nTicket: {}\nBot: {}\nAttempt: {}\nWorktree: {}\n\nComplete this workflow step. Commit repository changes before reporting done. Write result.json to {} with all keys: {{\"schema\":1,\"status\":\"done\",\"summary\":\"...\",\"artifacts\":[],\"block\":null,\"verdict\":null,\"questions\":[]}}. For a block use status block and block {{kind,reason}}. The block kind must be one of: owner_question, test_defect, capability, transient, scope_change, unknown. Review verdict must be clean or changes_needed. Save produced artifacts beside result.json.\n\nProduces: {:?}\nChecks: {:?}\n",
            self.record.run.id,
            request.step,
            self.record.ticket,
            request.bot,
            request.number,
            self.record.worktree.display(),
            dir.join("result.json").display(),
            step.produces,
            step.check
        );
        task.push_str("\nDo not start other workflow steps, poll a board, pass owner gates, merge or deploy. Report questions and blocks through result.json; the hub routes them.\n");
        let manifest = self.dir.join("context/roles.json");
        match fs::read_to_string(&manifest) {
            Ok(body) => {
                let hashes: BTreeMap<String, String> =
                    serde_json::from_str(&body).context("read role manifest")?;
                if let Some(expected) = hashes.get(&request.step) {
                    let role = read_text(
                        &self
                            .dir
                            .join("context/roles")
                            .join(format!("{}.md", request.step)),
                    )
                    .context("read frozen role guidance")?;
                    ensure!(
                        intake::hash(&role) == *expected,
                        "role snapshot hash mismatch: {}",
                        request.step
                    );
                    task.push_str(&format!("\n## Role guidance\n{role}\n"));
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && !self.dir.join("context/roles").exists() => {}
            Err(error) => return Err(error).context("read role manifest"),
        }
        if let Ok(issue) = read_text(&self.dir.join("context/issue.md")) {
            task.push_str(&format!("\n## Ticket context\n{issue}\n"));
        }
        for input in &step.inputs {
            let body = if input == "block_reason" {
                self.observations
                    .borrow()
                    .block_reason
                    .clone()
                    .or_else(|| self.record.run.block.as_ref().map(|b| b.reason.clone()))
                    .or_else(|| {
                        self.record
                            .run
                            .events
                            .iter()
                            .rev()
                            .find(|e| e.kind == "block")
                            .map(|e| e.reason.clone())
                    })
                    .unwrap_or_default()
            } else if let Some(reference) = request.freezes.get(input) {
                reference.clone()
            } else if input == "merge" {
                read_text(&self.dir.join("context/merge.json"))
                    .context("missing verified merge receipt")?
            } else if input.starts_with("diff:") {
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
                            && (self.observations.borrow().completed.contains_key(*name)
                                || self
                                    .record
                                    .run
                                    .events
                                    .iter()
                                    .any(|e| e.step == **name && e.kind == "step_done"))
                    })
                    .map(|(name, _)| name)
                    .with_context(|| format!("missing accepted input {input}"))?;
                let directory =
                    producer_dir(self.dir, self.record, &self.observations.borrow(), producer)?;
                read_text(&artifact(&directory, input)?)?
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
        self.prepare(request).map_err(|error| {
            eprintln!(
                "hermon hub: {}: task preparation: {error:#}",
                request.run_id
            );
            capability(error)
        })?;
        self.process.start(request)
    }
    fn resume_after_answer(
        &mut self,
        previous: &str,
        request: &StartRequest,
        answer: &str,
    ) -> std::result::Result<String, Block> {
        self.prepare(request).map_err(|error| {
            eprintln!(
                "hermon hub: {}: task preparation: {error:#}",
                request.run_id
            );
            capability(error)
        })?;
        self.process.resume_after_answer(previous, request, answer)
    }
    fn poll(&mut self, handle: &str) -> std::result::Result<Poll, Block> {
        let poll = self.process.poll(handle)?;
        if matches!(
            poll,
            Poll::Finished {
                process_success: true,
                completion: true,
                ..
            }
        ) && let Some(attempt) = self
            .record
            .run
            .active
            .as_ref()
            .filter(|a| a.handle == handle)
        {
            let directory =
                attempt_dir(self.dir, &attempt.step, attempt.number).map_err(capability)?;
            let mut observations = self.observations.borrow_mut();
            observations
                .completed
                .insert(attempt.step.clone(), directory);
            if let Poll::Finished {
                result: Some(ref body),
                ..
            } = poll
            {
                observations.block_reason = serde_json::from_str::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| v["block"]["reason"].as_str().map(str::to_owned));
            }
        }
        Ok(poll)
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
        let attempt = self
            .record
            .run
            .active
            .as_ref()
            .filter(|attempt| attempt.step == step)
            .context("missing checked attempt identity")
            .map_err(capability)?;
        let dir = attempt_dir(self.dir, step, attempt.number).map_err(capability)?;
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
                            report: c
                                .report
                                .clone()
                                .or_else(|| (name == "test").then_some(TestReportFormat::Libtest)),
                        },
                    )
                })
                .collect(),
        })?;
        match fs::read_to_string(dir.join("base_commit")) {
            Ok(base) => git.set_path_base(base),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(capability(error)),
        }
        // Trusted expected new test IDs are supplied separately from agent output.
        let mut context = AttemptContext {
            number,
            bot,
            ..AttemptContext::default()
        };
        let producer = workflow.steps.iter().find(|(_, spec)| {
            spec.produces
                .iter()
                .any(|name| name == "test-expectations.json")
        });
        let expected = if let Some((producer, _)) = producer {
            // Planner expectations are independent of the test worker. Never
            // replace them with a newer unaccepted directory or worker result.
            if checks
                .iter()
                .any(|check| matches!(check.as_str(), "tests_fail" | "tests_pass"))
            {
                artifact(
                    &accepted(self.dir, self.record, producer).map_err(capability)?,
                    "test-expectations.json",
                )
                .map_err(capability)?
            } else {
                self.dir.join("context/test-expectations.json")
            }
        } else {
            self.dir.join("context/test-expectations.json")
        };
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
    observations: Rc<RefCell<Observations>>,
}
impl HostCodeHost<'_> {
    fn get(&mut self) -> std::result::Result<&mut GithubCodeHost, Block> {
        if self.host.is_none() {
            let plan = producer_dir(self.dir, self.record, &self.observations.borrow(), "plan")
                .and_then(|dir| read_text(&artifact(&dir, "plan.md")?))
                .unwrap_or_else(|_| self.record.ticket.clone());
            let review = producer_dir(self.dir, self.record, &self.observations.borrow(), "review")
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
        let host = self.get()?;
        if !host.merged(pr)? {
            return Ok(false);
        }
        let commit = host
            .merge_commit()
            .context("missing verified merge SHA")
            .map_err(capability)?
            .to_owned();
        let directory = self.dir.join("context");
        fs::create_dir_all(&directory).map_err(capability)?;
        let mut file = tempfile::NamedTempFile::new_in(&directory).map_err(capability)?;
        serde_json::to_writer(
            &mut file,
            &serde_json::json!({"pr":pr,"merge_commit":commit}),
        )
        .map_err(capability)?;
        file.as_file().sync_all().map_err(capability)?;
        file.persist(directory.join("merge.json"))
            .map_err(capability)?;
        Ok(true)
    }
}

struct HostTracker<'a> {
    inner: &'a mut LinearTracker,
    dir: &'a Path,
    record: &'a RunRecord,
    workflow: &'a Workflow,
    observations: Rc<RefCell<Observations>>,
}
impl hub::Tracker for HostTracker<'_> {
    fn gate_approved(
        &mut self,
        run: &str,
        step: &str,
        request: u32,
    ) -> std::result::Result<bool, Block> {
        let mut published = String::new();
        let observations = self.observations.borrow();
        for (producer, spec) in &self.workflow.steps {
            if spec.publish.is_empty()
                || !(observations.completed.contains_key(producer)
                    || self
                        .record
                        .run
                        .events
                        .iter()
                        .any(|e| e.step == *producer && e.kind == "step_done"))
            {
                continue;
            }
            let directory =
                producer_dir(self.dir, self.record, &observations, producer).map_err(capability)?;
            for name in &spec.publish {
                let path = artifact(&directory, name).map_err(capability)?;
                let body = read_text(&path).map_err(capability)?;
                published.push_str(&format!("{name}\n{body}\n\n"));
                if published.len() > 32 * 1024 {
                    return Err(capability(
                        "published plan exceeds 32 KiB; prepare a concise complete plan before requesting GO",
                    ));
                }
            }
        }
        self.inner
            .gate_with_artifacts(run, step, request, &published)
    }
    fn question(
        &mut self,
        run: &str,
        step: &str,
        request: u32,
        bot: &str,
        questions: &[String],
    ) -> std::result::Result<(), Block> {
        hub::Tracker::question(self.inner, run, step, request, bot, questions)
    }
    fn answer(
        &mut self,
        run: &str,
        step: &str,
        request: u32,
    ) -> std::result::Result<Option<String>, Block> {
        hub::Tracker::answer(self.inner, run, step, request)
    }
    fn alert(&mut self, run: &str, alert: &hub::Alert) -> Result<()> {
        hub::Tracker::alert(self.inner, run, alert)
    }
}
fn publish_outcome(tracker: &mut LinearTracker, dir: &Path, record: &RunRecord) -> Result<()> {
    let summary = record
        .run
        .events
        .iter()
        .rev()
        .find(|e| e.kind == "step_done")
        .map(|e| e.reason.as_str())
        .unwrap_or("Workflow completed");
    let docs = record
        .run
        .events
        .iter()
        .rev()
        .find(|e| e.step == "docs" && e.kind == "step_done")
        .map(|e| e.reason.as_str())
        .unwrap_or("");
    let merge = match fs::read(dir.join("context/merge.json")) {
        Ok(body) => serde_json::from_slice::<serde_json::Value>(&body)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Value::Null,
        Err(error) => return Err(error.into()),
    };
    let mut values = crate::tracker::context_values(
        &record.run.current,
        "",
        "All configured steps completed",
        "",
    );
    for (key, value) in [
        ("workflow", record.workflow_name.clone()),
        ("summary", summary.to_owned()),
        ("docs", docs.to_owned()),
        ("pr", record.run.pr.clone().unwrap_or_default()),
        (
            "merge_commit",
            merge["merge_commit"].as_str().unwrap_or("").to_owned(),
        ),
        ("run_status_cmd", format!("hermon status {}", record.run.id)),
    ] {
        values.insert(key.into(), value);
    }
    tracker.post_comment("outcome.md", &record.run.id, "outcome", &values)?;
    Ok(())
}
