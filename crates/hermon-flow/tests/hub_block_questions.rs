//! Issue 143: questions on a stopped worker block must be answerable.
use hermon_flow::{hub::*, workflow::Workflow};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const WORKFLOW: &str = "version: 1
name: block-questions
steps:
  plan:
    bot: planner
    next: owner_go
  owner_go:
    gate: owner_go
    next: work
  work:
    bot: worker
    on_block: {scope_change: plan}
limits:
  transient_retries: 1
";

fn finished(kind: Option<&str>, questions: &[&str]) -> Poll {
    let block = kind.map(|kind| serde_json::json!({"kind": kind, "reason": "needs a decision"}));
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
                "verdict": null,
                "questions": questions,
            })
            .to_string(),
        ),
    }
}

#[derive(Default)]
struct FakeAgent {
    starts: Vec<StartRequest>,
    resumes: Vec<(String, String, String)>,
    resume_errors: usize,
    polls: BTreeMap<String, VecDeque<Poll>>,
}
impl Agent for FakeAgent {
    fn family(&self, _: &str) -> String {
        "fixture".into()
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
        self.resumes
            .push((previous.into(), request.step.clone(), answer.into()));
        if self.resume_errors > 0 {
            self.resume_errors -= 1;
            return Err(Block::new(BlockKind::Capability, "cannot deliver answer"));
        }
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
    questions: Vec<(String, u32, String, Vec<String>)>,
    answers: BTreeMap<(String, u32), String>,
    approved: BTreeSet<(String, u32)>,
    alerts: Vec<Alert>,
}
impl Tracker for FakeTracker {
    fn question(
        &mut self,
        _: &str,
        step: &str,
        request: u32,
        bot: &str,
        questions: &[String],
    ) -> Result<(), Block> {
        self.questions
            .push((step.into(), request, bot.into(), questions.to_vec()));
        Ok(())
    }
    fn answer(&mut self, _: &str, step: &str, request: u32) -> Result<Option<String>, Block> {
        Ok(self.answers.get(&(step.into(), request)).cloned())
    }
    fn gate_approved(&mut self, _: &str, step: &str, request: u32) -> Result<bool, Block> {
        Ok(self.approved.contains(&(step.into(), request)))
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
    checks: usize,
}
impl Git for FakeGit {
    fn check(&mut self, _: &str, _: &[String], _: &BTreeMap<String, String>) -> Result<(), Block> {
        self.checks += 1;
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
    fn new(start: &str) -> Self {
        Self {
            workflow: Workflow::from_str("block-questions", WORKFLOW).unwrap(),
            run: Run::new("RUN-143", start),
            agent: FakeAgent::default(),
            tracker: FakeTracker::default(),
            git: FakeGit::default(),
        }
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
            .expect("expected a running worker")
            .handle
            .clone();
        self.agent
            .polls
            .entry(handle)
            .or_default()
            .push_back(outcome);
        self.tick();
    }
}

const QUESTIONS: [&str; 2] = [
    "May the deadline repair go to a separate issue?",
    "May the readiness timeout get bounded diagnosis?",
];

#[test]
fn unrouted_scope_change_questions_register_an_owner_question() {
    let mut r = Rig::new("plan");
    r.tick();
    r.finish(finished(Some("scope_change"), &QUESTIONS));
    assert_eq!(r.run.status, Status::Blocked, "{:?}", r.run);
    assert_eq!(r.run.current, "plan");
    assert_eq!(
        r.run.block.as_ref().map(|b| b.kind.clone()),
        Some(BlockKind::ScopeChange),
        "the original block kind must stay visible"
    );
    let question = r
        .run
        .owner_question
        .clone()
        .expect("worker questions must be registered for an owner reply");
    assert_eq!(question.step, "plan");
    assert_eq!(question.request, 1);
    assert_eq!(question.bot, "planner");
    assert_eq!(question.questions, QUESTIONS.map(String::from).to_vec());
    assert_eq!(question.previous_handle, "plan#1");
    assert_eq!(question.answer, None);
    assert_eq!(
        r.tracker.questions,
        vec![(
            "plan".into(),
            1,
            "planner".into(),
            QUESTIONS.map(String::from).to_vec()
        )],
        "the questions must be posted through the tracker question flow"
    );
    assert!(!r.tracker.alerts.is_empty(), "the block must still alert");
    assert_eq!(r.git.checks, 0, "a blocked result never runs step checks");
}

#[test]
fn answered_scope_change_resumes_the_same_step_without_passing_owner_go() {
    let mut r = Rig::new("plan");
    r.tick();
    r.finish(finished(Some("scope_change"), &QUESTIONS));
    r.tracker
        .answers
        .insert(("plan".into(), 1), "Use elapsed 24-hour periods".into());
    r.tick();
    assert_eq!(
        r.agent.resumes,
        vec![(
            "plan#1".into(),
            "plan".into(),
            "Use elapsed 24-hour periods".into()
        )],
        "the answer must reach the same step through the owner-answer adapter"
    );
    assert_eq!(r.run.status, Status::Running);
    assert_eq!(r.run.active.as_ref().unwrap().number, 2);
    assert!(r.run.owner_question.is_none());
    assert!(
        r.run
            .events
            .iter()
            .any(|e| e.kind == "owner_answer" && e.reason == "Use elapsed 24-hour periods")
    );
    r.finish(finished(None, &[]));
    for _ in 0..3 {
        r.tick();
    }
    assert_eq!(r.run.current, "owner_go");
    assert_eq!(
        r.run.status,
        Status::Gate,
        "the answer cannot pass Owner GO"
    );
    assert_eq!(r.run.gate_requests["owner_go"], 1);
    assert_eq!(
        r.agent.starts.iter().filter(|s| s.step == "work").count(),
        0
    );
}

#[test]
fn routed_scope_change_does_not_leave_a_pending_question() {
    let mut r = Rig::new("plan");
    r.tick();
    r.finish(finished(None, &[]));
    r.tracker.approved.insert(("owner_go".into(), 1));
    r.tick();
    assert_eq!(r.run.current, "work");
    r.finish(finished(Some("scope_change"), &QUESTIONS));
    assert_eq!(r.run.current, "plan", "the configured route still wins");
    assert!(r.run.owner_question.is_none(), "{:?}", r.run.owner_question);
    assert!(r.tracker.questions.is_empty());
}

#[test]
fn scope_change_without_questions_has_no_owner_question() {
    let mut r = Rig::new("plan");
    r.tick();
    r.finish(finished(Some("scope_change"), &[]));
    assert_eq!(r.run.status, Status::Blocked);
    assert!(r.run.owner_question.is_none());
    assert!(r.tracker.questions.is_empty());
}

#[test]
fn other_stopping_worker_blocks_with_questions_are_answerable() {
    for kind in ["capability", "test_defect", "unknown"] {
        let mut r = Rig::new("plan");
        r.tick();
        r.finish(finished(Some(kind), &QUESTIONS[..1]));
        assert_eq!(r.run.status, Status::Blocked, "{kind}");
        let question = r.run.owner_question.clone().expect(kind);
        assert_eq!(question.questions, vec![QUESTIONS[0].to_string()], "{kind}");
        assert_eq!(r.tracker.questions.len(), 1, "{kind}");
    }
}

#[test]
fn transient_block_questions_do_not_register_an_owner_question() {
    let mut r = Rig::new("plan");
    r.tick();
    r.finish(finished(Some("transient"), &QUESTIONS));
    assert_eq!(
        r.run.status,
        Status::Running,
        "transient blocks retry first"
    );
    r.finish(finished(Some("transient"), &QUESTIONS));
    assert_eq!(r.run.status, Status::Blocked);
    assert!(r.run.owner_question.is_none());
    assert!(r.tracker.questions.is_empty());
}

#[test]
fn manual_retry_of_a_question_block_starts_fresh() {
    let mut r = Rig::new("plan");
    r.tick();
    r.finish(finished(Some("scope_change"), &QUESTIONS));
    retry(&mut r.run, "owner retries without answering").unwrap();
    assert!(
        r.run.owner_question.is_none(),
        "retry withdraws the unanswered question"
    );
    r.tick();
    assert!(r.agent.resumes.is_empty(), "retry is not an answer");
    assert_eq!(r.agent.starts.len(), 2);
    assert!(r.run.owner_question.is_none());
}

#[test]
fn retry_after_a_failed_resume_keeps_the_answer() {
    let mut r = Rig::new("plan");
    r.tick();
    r.finish(finished(Some("scope_change"), &QUESTIONS));
    r.agent.resume_errors = 1;
    r.tracker
        .answers
        .insert(("plan".into(), 1), "Use elapsed 24-hour periods".into());
    r.tick();
    assert_eq!(r.run.status, Status::Blocked, "{:?}", r.run);
    assert_eq!(
        r.run.block.as_ref().map(|b| b.kind.clone()),
        Some(BlockKind::Capability)
    );
    let questions = r.tracker.questions.len();
    r.tick();
    assert_eq!(
        r.tracker.questions.len(),
        questions,
        "an answered question is not asked again"
    );
    retry(&mut r.run, "adapter fixed").unwrap();
    r.tick();
    assert_eq!(r.agent.resumes.len(), 2);
    assert_eq!(r.agent.resumes[1].2, "Use elapsed 24-hour periods");
    assert_eq!(r.run.status, Status::Running);
    assert!(r.agent.starts.len() >= 2);
}
