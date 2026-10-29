//! Deterministic workflow hub; adapters own all external effects.
use crate::workflow::{Step, Workflow};
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    OwnerQuestion,
    TestDefect,
    Capability,
    Transient,
    ScopeChange,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    pub kind: BlockKind,
    pub reason: String,
}
impl Block {
    pub fn new(kind: BlockKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            reason: reason.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Ready,
    Running,
    Gate,
    Blocked,
    Done,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub kind: String,
    pub step: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    pub step: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub step: String,
    pub number: u32,
    pub handle: String,
    /// Seconds on the injected clock's timeline.
    pub started_at: u64,
    pub family: String,
    pub freezes: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub current: String,
    pub status: Status,
    pub active: Option<Attempt>,
    pub attempts: Vec<Attempt>,
    pub events: Vec<Event>,
    pub alerts: Vec<Alert>,
    pub block: Option<Block>,
    pub freezes: BTreeMap<String, String>,
    pub pr: Option<String>,
    pub runs: BTreeMap<String, u32>,
    pub retries: BTreeMap<String, u32>,
    pub check_failures: BTreeMap<String, u32>,
    pub gate_requests: BTreeMap<String, u32>,
    pub unsettled: bool,
    /// Revisions belong to a tag, including tags shared by red and red_fix.
    #[serde(default)]
    pub evidence_revisions: BTreeMap<String, u32>,
    #[serde(default)]
    pub alert_keys: BTreeSet<String>,
}
impl Run {
    pub fn new(id: impl Into<String>, start: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            current: start.into(),
            status: Status::Ready,
            active: None,
            attempts: vec![],
            events: vec![],
            alerts: vec![],
            block: None,
            freezes: BTreeMap::new(),
            pr: None,
            runs: BTreeMap::new(),
            retries: BTreeMap::new(),
            check_failures: BTreeMap::new(),
            gate_requests: BTreeMap::new(),
            unsettled: false,
            evidence_revisions: BTreeMap::new(),
            alert_keys: BTreeSet::new(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct StartRequest {
    pub run_id: String,
    pub step: String,
    pub bot: String,
    pub number: u32,
    pub freezes: BTreeMap<String, String>,
}
#[derive(Debug, Clone)]
pub enum Poll {
    Running,
    Finished {
        process_success: bool,
        completion: bool,
        result: Option<String>,
    },
    /// The worker may still be doing work despite a dead viewer/process.
    Unsettled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    Confirmed,
    Unsettled,
}
pub trait Agent {
    fn family(&self, bot: &str) -> String;
    /// The host adapter accounts for this bot's attempts across all runs.
    fn available(&self, bot: &str) -> bool;
    /// Bind starts idempotently to (run_id, step, number). An error guarantees
    /// no worker was launched; an uncertain launch must return a pollable handle.
    fn start(&mut self, request: &StartRequest) -> std::result::Result<String, Block>;
    /// Resume an answered, settled attempt; adapters without sessions start fresh.
    fn resume_after_answer(
        &mut self,
        _previous_handle: &str,
        request: &StartRequest,
        _answer: &str,
    ) -> std::result::Result<String, Block> {
        self.start(request)
    }
    /// Finished confirms worker settlement, even when its result is invalid.
    /// Observation errors cannot establish settlement and block resend.
    fn poll(&mut self, handle: &str) -> std::result::Result<Poll, Block>;
    fn stop(&mut self, handle: &str) -> std::result::Result<Settlement, Block>;
}
pub trait Tracker {
    fn question(
        &mut self,
        _run_id: &str,
        _step: &str,
        _request: u32,
        _bot: &str,
        _questions: &[String],
    ) -> std::result::Result<(), Block> {
        Ok(())
    }
    fn answer(
        &mut self,
        _run_id: &str,
        _step: &str,
        _request: u32,
    ) -> std::result::Result<Option<String>, Block> {
        Ok(None)
    }

    /// True only for an exact authorized owner reply to this gate request.
    fn gate_approved(
        &mut self,
        run_id: &str,
        step: &str,
        request: u32,
    ) -> std::result::Result<bool, Block>;
    /// Deliver through the host's durable outbox. An error may be retried.
    fn alert(&mut self, run_id: &str, alert: &Alert) -> Result<()>;
}
pub trait CodeHost {
    /// Return the existing PR for this run when the request is repeated.
    fn open_pr(&mut self, run_id: &str) -> std::result::Result<String, Block>;
    /// True only when merged and the merge commit is an ancestor of the base.
    fn merged(&mut self, pr: &str) -> std::result::Result<bool, Block>;
}
pub trait Git {
    fn check(
        &mut self,
        step: &str,
        checks: &[String],
        freezes: &BTreeMap<String, String>,
    ) -> std::result::Result<(), Block>;
    /// Bound to one run; the tag and revision are an idempotency key.
    fn evidence(
        &mut self,
        step: &str,
        tag: &str,
        revision: u32,
    ) -> std::result::Result<String, Block>;
}
pub trait Clock {
    fn now(&self) -> u64;
}
pub struct Dependencies<'a> {
    pub agent: &'a mut dyn Agent,
    pub tracker: &'a mut dyn Tracker,
    pub code_host: &'a mut dyn CodeHost,
    pub git: &'a mut dyn Git,
    pub clock: &'a dyn Clock,
}
/// Reconcile one observation, then follow automatic routes up to the next wait.
/// The host owns per-run locking and durable snapshots. No wall clock, process,
/// filesystem or network calls are made except through the supplied interfaces.
pub fn advance(run: &mut Run, workflow: &Workflow, deps: &mut Dependencies<'_>) -> Result<()> {
    // A very large configured retry limit must not monopolize the host loop.
    // Ready remains an automatic route if this cooperative budget is exhausted.
    let budget = workflow.steps.len().saturating_mul(4).max(16);
    for _ in 0..budget {
        let step = workflow
            .steps
            .get(&run.current)
            .ok_or_else(|| anyhow!("unknown run step {:?}", run.current))?;
        match run.status {
            Status::Done => return Ok(()),
            Status::Blocked => {
                if run.unsettled {
                    let attempt = run
                        .active
                        .as_ref()
                        .ok_or_else(|| anyhow!("unsettled run has no worker handle"))?;
                    // A late result confirms settlement only. It cannot erase a
                    // prior timeout or turn an unknown outcome into acceptance.
                    if matches!(deps.agent.poll(&attempt.handle), Ok(Poll::Finished { .. })) {
                        run.active = None;
                        run.unsettled = false;
                        if let Some(block) = &mut run.block {
                            block.reason =
                                "worker settlement confirmed; owner retry required".into();
                        }
                        event(
                            run,
                            "settled",
                            "worker settlement confirmed; owner retry required",
                        );
                    }
                }
                let reason = run
                    .block
                    .as_ref()
                    .map(|b| b.reason.clone())
                    .unwrap_or_else(|| "owner action required".into());
                notify(run, deps.tracker, &reason)?;
                return Ok(());
            }
            Status::Running => {
                let attempt = run
                    .active
                    .clone()
                    .ok_or_else(|| anyhow!("running run has no active attempt"))?;
                let age = deps.clock.now().saturating_sub(attempt.started_at);
                // Record deadline state before signalling. A zero exit/result
                // observed after the deadline is never success evidence.
                if age >= u64::from(workflow.limits.attempt_timeout_minutes) * 60 {
                    event(run, "timeout", "attempt deadline reached");
                    run.unsettled = true;
                    run.status = Status::Blocked;
                    run.block = Some(Block::new(
                        BlockKind::Transient,
                        "attempt timed out; worker settlement required",
                    ));
                    match deps.agent.stop(&attempt.handle) {
                        Ok(Settlement::Confirmed) => {
                            run.unsettled = false;
                            run.active = None;
                            handle_block(
                                run,
                                workflow,
                                step,
                                Block::new(BlockKind::Transient, "attempt timed out"),
                            );
                        }
                        Ok(Settlement::Unsettled) => {
                            notify(
                                run,
                                deps.tracker,
                                "attempt timed out; worker settlement required",
                            )?;
                        }
                        Err(block) => {
                            run.block = Some(block);
                            let reason = run.block.as_ref().unwrap().reason.clone();
                            notify(run, deps.tracker, &reason)?;
                        }
                    }
                    continue;
                }
                match deps.agent.poll(&attempt.handle) {
                    Ok(Poll::Running) => {
                        if age >= u64::from(workflow.limits.stall_alert_minutes) * 60 {
                            notify(
                                run,
                                deps.tracker,
                                "attempt stalled; worker is still running",
                            )?;
                        }
                        return Ok(());
                    }
                    Ok(Poll::Unsettled) | Err(_) => {
                        run.unsettled = true;
                        run.status = Status::Blocked;
                        run.block = Some(Block::new(
                            BlockKind::Unknown,
                            "worker outcome unresolved; settlement required",
                        ));
                        event(
                            run,
                            "unsettled",
                            "worker outcome unresolved; settlement required",
                        );
                        continue;
                    }
                    Ok(Poll::Finished {
                        process_success,
                        completion,
                        result,
                    }) => {
                        run.active = None;
                        if !process_success {
                            handle_block(
                                run,
                                workflow,
                                step,
                                Block::new(BlockKind::Transient, "agent process failed"),
                            );
                            continue;
                        }
                        let result = if completion {
                            parse_result(result.as_deref(), step)
                        } else {
                            Err(Block::new(
                                BlockKind::Unknown,
                                "agent did not report successful completion",
                            ))
                        };
                        match result {
                            Err(block) => handle_block(run, workflow, step, block),
                            Ok(result) => {
                                if let Some(block) = result.block {
                                    handle_block(run, workflow, step, block);
                                    continue;
                                }
                                if let Err(block) =
                                    deps.git.check(&run.current, &step.check, &attempt.freezes)
                                {
                                    if block.kind == BlockKind::Transient {
                                        handle_block(run, workflow, step, block);
                                        continue;
                                    }
                                    // Cap semantic check retries independently of
                                    // transport retries and workflow loop limits.
                                    let failures =
                                        run.check_failures.entry(run.current.clone()).or_default();
                                    *failures += 1;
                                    if *failures == 1 && block.kind == BlockKind::Unknown {
                                        run.status = Status::Ready;
                                        run.block = Some(block);
                                        event(
                                            run,
                                            "check_retry",
                                            "step checks failed; retrying once",
                                        );
                                    } else {
                                        stop_on_block(run, block);
                                    }
                                    continue;
                                }
                                if let Some(tag) = &step.tag {
                                    let revision =
                                        run.evidence_revisions.get(tag).copied().unwrap_or(0) + 1;
                                    match deps.git.evidence(&run.current, tag, revision) {
                                        Ok(reference) => {
                                            run.evidence_revisions.insert(tag.clone(), revision);
                                            run.freezes.insert(tag.clone(), reference.clone());
                                            event(run, "evidence", &reference);
                                        }
                                        Err(block) => {
                                            handle_block(run, workflow, step, block);
                                            continue;
                                        }
                                    }
                                }
                                event(run, "step_done", &result.summary);
                                if result.verdict.as_deref() == Some("changes_needed")
                                    && step
                                        .max_rounds
                                        .is_some_and(|limit| run.runs[&run.current] >= limit)
                                {
                                    stop_on_block(
                                        run,
                                        Block::new(
                                            BlockKind::Capability,
                                            "review max_rounds reached; owner action required",
                                        ),
                                    );
                                } else {
                                    let next = result
                                        .verdict
                                        .as_ref()
                                        .and_then(|v| step.on_verdict.get(v))
                                        .or(step.next.as_ref());
                                    transition(run, next);
                                }
                            }
                        }
                    }
                }
            }
            Status::Gate => {
                let approved = match step.gate.as_deref() {
                    Some("owner_go") => deps.tracker.gate_approved(
                        &run.id,
                        &run.current,
                        run.gate_requests[&run.current],
                    ),
                    Some("owner_merge") => match &run.pr {
                        Some(pr) => deps.code_host.merged(pr),
                        None => Err(Block::new(
                            BlockKind::Unknown,
                            "merge gate has no pull request",
                        )),
                    },
                    _ => Err(Block::new(BlockKind::Unknown, "unknown gate")),
                };
                match approved {
                    Ok(true) => {
                        event(run, "gate_passed", step.gate.as_deref().unwrap_or("gate"));
                        transition(run, step.next.as_ref());
                    }
                    Ok(false) => {
                        notify(run, deps.tracker, "owner gate is waiting for approval")?;
                        return Ok(());
                    }
                    Err(block) => handle_block(run, workflow, step, block),
                }
            }
            Status::Ready => {
                ensure!(
                    run.active.is_none() && !run.unsettled,
                    "cannot start while a worker is unresolved"
                );
                if step.gate.is_some() {
                    *run.gate_requests.entry(run.current.clone()).or_default() += 1;
                    run.status = Status::Gate;
                    event(run, "gate_opened", step.gate.as_deref().unwrap());
                    continue;
                }
                let executions = run.runs.get(&run.current).copied().unwrap_or(0);
                if step.max_runs.is_some_and(|limit| executions >= limit)
                    || step.max_rounds.is_some_and(|limit| executions >= limit)
                {
                    stop_on_block(
                        run,
                        Block::new(
                            BlockKind::Capability,
                            "step execution limit reached; owner action required",
                        ),
                    );
                    continue;
                }
                if step.action.as_deref() == Some("open_pr") {
                    run.runs.insert(run.current.clone(), executions + 1);
                    match deps.code_host.open_pr(&run.id) {
                        Ok(pr) if !pr.trim().is_empty() => {
                            run.pr = Some(pr);
                            event(run, "step_done", "pull request opened");
                            transition(run, step.next.as_ref());
                        }
                        Ok(_) => stop_on_block(
                            run,
                            Block::new(BlockKind::Unknown, "code host returned no pull request"),
                        ),
                        Err(block) => handle_block(run, workflow, step, block),
                    }
                    continue;
                }
                let bot = step
                    .bot
                    .as_deref()
                    .ok_or_else(|| anyhow!("step has no executable kind"))?;
                let family = deps.agent.family(bot);
                if let Some(other) = &step.require_different_family {
                    let prior = run
                        .attempts
                        .iter()
                        .rev()
                        .find(|attempt| &attempt.step == other);
                    if family.trim().is_empty()
                        || prior.is_none_or(|a| a.family.trim().is_empty() || a.family == family)
                    {
                        stop_on_block(
                            run,
                            Block::new(
                                BlockKind::Capability,
                                "step requires a different known agent family",
                            ),
                        );
                        continue;
                    }
                }
                if !deps.agent.available(bot) {
                    notify(run, deps.tracker, "waiting for bot concurrency capacity")?;
                    return Ok(());
                }
                let request = StartRequest {
                    run_id: run.id.clone(),
                    step: run.current.clone(),
                    bot: bot.into(),
                    number: executions + 1,
                    freezes: run.freezes.clone(),
                };
                run.runs.insert(run.current.clone(), request.number);
                match deps.agent.start(&request) {
                    Ok(handle) => {
                        let attempt = Attempt {
                            step: request.step,
                            number: request.number,
                            handle,
                            started_at: deps.clock.now(),
                            family,
                            freezes: request.freezes,
                        };
                        run.attempts.push(attempt.clone());
                        run.active = Some(attempt);
                        run.status = Status::Running;
                        run.block = None;
                        event(run, "started", bot);
                        return Ok(());
                    }
                    Err(block) => handle_block(run, workflow, step, block),
                }
            }
        }
    }
    Ok(())
}

fn event(run: &mut Run, kind: &str, reason: &str) {
    run.events.push(Event {
        kind: kind.into(),
        step: run.current.clone(),
        reason: reason.into(),
    });
}

fn transition(run: &mut Run, next: Option<&String>) {
    // A completed/overridden visit must not consume a future visit's retry
    // allowance, including gates and built-in actions without bot completion.
    run.retries.remove(&run.current);
    run.check_failures.remove(&run.current);
    run.active = None;
    run.block = None;
    if let Some(next) = next {
        event(run, "route", next);
        run.current.clone_from(next);
        run.status = Status::Ready;
    } else {
        run.status = Status::Done;
        event(run, "done", "workflow completed");
    }
}

fn stop_on_block(run: &mut Run, block: Block) {
    event(run, "blocked", &block.reason);
    run.status = Status::Blocked;
    run.block = Some(block);
}

fn handle_block(run: &mut Run, workflow: &Workflow, step: &Step, block: Block) {
    event(run, "block", &block.reason);
    if block.kind == BlockKind::Transient {
        let retries = run.retries.entry(run.current.clone()).or_default();
        if *retries < workflow.limits.transient_retries {
            *retries += 1;
            run.block = Some(block);
            run.status = Status::Ready;
            event(run, "transient_retry", "retrying transient failure");
            return;
        }
    } else {
        let kind = match block.kind {
            BlockKind::OwnerQuestion => "owner_question",
            BlockKind::TestDefect => "test_defect",
            BlockKind::ScopeChange => "scope_change",
            BlockKind::Capability => "capability",
            BlockKind::Unknown => "unknown",
            BlockKind::Transient => unreachable!(),
        };
        if let Some(target) = step.on_block.get(kind) {
            transition(run, Some(target));
            return;
        }
    }
    stop_on_block(run, block);
}

fn notify(run: &mut Run, tracker: &mut dyn Tracker, reason: &str) -> Result<()> {
    // Polling is not an event. This generation stays stable while waiting and
    // changes when a new attempt, block, gate or manual action is recorded.
    let key = format!("{}:{}:{reason}", run.current, run.events.len());
    if !run.alert_keys.contains(&key) {
        let alert = Alert {
            step: run.current.clone(),
            reason: reason.into(),
        };
        tracker.alert(&run.id, &alert)?;
        run.alerts.push(alert);
        run.alert_keys.insert(key);
    }
    Ok(())
}

pub fn retry(run: &mut Run, reason: &str) -> Result<()> {
    ensure!(!reason.trim().is_empty(), "retry requires a reason");
    ensure!(
        run.status == Status::Blocked,
        "only a blocked step can be retried"
    );
    ensure!(
        !run.unsettled && run.active.is_none(),
        "worker settlement required before retry"
    );
    run.retries.remove(&run.current);
    run.check_failures.remove(&run.current);
    run.block = None;
    run.status = Status::Ready;
    event(run, "retry", reason);
    Ok(())
}

pub fn override_step(run: &mut Run, workflow: &Workflow, reason: &str) -> Result<()> {
    ensure!(!reason.trim().is_empty(), "override requires a reason");
    ensure!(
        run.status == Status::Blocked,
        "only a blocked step can be overridden"
    );
    ensure!(
        !run.unsettled && run.active.is_none(),
        "worker settlement required before override"
    );
    let step = workflow
        .steps
        .get(&run.current)
        .ok_or_else(|| anyhow!("unknown step"))?;
    ensure!(step.gate.is_none(), "owner gates cannot be overridden");
    event(run, "override", reason);
    transition(run, step.next.as_ref());
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepResult {
    schema: u32,
    status: String,
    summary: String,
    artifacts: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    block: Option<Block>,
    #[serde(deserialize_with = "nullable")]
    verdict: Option<String>,
    questions: Vec<String>,
}

// The contract allows null, not an omitted key. Serde's plain Option field
// otherwise treats missing fields as null without calling a deserializer.
fn nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn parse_result(text: Option<&str>, step: &Step) -> std::result::Result<StepResult, Block> {
    let validate = || -> Result<StepResult> {
        let text = text.ok_or_else(|| anyhow!("missing result.json"))?;
        let result: StepResult = serde_json::from_str(text)?;
        ensure!(result.schema == 1, "unsupported result schema");
        ensure!(!result.summary.trim().is_empty(), "result summary is empty");
        ensure!(
            matches!(result.status.as_str(), "done" | "block"),
            "unknown result status"
        );
        ensure!(
            (result.status == "block") == result.block.is_some(),
            "contradictory result block/status"
        );
        if let Some(block) = &result.block {
            ensure!(!block.reason.trim().is_empty(), "block reason is empty");
            ensure!(result.verdict.is_none(), "blocked result has a verdict");
        }
        if let Some(verdict) = &result.verdict {
            ensure!(
                step.require_different_family.is_some() || !step.on_verdict.is_empty(),
                "verdict is only allowed on a review step"
            );
            ensure!(
                matches!(verdict.as_str(), "clean" | "changes_needed"),
                "unknown review verdict"
            );
        }
        if result.status == "done"
            && (step.require_different_family.is_some() || !step.on_verdict.is_empty())
        {
            ensure!(result.verdict.is_some(), "review result requires a verdict");
        }
        // Artifacts/questions are typed data for later adapters; neither can
        // manufacture a gate approval or successful check in this core.
        let _ = (&result.artifacts, &result.questions);
        Ok(result)
    };
    validate().map_err(|error| Block::new(BlockKind::Unknown, format!("invalid result: {error:#}")))
}
