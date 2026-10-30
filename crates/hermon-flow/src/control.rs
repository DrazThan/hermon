//! Locked CLI controls and replayable, ordered run events for the future host.
use crate::{
    agent::{ProcessAgent, lock},
    hub::{self, Block, BlockKind, Event, Run, Settlement, Status},
    intake::{self, Project, identifier},
    store::RunStore,
    templates::Templates,
    workflow::Workflow,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notice {
    pub sequence: usize,
    pub step: String,
    pub text: String,
    pub delivered: bool,
}
/// Schema one is also the JSON status record. No command environment or tokens
/// are serialized here. Project points to the host's external configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub schema: u32,
    pub ticket: String,
    pub workflow_name: String,
    pub workflow_hash: String,
    pub workflow_source: String,
    pub template_hashes: BTreeMap<String, String>,
    pub templates: BTreeMap<String, String>,
    pub project: PathBuf,
    pub worktree: PathBuf,
    pub base: String,
    pub base_commit: String,
    pub branch: String,
    pub paused: bool,
    pub cancelled: bool,
    pub run: Run,
    #[serde(default)]
    pub notices: Vec<Notice>,
    #[serde(default)]
    pub started: bool,
}
#[derive(Serialize, Deserialize)]
struct JournalEvent {
    schema: u32,
    run_id: String,
    sequence: usize,
    kind: String,
    step: String,
    reason: String,
    /// Write-ahead receipt: recover even if the final run.json rename failed.
    snapshot: RunRecord,
}
fn journal(dir: &Path) -> Result<Vec<JournalEvent>> {
    let bytes = match fs::read(dir.join("events.jsonl")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    // A torn final append is not committed. Remove it while holding the lock;
    // all prior complete events remain immutable and their receipt can replay.
    let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    if end < bytes.len() {
        fs::OpenOptions::new()
            .write(true)
            .open(dir.join("events.jsonl"))?
            .set_len(end as u64)?;
    }
    let mut events = vec![];
    for line in bytes[..end]
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
    {
        let event: JournalEvent =
            serde_json::from_slice(line).context("invalid committed run event")?;
        ensure!(
            event.schema == 1 && event.sequence == events.len() + 1,
            "invalid run event sequence/schema"
        );
        events.push(event);
    }
    Ok(events)
}
/// Caller holds the per-run lock (or the intake staging directory).
pub fn save(dir: &Path, record: &RunRecord) -> Result<()> {
    let store = RunStore::new(dir)?;
    let previous = journal(dir)?;
    ensure!(
        previous.len() <= record.run.events.len(),
        "snapshot would discard committed events"
    );
    for (index, event) in record.run.events.iter().enumerate().skip(previous.len()) {
        store.append_event(&JournalEvent {
            schema: 1,
            run_id: record.run.id.clone(),
            sequence: index + 1,
            kind: event.kind.clone(),
            step: event.step.clone(),
            reason: event.reason.clone(),
            snapshot: record.clone(),
        })?;
    }
    store.write_run(record)
}
fn read(dir: &Path, id: &str) -> Result<RunRecord> {
    let store = RunStore::new(dir)?;
    let mut record: RunRecord = store.read_run()?.context("missing run snapshot")?;
    let events = journal(dir)?;
    let mut recovered = false;
    if let Some(last) = events.last() {
        ensure!(last.run_id == id, "event belongs to another run");
        if last.snapshot.run.events.len() > record.run.events.len() {
            record = last.snapshot.clone();
            recovered = true;
        }
    }
    ensure!(
        record.schema == 1 && record.run.id == id,
        "run schema/identity mismatch"
    );
    ensure!(
        intake::hash(&record.workflow_source) == record.workflow_hash,
        "workflow snapshot hash mismatch"
    );
    for (name, body) in &record.templates {
        ensure!(
            record.template_hashes.get(name) == Some(&intake::hash(body)),
            "template snapshot hash mismatch"
        );
    }
    if recovered || events.len() != record.run.events.len() {
        save(dir, &record)?;
    }
    Ok(record)
}
fn directory(data: &Path, id: &str) -> Result<PathBuf> {
    identifier(id)?;
    let dir = data.join("runs").join(id);
    ensure!(
        dir.is_dir() && dir.join("run.json").is_file(),
        "unknown run {id}"
    );
    let root = fs::canonicalize(data.join("runs"))?;
    let dir = fs::canonicalize(dir)?;
    ensure!(
        dir.starts_with(&root) && dir != root,
        "run directory outside data directory"
    );
    Ok(dir)
}
#[derive(Debug)]
pub enum Action {
    Answer(String),
    Approve,
    Retry(String),
    Override { step: String, reason: String },
    Pause,
    Resume,
    Cancel,
}
fn event(record: &mut RunRecord, kind: &str, reason: &str) {
    record.run.events.push(Event {
        kind: kind.into(),
        step: record.run.current.clone(),
        reason: reason.into(),
    });
}
fn transition(record: &mut RunRecord, next: Option<&str>) {
    record.run.retries.remove(&record.run.current);
    record.run.check_failures.remove(&record.run.current);
    record.run.block = None;
    record.run.owner_question = None;
    if let Some(next) = next {
        record.run.current = next.into();
        record.run.status = Status::Ready;
    } else {
        record.run.status = Status::Done;
    }
}
/// Each control is validated under the same lock used by the hosting loop.
/// Persist cancellation before requesting a stop from the owned supervisor.
pub fn apply(data: &Path, id: &str, action: Action) -> Result<RunRecord> {
    let dir = directory(data, id)?;
    let _lock = lock(&dir.join("run.lock"), false)?.context("run lock")?;
    let mut record = read(&dir, id)?;
    ensure!(
        !record.cancelled && !record.run.cancelled,
        "run is cancelled"
    );
    ensure!(record.run.status != Status::Done, "run is already done");
    let workflow = Workflow::from_str("intake workflow snapshot", &record.workflow_source)?;
    let step = workflow
        .steps
        .get(&record.run.current)
        .context("unknown current step")?;
    let mut notice = None;
    match action {
        Action::Pause => {
            ensure!(!record.paused, "run is already paused");
            record.paused = true;
            record.run.paused = true;
            event(&mut record, "pause", "Owner paused dispatch in Hermon");
        }
        Action::Resume => {
            ensure!(record.paused, "run is not paused");
            record.paused = false;
            record.run.paused = false;
            event(&mut record, "resume", "Owner resumed dispatch in Hermon");
        }
        Action::Cancel => {
            record.cancelled = true;
            record.run.cancelled = true;
            record.run.block = Some(Block::new(
                BlockKind::Capability,
                "Owner cancelled run in Hermon",
            ));
            record.run.status = Status::Blocked;
            record.run.unsettled = record.run.active.is_some();
            event(&mut record, "cancel", "Owner cancelled run in Hermon");
        }
        Action::Retry(target) => {
            ensure!(
                target == record.run.current,
                "retry step must match the current blocked step"
            );
            hub::retry(&mut record.run, "Owner requested retry in Hermon")?;
        }
        Action::Override {
            step: target,
            reason,
        } => {
            ensure!(
                target == record.run.current,
                "override step must match the current blocked step"
            );
            hub::override_step(&mut record.run, &workflow, &reason)?;
        }
        Action::Approve => {
            ensure!(
                !record.run.unsettled && record.run.active.is_none(),
                "worker settlement required before approval"
            );
            ensure!(
                record.run.status == Status::Gate && step.gate.as_deref() == Some("owner_go"),
                "approve requires a waiting owner_go gate; owner_merge requires verified merge ancestry"
            );
            let request = record
                .run
                .gate_requests
                .get(&record.run.current)
                .copied()
                .unwrap_or(0);
            ensure!(request > 0, "approval requires an open gate request");
            let text = format!(
                "Owner GO entered in Hermon for {} request {request}",
                record.run.current
            );
            event(&mut record, "approve", &text);
            notice = Some((record.run.current.clone(), text));
            transition(&mut record, step.next.as_deref());
        }
        Action::Answer(text) => {
            ensure!(!text.trim().is_empty(), "answer text is required");
            ensure!(
                record.run.status == Status::Blocked
                    && !record.run.unsettled
                    && record.run.active.is_none()
                    && record
                        .run
                        .block
                        .as_ref()
                        .is_some_and(|b| b.kind == BlockKind::OwnerQuestion),
                "answer requires a settled owner question"
            );
            let question = record
                .run
                .owner_question
                .as_mut()
                .filter(|q| q.step == record.run.current && q.request > 0 && q.answer.is_none())
                .context("no current unanswered owner question")?;
            question.answer = Some(text.clone());
            record.run.status = Status::Ready;
            record.run.block = None;
            notice = Some((
                record.run.current.clone(),
                format!("Answer entered in Hermon: {text}"),
            ));
            event(&mut record, "answer", &text);
        }
    }
    if let Some((step, text)) = notice {
        record.notices.push(Notice {
            sequence: record.run.events.len(),
            step,
            text,
            delivered: false,
        });
    }
    save(&dir, &record)?;
    if record.cancelled && record.run.active.is_some() {
        request_stop(data, &dir, &mut record)
            .context("cancellation recorded; worker settlement remains pending")?;
        save(&dir, &record)?;
    }
    // A retry can recover the intake's durable started outbox after an outage.
    if !record.started && !record.cancelled {
        intake::announce(&dir, &mut record)
            .context("control recorded; started notification remains pending")?;
        save(&dir, &record)?;
    }
    flush_notices(&dir, &mut record).context("control recorded; tracker mirror remains pending")?;
    Ok(record)
}
fn request_stop(data: &Path, dir: &Path, record: &mut RunRecord) -> Result<()> {
    use crate::hub::Agent;
    let attempt = record
        .run
        .active
        .as_ref()
        .context("missing cancelled attempt")?;
    identifier(&attempt.step)?;
    let expected = dir
        .join("steps")
        .join(&attempt.step)
        .join(attempt.number.to_string());
    ensure!(
        fs::canonicalize(&attempt.handle)? == fs::canonicalize(&expected)?,
        "cancel handle does not belong to this run attempt"
    );
    let mut agent = ProcessAgent::new(
        data.join("runs"),
        &record.worktree,
        std::env::current_exe()?,
        BTreeMap::new(),
        Duration::from_secs(1),
    )?;
    let settlement = agent
        .stop(&attempt.handle)
        .map_err(|b| anyhow::anyhow!("{}", b.reason))?;
    if settlement == Settlement::Confirmed {
        record.run.active = None;
        record.run.unsettled = false;
    }
    Ok(())
}
/// Retry the central-app outbox from a host, without replaying the local action.
pub fn flush_notices(dir: &Path, record: &mut RunRecord) -> Result<()> {
    if record.notices.iter().all(|n| n.delivered) {
        return Ok(());
    }
    let project = Project::load(&record.project)?;
    let templates = Templates::load(None, Some(&dir.join("context/templates")))?;
    let mut tracker = project.tracker(dir.join("tracker"), &record.ticket, templates)?;
    for index in 0..record.notices.len() {
        let notice = &record.notices[index];
        if notice.delivered {
            continue;
        }
        let values = BTreeMap::from([
            ("bot".into(), String::new()),
            ("step".into(), notice.step.clone()),
            ("answer".into(), notice.text.clone()),
            ("technical".into(), notice.text.clone()),
            (
                "run_status_cmd".into(),
                format!("hermon status {}", record.run.id),
            ),
        ]);
        tracker.post_comment(
            "answer_ack.md",
            &record.run.id,
            &format!("cli:{}", notice.sequence),
            &values,
        )?;
        record.notices[index].delivered = true;
        save(dir, record)?;
    }
    Ok(())
}
/// Status is one consistent, versioned envelope for both one run and all runs.
pub fn status(data: &Path, id: Option<&str>) -> Result<Vec<RunRecord>> {
    let ids = if let Some(id) = id {
        vec![id.to_owned()]
    } else {
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
        ids
    };
    ids.iter()
        .map(|id| {
            let dir = directory(data, id)?;
            let _lock = lock(&dir.join("run.lock"), false)?.context("run lock")?;
            read(&dir, id)
        })
        .collect()
}
