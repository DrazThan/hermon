//! Deterministic workflow hub; adapters own all external effects.
use crate::{
    ci::{self, Authorization, Candidate, ReviewReceipt},
    workflow::{Step, Workflow},
};
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
    CiFailed,
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
pub struct OwnerQuestion {
    pub step: String,
    pub request: u32,
    pub bot: String,
    pub questions: Vec<String>,
    pub previous_handle: String,
    pub answer: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub cancelled: bool,
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
    pub owner_question: Option<OwnerQuestion>,
    #[serde(default)]
    pub alert_keys: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<Candidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_receipt: Option<ReviewReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_policy_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<ci::WaitState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub red_proof: Option<crate::checks::RedProof>,
}
impl Run {
    pub fn new(id: impl Into<String>, start: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            paused: false,
            cancelled: false,
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
            owner_question: None,
            alert_keys: BTreeSet::new(),
            candidate: None,
            review_receipt: None,
            ci_policy_digest: None,
            ci: None,
            red_proof: None,
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
    /// Adapters must carry the answer into either a resumed or fresh session.
    /// Refuse by default rather than silently dropping the owner's decision.
    fn resume_after_answer(
        &mut self,
        _previous_handle: &str,
        _request: &StartRequest,
        _answer: &str,
    ) -> std::result::Result<String, Block> {
        Err(Block::new(
            BlockKind::Capability,
            "agent adapter cannot deliver owner-answer context",
        ))
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
    fn open_draft_pr(
        &mut self,
        _run_id: &str,
        _candidate: &Candidate,
    ) -> std::result::Result<String, Block> {
        Err(Block::new(
            BlockKind::Capability,
            "draft publication is unsupported",
        ))
    }
    fn sync_head(&mut self, _pr: &str, _candidate: &Candidate) -> std::result::Result<(), Block> {
        Err(Block::new(
            BlockKind::Capability,
            "candidate synchronization is unsupported",
        ))
    }
    fn ci_policy(&mut self, _digest: &str) -> std::result::Result<ci::Policy, Block> {
        Err(Block::new(
            BlockKind::Capability,
            "pinned CI policy is unavailable",
        ))
    }
    fn ci_status(
        &mut self,
        _pr: &str,
        _candidate: &Candidate,
        _policy: &ci::Policy,
        _digest: &str,
    ) -> std::result::Result<ci::Observation, Block> {
        Err(Block::new(
            BlockKind::Capability,
            "GitHub CI observation is unsupported",
        ))
    }
    fn ready_pr(
        &mut self,
        _authorization: &Authorization,
        _policy: &ci::Policy,
    ) -> std::result::Result<(), Block> {
        Err(Block::new(
            BlockKind::Capability,
            "ready publication is unsupported",
        ))
    }
}
pub trait Git {
    fn red_proof(
        &mut self,
        _reference: &str,
    ) -> std::result::Result<crate::checks::RedProof, Block> {
        Err(Block::new(
            BlockKind::Capability,
            "original RED proof is unsupported",
        ))
    }
    fn review_amendments(
        &mut self,
        _red: &str,
        _green: &str,
    ) -> std::result::Result<crate::checks::Amendments, Block> {
        Err(Block::new(
            BlockKind::Capability,
            "amendment verification is unsupported",
        ))
    }
    fn candidate(&mut self, _reference: &str) -> std::result::Result<String, Block> {
        Err(Block::new(
            BlockKind::Capability,
            "accepted candidate verification is unsupported",
        ))
    }
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
    if run.cancelled {
        // Cancellation is terminal for acceptance, not for supervision. Keep
        // observing the owned handle until the worker confirms settlement.
        if let Some(attempt) = &run.active {
            let settled = matches!(deps.agent.poll(&attempt.handle), Ok(Poll::Finished { .. }))
                || matches!(deps.agent.stop(&attempt.handle), Ok(Settlement::Confirmed));
            // Repeating stop is intentional: the host may have crashed after
            // persisting cancellation, before asking the owned worker to stop.
            if settled {
                run.active = None;
                run.unsettled = false;
                event(run, "settled", "cancelled worker settlement confirmed");
            }
        }
        return Ok(());
    }
    if run.paused {
        return Ok(());
    }
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
                // Any settled worker block that registered questions waits for
                // an owner reply, whatever its kind. Answering only resumes the
                // same step; checks and gates still apply to the new attempt.
                if !run.unsettled
                    && run.block.is_some()
                    && let Some(question) = run
                        .owner_question
                        .clone()
                        .filter(|q| q.step == run.current && q.answer.is_none())
                {
                    deps.tracker
                        .question(
                            &run.id,
                            &question.step,
                            question.request,
                            &question.bot,
                            &question.questions,
                        )
                        .map_err(|block| anyhow!("question delivery failed: {}", block.reason))?;
                    if let Some(answer) = deps
                        .tracker
                        .answer(&run.id, &question.step, question.request)
                        .map_err(|block| anyhow!("answer polling failed: {}", block.reason))?
                    {
                        run.owner_question.as_mut().unwrap().answer = Some(answer.clone());
                        run.status = Status::Ready;
                        run.block = None;
                        event(run, "owner_answer", &answer);
                        continue;
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
                                    // Questions on a block that stops for the
                                    // owner must be answerable. A configured
                                    // route or a new start clears them, and a
                                    // transient block retries instead.
                                    if block.kind == BlockKind::OwnerQuestion
                                        || (block.kind != BlockKind::Transient
                                            && !result.questions.is_empty())
                                    {
                                        run.owner_question = Some(OwnerQuestion {
                                            step: run.current.clone(),
                                            request: attempt.number,
                                            bot: step.bot.clone().unwrap_or_default(),
                                            questions: if result.questions.is_empty() {
                                                vec![block.reason.clone()]
                                            } else {
                                                result.questions
                                            },
                                            previous_handle: attempt.handle.clone(),
                                            answer: None,
                                        });
                                    }
                                    handle_block(run, workflow, step, block);
                                    continue;
                                }
                                let mut accepted_amendments = None;
                                let checked = (|| -> std::result::Result<(), Block> {
                                    deps.git
                                        .check(&run.current, &step.check, &attempt.freezes)?;
                                    if let Some(policy) = &step.review_amendments {
                                        let red =
                                            attempt.freezes.get(&policy.red).ok_or_else(|| {
                                                Block::new(
                                                    BlockKind::Capability,
                                                    "missing review RED",
                                                )
                                            })?;
                                        let green = attempt.freezes.get(&policy.green).ok_or_else(
                                            || {
                                                Block::new(
                                                    BlockKind::Capability,
                                                    "missing review GREEN",
                                                )
                                            },
                                        )?;
                                        let proof = deps.git.review_amendments(red, green)?;
                                        if result.verdict.as_deref() == Some("clean") {
                                            if result
                                                .accepted_amendments
                                                .as_ref()
                                                .is_some_and(|digest| digest != &proof.diff_sha256)
                                                || (!proof.empty
                                                    && result.accepted_amendments.as_ref()
                                                        != Some(&proof.diff_sha256))
                                            {
                                                return Err(Block::new(
                                                    BlockKind::Unknown,
                                                    "clean review requires the exact current amendment digest",
                                                ));
                                            }
                                            accepted_amendments = Some(proof);
                                        }
                                    }
                                    Ok(())
                                })();
                                if let Err(block) = checked {
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
                                    if tag == "red"
                                        && run.candidate.is_some()
                                        && workflow
                                            .steps
                                            .values()
                                            .any(|s| s.review_amendments.is_some())
                                    {
                                        stop_on_block(
                                            run,
                                            Block::new(
                                                BlockKind::Capability,
                                                "cannot replace original RED after GREEN; start a newly planned run",
                                            ),
                                        );
                                        continue;
                                    }
                                    let revision =
                                        run.evidence_revisions.get(tag).copied().unwrap_or(0) + 1;
                                    match deps.git.evidence(&run.current, tag, revision) {
                                        Ok(reference) => {
                                            run.evidence_revisions.insert(tag.clone(), revision);
                                            run.freezes.insert(tag.clone(), reference.clone());
                                            event(run, "evidence", &reference);
                                            if tag == "red"
                                                && workflow.steps.values().any(|s| {
                                                    s.check
                                                        .iter()
                                                        .any(|c| c.starts_with("red_guard:"))
                                                })
                                            {
                                                match deps.git.red_proof(&reference) {
                                                    Ok(proof)
                                                        if run.candidate.is_none()
                                                            || run.red_proof.as_ref()
                                                                == Some(&proof) =>
                                                    {
                                                        run.red_proof = Some(proof)
                                                    }
                                                    Ok(_) => {
                                                        stop_on_block(
                                                            run,
                                                            Block::new(
                                                                BlockKind::Capability,
                                                                "cannot replace RED after GREEN; start a newly planned run",
                                                            ),
                                                        );
                                                        continue;
                                                    }
                                                    Err(block) => {
                                                        handle_block(run, workflow, step, block);
                                                        continue;
                                                    }
                                                }
                                            }
                                            if tag == "green"
                                                && (workflow.lifecycle() || workflow.uses_ci())
                                            {
                                                match deps.git.candidate(&reference) {
                                                    Ok(sha) => {
                                                        if run.candidate.as_ref().is_none_or(|c| {
                                                            c.sha != sha || c.reference != reference
                                                        }) {
                                                            run.review_receipt = None;
                                                            run.ci = None;
                                                        }
                                                        let amendments = if let Some(review) =
                                                            workflow.steps.values().find_map(|s| {
                                                                s.review_amendments.as_ref()
                                                            }) {
                                                            let Some(red) =
                                                                run.freezes.get(&review.red)
                                                            else {
                                                                stop_on_block(
                                                                    run,
                                                                    Block::new(
                                                                        BlockKind::Capability,
                                                                        "missing amendment RED",
                                                                    ),
                                                                );
                                                                continue;
                                                            };
                                                            match deps
                                                                .git
                                                                .review_amendments(red, &reference)
                                                            {
                                                                Ok(receipt) => Some(receipt),
                                                                Err(block) => {
                                                                    handle_block(
                                                                        run, workflow, step, block,
                                                                    );
                                                                    continue;
                                                                }
                                                            }
                                                        } else {
                                                            None
                                                        };
                                                        run.candidate = Some(Candidate {
                                                            amendments,
                                                            sha,
                                                            reference,
                                                            step: run.current.clone(),
                                                            attempt: attempt.number,
                                                            family: attempt.family.clone(),
                                                            red_reference: run
                                                                .freezes
                                                                .get("red")
                                                                .cloned(),
                                                        });
                                                    }
                                                    Err(block) => {
                                                        handle_block(run, workflow, step, block);
                                                        continue;
                                                    }
                                                }
                                            }
                                        }
                                        Err(block) => {
                                            handle_block(run, workflow, step, block);
                                            continue;
                                        }
                                    }
                                }
                                if workflow.lifecycle() && step.require_different_family.is_some() {
                                    run.review_receipt = None;
                                    if result.verdict.as_deref() == Some("clean") {
                                        let proof =
                                            (|| -> std::result::Result<ReviewReceipt, Block> {
                                                let c =
                                                    run.candidate.as_ref().ok_or_else(|| {
                                                        Block::new(
                                                            BlockKind::Capability,
                                                            "review has no accepted candidate",
                                                        )
                                                    })?;
                                                if step.require_different_family.as_ref()
                                                    != Some(&c.step)
                                                    || attempt.family.is_empty()
                                                    || attempt.family == c.family
                                                    || deps.git.candidate(&c.reference)? != c.sha
                                                {
                                                    return Err(Block::new(
                                                        BlockKind::Capability,
                                                        "review candidate or independent family changed",
                                                    ));
                                                }
                                                Ok(ReviewReceipt {
                                                    candidate_sha: c.sha.clone(),
                                                    step: run.current.clone(),
                                                    attempt: attempt.number,
                                                    family: attempt.family.clone(),
                                                    summary: result.summary.clone(),
                                                    amendments: accepted_amendments.clone(),
                                                })
                                            })();
                                        match proof {
                                            Ok(receipt) => run.review_receipt = Some(receipt),
                                            Err(block) => {
                                                handle_block(run, workflow, step, block);
                                                continue;
                                            }
                                        }
                                    }
                                }
                                event(run, "step_done", &result.summary);
                                if result.verdict.as_deref() == Some("changes_needed")
                                    && step
                                        .max_rounds
                                        .is_some_and(|limit| visits(run, &run.current) >= limit)
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
                                    if workflow.lifecycle()
                                        && result.verdict.as_deref() == Some("changes_needed")
                                    {
                                        event(run, "review_changes", &result.summary);
                                    }
                                    transition(run, next);
                                }
                            }
                        }
                    }
                }
            }
            Status::Gate => {
                if step.gate.as_deref() == Some("github_ci") {
                    match observe_ci(run, deps) {
                        Ok(true) => {
                            event(run, "gate_passed", "github_ci");
                            transition(run, step.next.as_ref());
                        }
                        Ok(false) => return Ok(()),
                        Err(block) => handle_block(run, workflow, step, block),
                    }
                    continue;
                }
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
                let executions = run.runs.get(&run.current).copied().unwrap_or(0);
                // Limits bound visits, not attempts. A retry, check rerun or
                // answer repeats the current visit and must still start.
                let visit = visits(run, &run.current);
                if step.max_runs.is_some_and(|limit| visit > limit)
                    || step.max_rounds.is_some_and(|limit| visit > limit)
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
                if step.gate.is_some() {
                    // Retrying an automatic observation is not another gate visit.
                    if step.gate.as_deref() != Some("github_ci")
                        || !run.gate_requests.contains_key(&run.current)
                    {
                        *run.gate_requests.entry(run.current.clone()).or_default() += 1;
                    }
                    run.status = Status::Gate;
                    event(run, "gate_opened", step.gate.as_deref().unwrap());
                    if step.gate.as_deref() == Some("github_ci") {
                        // Publish the automatic wait before its first external poll.
                        // A short policy deadline must not erase that durable boundary
                        // when a slow initial observation consumes the whole budget.
                        match prepare_ci_wait(run, deps) {
                            Ok(_) => return Ok(()),
                            Err(block) => handle_block(run, workflow, step, block),
                        }
                    }
                    continue;
                }
                if matches!(step.action.as_deref(), Some("open_draft_pr" | "ready_pr")) {
                    let outcome = lifecycle_action(run, workflow, step, deps);
                    match outcome {
                        Ok(true) => {
                            run.runs.insert(run.current.clone(), executions + 1);
                            event(run, "step_done", step.action.as_deref().unwrap());
                            transition(run, step.next.as_ref());
                        }
                        Ok(false) => return Ok(()),
                        Err(block) => handle_block(run, workflow, step, block),
                    }
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
                let started = match run
                    .owner_question
                    .as_ref()
                    .filter(|q| q.step == run.current)
                {
                    Some(question) if question.answer.is_some() => deps.agent.resume_after_answer(
                        &question.previous_handle,
                        &request,
                        question.answer.as_deref().unwrap(),
                    ),
                    _ => deps.agent.start(&request),
                };
                match started {
                    Ok(handle) => {
                        let attempt = Attempt {
                            step: request.step,
                            number: request.number,
                            handle,
                            started_at: deps.clock.now(),
                            family,
                            freezes: request.freezes,
                        };
                        run.owner_question = None;
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

fn lifecycle_action(
    run: &mut Run,
    workflow: &Workflow,
    step: &Step,
    deps: &mut Dependencies<'_>,
) -> std::result::Result<bool, Block> {
    let candidate = run.candidate.clone().ok_or_else(|| {
        Block::new(
            BlockKind::Capability,
            "publication requires accepted GREEN evidence",
        )
    })?;
    if workflow
        .steps
        .values()
        .any(|s| s.review_amendments.is_some())
        && candidate.amendments.is_none()
    {
        return Err(Block::new(
            BlockKind::Capability,
            "light publication requires amendment proof",
        ));
    }
    if deps.git.candidate(&candidate.reference)? != candidate.sha {
        return Err(Block::new(
            BlockKind::Capability,
            "accepted candidate changed",
        ));
    }
    if step.action.as_deref() == Some("open_draft_pr") {
        let pr = deps.code_host.open_draft_pr(&run.id, &candidate)?;
        if pr.trim().is_empty() {
            return Err(Block::new(
                BlockKind::Capability,
                "draft publication returned no PR",
            ));
        }
        run.pr = Some(pr);
        return Ok(true);
    }
    let review = run.review_receipt.clone().ok_or_else(|| {
        Block::new(
            BlockKind::Capability,
            "ready requires an accepted independent clean review",
        )
    })?;
    if review.candidate_sha != candidate.sha {
        return Err(Block::new(BlockKind::Capability, "ready review is stale"));
    }
    if !observe_ci(run, deps)? {
        return Ok(false);
    }
    let digest = run
        .ci_policy_digest
        .as_deref()
        .ok_or_else(|| Block::new(BlockKind::Capability, "missing CI policy digest"))?;
    let policy = deps.code_host.ci_policy(digest)?;
    let auth = Authorization {
        run_id: run.id.clone(),
        pr: run.pr.clone().unwrap_or_default(),
        amendment_disposition: review.amendment_disposition(),
        candidate,
        review,
        ci: run
            .ci
            .as_ref()
            .and_then(|c| c.passed.clone())
            .ok_or_else(|| {
                Block::new(
                    BlockKind::Capability,
                    "ready requires exact-head passing CI",
                )
            })?,
    };
    auth.validate(&policy, digest)
        .map_err(|e| Block::new(BlockKind::Capability, e.to_string()))?;
    deps.code_host.ready_pr(&auth, &policy)?;
    Ok(true)
}

fn prepare_ci_wait(
    run: &mut Run,
    deps: &mut Dependencies<'_>,
) -> std::result::Result<ci::Policy, Block> {
    let candidate = run
        .candidate
        .as_ref()
        .ok_or_else(|| Block::new(BlockKind::Capability, "CI requires accepted GREEN evidence"))?;
    let digest = run
        .ci_policy_digest
        .as_deref()
        .ok_or_else(|| Block::new(BlockKind::Capability, "missing pinned CI policy digest"))?;
    let policy = deps.code_host.ci_policy(digest)?;
    let now = deps.clock.now();
    let visit = visits(run, &run.current);
    if run
        .ci
        .as_ref()
        .is_none_or(|c| c.expected_sha != candidate.sha)
    {
        run.ci = Some(ci::WaitState {
            step: run.current.clone(),
            visit,
            expected_sha: candidate.sha.clone(),
            first_wait: now,
            next_poll: now,
            missing: policy.selected_checks.clone(),
            checks: vec![],
            passed: None,
        });
    }
    let ci = run.ci.as_mut().unwrap();
    if ci.step != run.current || ci.visit != visit {
        ci.step = run.current.clone();
        ci.visit = visit;
        ci.next_poll = now;
        ci.passed = None;
    }
    Ok(policy)
}

fn observe_ci(run: &mut Run, deps: &mut Dependencies<'_>) -> std::result::Result<bool, Block> {
    let policy = prepare_ci_wait(run, deps)?;
    let candidate = run.candidate.clone().unwrap();
    let digest = run.ci_policy_digest.clone().unwrap();
    let pr = run
        .pr
        .clone()
        .ok_or_else(|| Block::new(BlockKind::Capability, "CI has no published pull request"))?;
    let now = deps.clock.now();
    let ci = run.ci.as_mut().unwrap();
    let age = now.saturating_sub(ci.first_wait);
    if (ci.passed.is_none() && age >= policy.timeout_seconds)
        || (!ci.missing.is_empty() && age >= policy.no_checks_timeout_seconds)
    {
        let names = if ci.missing.is_empty() {
            policy.selected_checks.clone()
        } else {
            ci.missing.clone()
        };
        return Err(ci_deadline(&names, &pr));
    }
    if now < ci.next_poll {
        return Ok(false);
    }
    ci.next_poll = now.saturating_add(policy.poll_interval_seconds);
    ci.passed = None;
    if deps.git.candidate(&candidate.reference)? != candidate.sha {
        return Err(Block::new(BlockKind::Capability, "CI candidate changed"));
    }
    let observation = deps
        .code_host
        .ci_status(&pr, &candidate, &policy, &digest)?;
    let observed_at = deps.clock.now();
    ci.next_poll = observed_at.saturating_add(policy.poll_interval_seconds);
    let age = observed_at.saturating_sub(ci.first_wait);
    if age >= policy.timeout_seconds {
        return Err(Block::new(
            BlockKind::Capability,
            format!("CI observation exceeded overall deadline ({pr})"),
        ));
    }
    match observation {
        ci::Observation::Passed(receipt) => {
            if deps.git.candidate(&candidate.reference)? != candidate.sha
                || receipt.candidate_sha != candidate.sha
                || receipt.policy_digest != digest
            {
                return Err(Block::new(
                    BlockKind::Capability,
                    "CI receipt or local candidate changed",
                ));
            }
            ci.missing.clear();
            ci.checks = receipt.checks.clone();
            ci.passed = Some(receipt);
            Ok(true)
        }
        ci::Observation::Waiting { missing, checks } => {
            ci.missing = missing;
            ci.checks = checks;
            if !ci.missing.is_empty() && age >= policy.no_checks_timeout_seconds {
                return Err(ci_deadline(&ci.missing, &pr));
            }
            Ok(false)
        }
    }
}

fn ci_deadline(names: &[String], pr: &str) -> Block {
    let mut reason = format!("CI deadline reached; pending/missing checks ({pr}): ");
    let mut omitted = 0;
    for name in names {
        if reason.len() + name.len() < 7800 {
            reason.push_str(name);
            reason.push_str(", ");
        } else {
            omitted += 1;
        }
    }
    if omitted > 0 {
        reason.push_str(&format!("{omitted} additional jobs; see pinned CI policy"));
    }
    Block::new(BlockKind::Capability, reason)
}

fn event(run: &mut Run, kind: &str, reason: &str) {
    run.events.push(Event {
        kind: kind.into(),
        step: run.current.clone(),
        reason: reason.into(),
    });
}

/// Entries into `step`, including the current one. Every event carries the
/// step that was current when it was recorded, so a change of step in the log
/// starts a visit; a `route` back to the same step starts another. Deriving
/// this from the journal also covers runs persisted before visits mattered.
pub fn visits(run: &Run, step: &str) -> u32 {
    let mut count = 0;
    let mut last: Option<&str> = None;
    for event in &run.events {
        if last != Some(event.step.as_str()) {
            last = Some(&event.step);
            count += u32::from(event.step == step);
        }
        count += u32::from(event.kind == "route" && event.step == step && event.reason == step);
    }
    count + u32::from(last != Some(run.current.as_str()) && run.current == step)
}

fn transition(run: &mut Run, next: Option<&String>) {
    // A completed/overridden visit must not consume a future visit's retry
    // allowance, including gates and built-in actions without bot completion.
    run.retries.remove(&run.current);
    run.check_failures.remove(&run.current);
    run.active = None;
    run.block = None;
    run.owner_question = None;
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
            BlockKind::CiFailed => "ci_failed",
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
    // Retrying instead of answering withdraws the pending question. An
    // answered question stays so a failed resume can be retried.
    if run
        .owner_question
        .as_ref()
        .is_some_and(|q| q.answer.is_none())
    {
        run.owner_question = None;
    }
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
    ensure!(step.gate.is_none(), "gates cannot be overridden");
    ensure!(
        !matches!(step.action.as_deref(), Some("open_draft_pr" | "ready_pr")),
        "lifecycle actions cannot be overridden"
    );
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
    #[serde(default)]
    accepted_amendments: Option<String>,
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
        if result.accepted_amendments.is_some() {
            ensure!(
                step.review_amendments.is_some(),
                "accepted_amendments requires opted-in review"
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
