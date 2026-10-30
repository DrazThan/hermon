//! Project configuration and doctor-gated, explicitly confirmed CLI intake.
use crate::{
    agent::{BotConfig, lock},
    control::{RunRecord, save},
    doctor,
    hub::{Block, BlockKind, Event, Run, Status},
    templates::Templates,
    tracker::{LinearConfig, LinearTracker},
    workflow::{Workflow, yaml_value},
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

pub const TEMPLATE_NAMES: [&str; 7] = [
    "started.md",
    "question.md",
    "go_request.md",
    "answer_ack.md",
    "step_done.md",
    "alert.md",
    "outcome.md",
];
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub repo: PathBuf,
    pub worktree_root: PathBuf,
    pub branch_prefix: String,
    pub base: String,
    pub start: String,
    pub env: BTreeMap<String, String>,
    pub bots: BTreeMap<String, BotConfig>,
    pub commands: BTreeMap<String, ProjectCommand>,
    #[serde(default)]
    pub paths: BTreeMap<String, Vec<String>>,
    pub tracker: TrackerConfig,
    pub code_host: CodeHostConfig,
    pub templates: Option<PathBuf>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCommand {
    pub argv: Vec<String>,
    pub timeout_seconds: u64,
    pub report: Option<crate::checks::TestReportFormat>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackerConfig {
    pub endpoint: String,
    pub token_env: String,
    pub owner_ids: BTreeSet<String>,
    pub owner_mention: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeHostConfig {
    pub repo: String,
    #[serde(default = "origin")]
    pub remote: String,
}
fn origin() -> String {
    "origin".into()
}
impl Project {
    pub fn load(path: &Path) -> Result<Self> {
        let source = fs::read_to_string(path)
            .with_context(|| format!("{}: read project", path.display()))?;
        let mut project: Self = serde_path_to_error::deserialize(yaml_value(&source)?)
            .with_context(|| format!("{}: invalid project", path.display()))?;
        let parent = fs::canonicalize(path)?
            .parent()
            .context("project directory")?
            .to_owned();
        project.repo =
            fs::canonicalize(parent.join(&project.repo)).context("project.repo must exist")?;
        project.worktree_root = parent.join(&project.worktree_root);
        project.templates = project.templates.map(|p| parent.join(p));
        ensure!(project.repo.is_dir(), "project.repo must be a directory");
        ensure!(
            !project.branch_prefix.trim().is_empty(),
            "branch_prefix is required"
        );
        ensure!(
            !project.tracker.token_env.is_empty(),
            "tracker.token_env is required"
        );
        for (key, value) in &project.env {
            ensure!(
                !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0'),
                "invalid project environment"
            );
        }
        Ok(project)
    }
    pub fn tracker(
        &self,
        state: impl AsRef<Path>,
        ticket: &str,
        templates: Templates,
    ) -> Result<LinearTracker> {
        let token = std::env::var(&self.tracker.token_env).with_context(|| {
            format!(
                "set {} for central tracker authentication",
                self.tracker.token_env
            )
        })?;
        LinearTracker::new(
            LinearConfig {
                endpoint: self.tracker.endpoint.clone(),
                token,
                issue_id: ticket.into(),
                owner_ids: self.tracker.owner_ids.clone(),
                owner_mention: self.tracker.owner_mention.clone(),
            },
            templates,
            state,
        )
    }
}
pub fn templates(project: &Project, workflow: &Workflow, path: &Path) -> Result<Templates> {
    let pack = workflow
        .templates
        .as_ref()
        .map(|p| path.parent().unwrap_or(Path::new(".")).join(p));
    Templates::load(project.templates.as_deref(), pack.as_deref())
}
pub fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
/// Role files are optional for generic packs. Existing entries must be readable,
/// bounded files inside the pack; snapshot them before allocating a run.
fn roles(workflow: &Workflow, path: &Path) -> Result<BTreeMap<String, String>> {
    let root = fs::canonicalize(path)?
        .parent()
        .context("workflow directory")?
        .to_owned();
    let mut roles = BTreeMap::new();
    for (name, step) in &workflow.steps {
        if step.bot.is_none() {
            continue;
        }
        identifier(name)?;
        let path = root.join("roles").join(format!("{name}.md"));
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("read role metadata"),
            Ok(_) => {}
        }
        let target = fs::canonicalize(&path).context("resolve role path")?;
        ensure!(
            target.starts_with(&root),
            "role outside workflow pack: {name}"
        );
        ensure!(
            target.is_file() && fs::metadata(&target)?.len() <= 1024 * 1024,
            "role {name} must be a file of at most 1 MiB"
        );
        let body = fs::read_to_string(target).with_context(|| format!("read role {name}"))?;
        ensure!(!body.trim().is_empty(), "role {name} must not be empty");
        roles.insert(name.clone(), body);
    }
    Ok(roles)
}
pub(crate) fn identifier(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 200
            && id != "."
            && id != ".."
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
        "run ID must be a single alphanumeric path component (dash, dot and underscore allowed)"
    );
    Ok(())
}

pub struct Request<'a> {
    pub workflow: &'a Path,
    pub project: &'a Path,
    pub data_dir: &'a Path,
    pub ticket: &'a str,
    pub run_id: &'a str,
    pub base: Option<&'a str>,
    pub start: Option<&'a str>,
    pub confirmed: bool,
}
/// The caller explicitly confirms the workflow/base; no model can approve intake.
/// Failed doctor checks occur before creating any durable run or git worktree.
pub fn create(request: Request<'_>) -> Result<RunRecord> {
    ensure!(
        request.confirmed,
        "confirm workflow and base with --confirm (use --base/--start to select them)"
    );
    identifier(request.run_id)?;
    ensure!(!request.ticket.trim().is_empty(), "ticket is required");
    let run_dir = request.data_dir.join("runs").join(request.run_id);
    ensure!(!run_dir.exists(), "run {} already exists", request.run_id);
    let project = Project::load(request.project)?;
    let source = fs::read_to_string(request.workflow)?;
    let workflow = Workflow::from_str(&request.workflow.display().to_string(), &source)?;
    let role_bodies = roles(&workflow, request.workflow)?;
    let start = request.start.unwrap_or(&project.start);
    ensure!(
        workflow.steps.contains_key(start),
        "unknown starting step {start}"
    );
    let selected = templates(&project, &workflow, request.workflow)?;
    let base = request.base.unwrap_or(&project.base);
    let branch = format!("{}/{}", project.branch_prefix, request.run_id);
    let temporary = tempfile::tempdir()?;
    let mut tracker = project.tracker(
        temporary.path().join("tracker"),
        request.ticket,
        Templates::load(None, None)?,
    )?;
    let issue = tracker.read_issue().context("read intake ticket")?;
    let mut context = format!("# {}\n\n{}\n", issue.title, issue.description);
    let mut cursor = None;
    loop {
        let page = tracker.list_comments_since(cursor.as_deref())?;
        for comment in page.comments {
            context.push_str(&format!("\n{}: {}\n", comment.user_id, comment.body));
        }
        // The tracker cursor carries a durable watermark even after the last
        // page. Only an outstanding Relay `after` means more pages this poll.
        let more = page
            .next_cursor
            .as_deref()
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()?
            .is_some_and(|c| !c["after"].is_null());
        cursor = page.next_cursor;
        if !more {
            break;
        }
    }
    doctor::check(&project, &workflow, Some(request.ticket))?.require_pass()?;
    for reference in [base, &branch] {
        doctor::execute(
            &project,
            &[
                "git".into(),
                "check-ref-format".into(),
                format!("refs/heads/{reference}"),
            ],
            None,
            15,
        )?;
    }
    // Resolve the owner's base before allocating any worktree. Preserve both
    // selected branch name and exact base commit in the durable receipt.
    let base_commit = doctor::execute(
        &project,
        &[
            "git".into(),
            "rev-parse".into(),
            "--verify".into(),
            format!("refs/heads/{base}^{{commit}}"),
        ],
        None,
        15,
    )?
    .trim()
    .to_owned();
    let template_bodies: BTreeMap<_, _> = TEMPLATE_NAMES
        .iter()
        .map(|n| (n.to_string(), selected.get(n).unwrap().to_owned()))
        .collect();
    let template_hashes = template_bodies
        .iter()
        .map(|(n, b)| (n.clone(), hash(b)))
        .collect();
    fs::create_dir_all(request.data_dir)?;
    let _lock = lock(&request.data_dir.join("intake.lock"), false)?.context("intake lock")?;
    ensure!(!run_dir.exists(), "run {} already exists", request.run_id);
    fs::create_dir_all(&project.worktree_root)?;
    let worktree = fs::canonicalize(&project.worktree_root)?.join(request.run_id);
    let worktree_text = worktree
        .to_str()
        .context("worktree path must be UTF-8")?
        .to_owned();
    doctor::execute(
        &project,
        &[
            "git".into(),
            "worktree".into(),
            "add".into(),
            "-b".into(),
            branch.clone(),
            "--".into(),
            worktree_text.clone(),
            base_commit.clone(),
        ],
        None,
        30,
    )?;
    let mut record = RunRecord {
        schema: 1,
        ticket: request.ticket.into(),
        workflow_name: workflow.name.clone(),
        workflow_hash: hash(&source),
        workflow_source: source,
        template_hashes,
        templates: template_bodies,
        project: fs::canonicalize(request.project)?,
        worktree,
        base: base.into(),
        base_commit,
        branch,
        paused: false,
        cancelled: false,
        run: Run::new(request.run_id, start),
        notices: vec![],
        started: false,
    };
    record.run.events.push(Event {
        kind: "intake".into(),
        step: start.into(),
        reason: format!(
            "Confirmed {} on {} after doctor passed",
            record.workflow_name, record.base
        ),
    });
    let initialize = || -> Result<fs::File> {
        fs::create_dir_all(run_dir.parent().unwrap())?;
        let staging = tempfile::Builder::new()
            .prefix(".intake-")
            .tempdir_in(run_dir.parent().unwrap())?;
        fs::create_dir_all(staging.path().join("context"))?;
        fs::write(staging.path().join("context/issue.md"), context)?;
        fs::write(
            staging.path().join("context/workflow.yaml"),
            &record.workflow_source,
        )?;
        fs::create_dir_all(staging.path().join("context/templates"))?;
        for (name, body) in &record.templates {
            fs::write(staging.path().join("context/templates").join(name), body)?;
        }
        fs::create_dir_all(staging.path().join("context/roles"))?;
        let mut role_hashes = BTreeMap::new();
        for (name, body) in &role_bodies {
            fs::write(
                staging
                    .path()
                    .join("context/roles")
                    .join(format!("{name}.md")),
                body,
            )?;
            role_hashes.insert(name, hash(body));
        }
        fs::write(
            staging.path().join("context/roles.json"),
            serde_json::to_vec(&role_hashes)?,
        )?;
        // Lock the inode before publication, retaining it through started
        // delivery and the final snapshot so controls cannot race intake.
        let run_lock = lock(&staging.path().join("run.lock"), false)?.context("run lock")?;
        save(staging.path(), &record)?;
        fs::rename(staging.path(), &run_dir)?;
        Ok(run_lock)
    };
    let _run_lock = match initialize() {
        Ok(lock) => lock,
        Err(error) => {
            let _ = doctor::execute(
                &project,
                &[
                    "git".into(),
                    "worktree".into(),
                    "remove".into(),
                    "--".into(),
                    worktree_text.clone(),
                ],
                None,
                15,
            );
            let _ = doctor::execute(
                &project,
                &[
                    "git".into(),
                    "branch".into(),
                    "-d".into(),
                    record.branch.clone(),
                ],
                None,
                15,
            );
            return Err(error);
        }
    };
    if let Err(error) = announce(&run_dir, &mut record) {
        record.run.status = Status::Blocked;
        record.run.block = Some(Block::new(
            BlockKind::Capability,
            "started comment pending; restore tracker access and retry intake notification",
        ));
        record.run.events.push(Event {
            kind: "intake_blocked".into(),
            step: start.into(),
            reason: "started comment delivery failed; retry after fixing tracker access".into(),
        });
        save(&run_dir, &record)?;
        return Err(error).context("run created, but started notification is pending");
    }
    save(&run_dir, &record)?;
    Ok(record)
}
/// Idempotent started notification; a host can retry it after connectivity returns.
pub fn announce(dir: &Path, record: &mut RunRecord) -> Result<()> {
    if record.started {
        return Ok(());
    }
    let project = Project::load(&record.project)?;
    let templates = Templates::load(None, Some(&dir.join("context/templates")))?;
    let mut tracker = project.tracker(dir.join("tracker"), &record.ticket, templates)?;
    tracker.post_comment(
        "started.md",
        &record.run.id,
        "started",
        &BTreeMap::from([
            ("workflow".into(), record.workflow_name.clone()),
            ("base_branch".into(), record.base.clone()),
            ("branch".into(), record.branch.clone()),
            (
                "run_status_cmd".into(),
                format!("hermon status {}", record.run.id),
            ),
        ]),
    )?;
    record.started = true;
    Ok(())
}
