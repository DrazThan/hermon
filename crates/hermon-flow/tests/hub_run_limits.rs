//! Issue 146: restarts within one visit must not spend a step's run limit.
use hermon_flow::{hub::*, workflow::Workflow};
use std::collections::{BTreeMap, VecDeque};

const WORKFLOW: &str = "version: 1
name: run-limits
steps:
  green:
    bot: worker
    next: review
    on_block: {test_defect: red_fix}
  red_fix:
    bot: fixer
    next: green
    max_runs: 2
  review:
    bot: reviewer
    on_verdict: {changes_needed: green}
    max_rounds: 2
limits:
  transient_retries: 1
";

const LIMIT: &str = "step execution limit reached; owner action required";

fn result(block: Option<&str>, verdict: Option<&str>, questions: &[&str]) -> Poll {
    let block = block.map(|kind| serde_json::json!({"kind": kind, "reason": "needs work"}));
    Poll::Finished {
        process_success: true,
        completion: true,
        result: Some(
            serde_json::json!({
                "schema": 1,
                "status": if block.is_some() { "block" } else { "done" },
                "summary": "worker finished",
                "artifacts": [],
                "block": block,
                "verdict": verdict,
                "questions": questions,
            })
            .to_string(),
        ),
    }
}
fn done() -> Poll {
    result(None, None, &[])
}
fn verdict(v: &str) -> Poll {
    result(None, Some(v), &[])
}
fn blocked(kind: &str) -> Poll {
    result(Some(kind), None, &[])
}
/// The issue's observation: ACP reported `stopReason: cancelled`, no result.
fn cancelled() -> Poll {
    Poll::Finished {
        process_success: false,
        completion: false,
        result: None,
    }
}

#[derive(Default)]
struct FakeAgent {
    starts: Vec<StartRequest>,
    resumes: Vec<(String, String)>,
    polls: BTreeMap<String, VecDeque<Poll>>,
}
impl Agent for FakeAgent {
    fn family(&self, bot: &str) -> String {
        bot.into()
    }
    fn available(&self, _: &str) -> bool {
        true
    }
    fn start(&mut self, request: &StartRequest) -> Result<String, Block> {
        self.starts.push(request.clone());
        Ok(format!("{}#{}", request.step, request.number))
    }
    fn resume_after_answer(
        &mut self,
        previous: &str,
        request: &StartRequest,
        answer: &str,
    ) -> Result<String, Block> {
        self.resumes.push((previous.into(), answer.into()));
        self.start(request)
    }
    fn poll(&mut self, handle: &str) -> Result<Poll, Block> {
        Ok(self
            .polls
            .get_mut(handle)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Poll::Running))
    }
    fn stop(&mut self, _: &str) -> Result<Settlement, Block> {
        Ok(Settlement::Confirmed)
    }
}

#[derive(Default)]
struct FakeTracker {
    answers: BTreeMap<(String, u32), String>,
    alerts: Vec<Alert>,
}
impl Tracker for FakeTracker {
    fn answer(&mut self, _: &str, step: &str, request: u32) -> Result<Option<String>, Block> {
        Ok(self.answers.get(&(step.into(), request)).cloned())
    }
    fn gate_approved(&mut self, _: &str, _: &str, _: u32) -> Result<bool, Block> {
        Ok(false)
    }
    fn alert(&mut self, _: &str, alert: &Alert) -> anyhow::Result<()> {
        self.alerts.push(alert.clone());
        Ok(())
    }
}

struct FakeHost;
impl CodeHost for FakeHost {
    fn open_pr(&mut self, _: &str) -> Result<String, Block> {
        Ok("PR-1".into())
    }
    fn merged(&mut self, _: &str) -> Result<bool, Block> {
        Ok(false)
    }
}

#[derive(Default)]
struct FakeGit {
    fail_checks: usize,
}
impl Git for FakeGit {
    fn check(&mut self, _: &str, _: &[String], _: &BTreeMap<String, String>) -> Result<(), Block> {
        if self.fail_checks > 0 {
            self.fail_checks -= 1;
            return Err(Block::new(BlockKind::Unknown, "fixture check failed"));
        }
        Ok(())
    }
    fn evidence(&mut self, _: &str, tag: &str, revision: u32) -> Result<String, Block> {
        Ok(format!("{tag}.{revision}"))
    }
}

struct FakeClock;
impl Clock for FakeClock {
    fn now(&self) -> u64 {
        0
    }
}

struct Rig {
    workflow: Workflow,
    run: Run,
    agent: FakeAgent,
    tracker: FakeTracker,
    git: FakeGit,
}
impl Rig {
    fn new() -> Self {
        let mut rig = Self {
            workflow: Workflow::from_str("run-limits", WORKFLOW).unwrap(),
            run: Run::new("RUN-146", "green"),
            agent: FakeAgent::default(),
            tracker: FakeTracker::default(),
            git: FakeGit::default(),
        };
        rig.tick();
        rig
    }
    fn tick(&mut self) {
        advance(
            &mut self.run,
            &self.workflow,
            &mut Dependencies {
                agent: &mut self.agent,
                tracker: &mut self.tracker,
                code_host: &mut FakeHost,
                git: &mut self.git,
                clock: &FakeClock,
            },
        )
        .unwrap();
    }
    fn finish(&mut self, outcome: Poll) {
        let handle = self
            .run
            .active
            .as_ref()
            .unwrap_or_else(|| panic!("expected a running worker: {:?}", self.run))
            .handle
            .clone();
        self.agent
            .polls
            .entry(handle)
            .or_default()
            .push_back(outcome);
        self.tick();
    }
    fn active(&self) -> (String, u32) {
        let attempt = self
            .run
            .active
            .as_ref()
            .unwrap_or_else(|| panic!("expected a running worker: {:?}", self.run));
        (attempt.step.clone(), attempt.number)
    }
    fn starts(&self, step: &str) -> usize {
        self.agent.starts.iter().filter(|s| s.step == step).count()
    }
    fn limit_blocked(&self) -> bool {
        self.run.events.iter().any(|e| e.reason == LIMIT)
    }
    /// green#1 -> red_fix#1 -> green#2 -> red_fix#2, the last allowed visit.
    fn reach_last_red_fix_visit(&mut self) {
        self.finish(blocked("test_defect"));
        assert_eq!(self.active(), ("red_fix".into(), 1));
        self.finish(done());
        assert_eq!(self.active(), ("green".into(), 2));
        self.finish(blocked("test_defect"));
        assert_eq!(self.active(), ("red_fix".into(), 2));
    }
}

#[test]
fn transient_failure_on_last_allowed_visit_retries_within_the_visit() {
    let mut r = Rig::new();
    r.reach_last_red_fix_visit();
    r.finish(cancelled());
    assert!(
        !r.limit_blocked(),
        "transient retry hit the run limit: {:?}",
        r.run
    );
    assert_eq!(r.run.status, Status::Running, "{:?}", r.run);
    assert_eq!(r.active(), ("red_fix".into(), 3));

    r.finish(done());
    assert_eq!(r.active(), ("green".into(), 3));

    // The limit still bounds the correction loop: a third visit is refused.
    r.finish(blocked("test_defect"));
    assert_eq!(r.run.status, Status::Blocked);
    assert_eq!(r.run.current, "red_fix");
    assert_eq!(r.run.block.as_ref().map(|b| b.reason.as_str()), Some(LIMIT));
    assert_eq!(r.starts("red_fix"), 3);
}

#[test]
fn check_rerun_on_last_allowed_visit_starts() {
    let mut r = Rig::new();
    r.reach_last_red_fix_visit();
    r.git.fail_checks = 1;
    r.finish(done());
    assert!(r.run.events.iter().any(|e| e.kind == "check_retry"));
    assert!(
        !r.limit_blocked(),
        "check rerun hit the run limit: {:?}",
        r.run
    );
    assert_eq!(r.active(), ("red_fix".into(), 3));
}

#[test]
fn owner_retry_after_exhausted_transients_on_last_visit_needs_no_override() {
    let mut r = Rig::new();
    r.reach_last_red_fix_visit();
    r.finish(cancelled());
    assert_eq!(r.active(), ("red_fix".into(), 3));
    r.finish(cancelled());
    assert_eq!(r.run.status, Status::Blocked);
    let block = r.run.block.clone().unwrap();
    assert_eq!(block.kind, BlockKind::Transient, "{block:?}");
    assert_eq!(block.reason, "agent process failed");
    assert!(!r.limit_blocked());

    retry(&mut r.run, "worker restarted").unwrap();
    r.tick();
    assert_eq!(r.run.status, Status::Running, "{:?}", r.run);
    assert_eq!(r.active(), ("red_fix".into(), 4));
}

#[test]
fn answered_question_on_last_visit_resumes() {
    let mut r = Rig::new();
    r.reach_last_red_fix_visit();
    r.tracker
        .answers
        .insert(("red_fix".into(), 2), "keep the fixture".into());
    r.finish(result(Some("owner_question"), None, &["Keep the fixture?"]));
    assert!(
        !r.limit_blocked(),
        "owner answer hit the run limit: {:?}",
        r.run
    );
    assert_eq!(
        r.agent.resumes,
        vec![("red_fix#2".into(), "keep the fixture".into())]
    );
    assert_eq!(r.active(), ("red_fix".into(), 3));
}

#[test]
fn review_transient_retry_does_not_spend_a_round() {
    let mut r = Rig::new();
    r.finish(done());
    assert_eq!(r.active(), ("review".into(), 1));
    r.finish(cancelled());
    assert_eq!(r.active(), ("review".into(), 2));
    r.finish(verdict("changes_needed"));
    assert_eq!(
        r.run.current, "green",
        "first review round was treated as the last: {:?}",
        r.run
    );
    assert_eq!(r.active(), ("green".into(), 2));

    // The second review visit is still the last allowed round.
    r.finish(done());
    assert_eq!(r.active(), ("review".into(), 3));
    r.finish(verdict("changes_needed"));
    assert_eq!(r.run.status, Status::Blocked);
    assert_eq!(
        r.run.block.as_ref().map(|b| b.reason.as_str()),
        Some("review max_rounds reached; owner action required")
    );
}

#[test]
fn self_route_counts_as_a_new_visit() {
    let workflow = Workflow::from_str(
        "self-loop",
        "version: 1\nname: self-loop\nsteps:\n  fix:\n    bot: fixer\n    on_block: {test_defect: fix}\n    max_runs: 2\n",
    )
    .unwrap();
    let mut r = Rig::new();
    r.workflow = workflow;
    r.run = Run::new("RUN-146", "fix");
    r.agent = FakeAgent::default();
    r.tick();
    r.finish(blocked("test_defect"));
    assert_eq!(r.active(), ("fix".into(), 2));
    r.finish(blocked("test_defect"));
    assert_eq!(r.run.status, Status::Blocked);
    assert_eq!(r.run.block.as_ref().map(|b| b.reason.as_str()), Some(LIMIT));
    assert_eq!(r.starts("fix"), 2);
}
