//! Frozen, independently authored Slice 3 behavioral acceptance tests.
use hermon_flow::{hub::*, workflow::Workflow};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

fn done(verdict: Option<&str>) -> Poll {
    Poll::Finished {
        process_success: true,
        completion: true,
        result: Some(
            serde_json::json!({
                "schema":1,"status":"done","summary":"finished","artifacts":[],"block":null,
                "verdict":verdict,"questions":[]
            })
            .to_string(),
        ),
    }
}
fn blocked(kind: &str) -> Poll {
    Poll::Finished {
        process_success: true,
        completion: true,
        result: Some(
            serde_json::json!({
                "schema":1,"status":"block","summary":"blocked","artifacts":[],
                "block":{"kind":kind,"reason":"specific reason"},"verdict":null,"questions":[]
            })
            .to_string(),
        ),
    }
}
#[derive(Default)]
struct FakeAgent {
    start_errors: VecDeque<Block>,
    poll_error: bool,
    stop_error: bool,
    starts: Vec<StartRequest>,
    polls: BTreeMap<String, VecDeque<Poll>>,
    stops: Vec<String>,
    unavailable: bool,
    same_family: bool,
    unsettled_stop: bool,
}
impl Agent for FakeAgent {
    fn family(&self, bot: &str) -> String {
        if self.same_family {
            "same".into()
        } else if bot == "sdlc-reviewer" {
            "review-family".into()
        } else {
            "worker-family".into()
        }
    }
    fn available(&self, _: &str) -> bool {
        !self.unavailable
    }
    fn start(&mut self, r: &StartRequest) -> Result<String, Block> {
        self.starts.push(r.clone());
        if let Some(error) = self.start_errors.pop_front() {
            return Err(error);
        }
        Ok(format!("{}#{}", r.step, r.number))
    }
    fn poll(&mut self, handle: &str) -> Result<Poll, Block> {
        if self.poll_error {
            return Err(Block::new(BlockKind::Transient, "lost worker connection"));
        }
        Ok(self
            .polls
            .get_mut(handle)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Poll::Running))
    }
    fn stop(&mut self, handle: &str) -> Result<Settlement, Block> {
        self.stops.push(handle.into());
        if self.stop_error {
            return Err(Block::new(BlockKind::Capability, "cannot settle worker"));
        }
        Ok(if self.unsettled_stop {
            Settlement::Unsettled
        } else {
            Settlement::Confirmed
        })
    }
}
#[derive(Default)]
struct FakeTracker {
    gate_errors: usize,
    gate_error: bool,
    gate_polls: usize,
    fail_alerts: usize,
    approved: BTreeSet<(String, u32)>,
    alerts: Vec<Alert>,
}
impl Tracker for FakeTracker {
    fn gate_approved(&mut self, _: &str, step: &str, request: u32) -> Result<bool, Block> {
        self.gate_polls += 1;
        if self.gate_error || self.gate_errors > 0 {
            self.gate_errors = self.gate_errors.saturating_sub(1);
            return Err(Block::new(
                BlockKind::Transient,
                "tracker temporarily unavailable",
            ));
        }
        Ok(self.approved.contains(&(step.into(), request)))
    }
    fn alert(&mut self, _: &str, alert: &Alert) -> anyhow::Result<()> {
        if self.fail_alerts > 0 {
            self.fail_alerts -= 1;
            anyhow::bail!("tracker unavailable");
        }
        self.alerts.push(alert.clone());
        Ok(())
    }
}
#[derive(Default)]
struct FakeHost {
    transient_errors: usize,
    opened: usize,
    merged: bool,
    fail: bool,
}
impl CodeHost for FakeHost {
    fn open_pr(&mut self, _: &str) -> Result<String, Block> {
        self.opened += 1;
        if self.transient_errors > 0 {
            self.transient_errors -= 1;
            return Err(Block::new(
                BlockKind::Transient,
                "code host temporarily unavailable",
            ));
        }
        if self.fail {
            Err(Block::new(BlockKind::Capability, "code host login missing"))
        } else {
            Ok("PR-1".into())
        }
    }
    fn merged(&mut self, pr: &str) -> Result<bool, Block> {
        assert_eq!(pr, "PR-1");
        Ok(self.merged)
    }
}
type CheckCall = (String, Vec<String>, BTreeMap<String, String>);
#[derive(Default)]
struct FakeGit {
    check_error: Option<BlockKind>,
    evidence_error: Option<BlockKind>,
    fail_evidence: bool,
    checks: Vec<CheckCall>,
    evidence: Vec<(String, String, u32)>,
    fail_checks: usize,
}
impl Git for FakeGit {
    fn check(
        &mut self,
        step: &str,
        checks: &[String],
        freezes: &BTreeMap<String, String>,
    ) -> Result<(), Block> {
        self.checks
            .push((step.into(), checks.to_vec(), freezes.clone()));
        if let Some(kind) = &self.check_error {
            return Err(Block::new(kind.clone(), "check temporarily unavailable"));
        }
        if self.fail_checks > 0 {
            self.fail_checks -= 1;
            Err(Block::new(BlockKind::Unknown, "project check failed"))
        } else {
            Ok(())
        }
    }
    fn evidence(&mut self, step: &str, tag: &str, revision: u32) -> Result<String, Block> {
        self.evidence.push((step.into(), tag.into(), revision));
        if let Some(kind) = &self.evidence_error {
            return Err(Block::new(kind.clone(), "evidence temporarily unavailable"));
        }
        if self.fail_evidence {
            return Err(Block::new(BlockKind::Capability, "cannot write evidence"));
        }
        Ok(format!("{tag}.{revision}"))
    }
}
#[derive(Default)]
struct FakeClock(u64);
impl Clock for FakeClock {
    fn now(&self) -> u64 {
        self.0
    }
}
struct Rig {
    workflow: Workflow,
    run: Run,
    agent: FakeAgent,
    tracker: FakeTracker,
    host: FakeHost,
    git: FakeGit,
    clock: FakeClock,
}
impl Rig {
    fn new() -> Self {
        Self {
            workflow: Workflow::from_str("sdlc", include_str!("fixtures/sdlc.yaml")).unwrap(),
            run: Run::new("RUN-118", "plan"),
            agent: FakeAgent::default(),
            tracker: FakeTracker::default(),
            host: FakeHost::default(),
            git: FakeGit::default(),
            clock: FakeClock(100),
        }
    }
    fn tick(&mut self) {
        advance(
            &mut self.run,
            &self.workflow,
            &mut Dependencies {
                agent: &mut self.agent,
                tracker: &mut self.tracker,
                code_host: &mut self.host,
                git: &mut self.git,
                clock: &self.clock,
            },
        )
        .unwrap();
    }
    fn finish(&mut self, outcome: Poll) {
        let handle = self
            .run
            .active
            .as_ref()
            .expect("expected running bot")
            .handle
            .clone();
        self.agent
            .polls
            .entry(handle)
            .or_default()
            .push_back(outcome);
        self.tick();
    }
    fn approve(&mut self) {
        let step = self.run.current.clone();
        let request = *self
            .run
            .gate_requests
            .get(&step)
            .expect("gate request must be recorded");
        self.tracker.approved.insert((step, request));
        if self.run.current == "merge" {
            self.host.merged = true;
        }
    }
    fn to(&mut self, target: &str) {
        for _ in 0..100 {
            if self.run.current == target
                && matches!(self.run.status, Status::Running | Status::Gate)
            {
                return;
            }
            match self.run.status {
                Status::Running => {
                    let verdict = (self.run.current == "review").then_some("clean");
                    self.finish(done(verdict));
                }
                Status::Gate => {
                    self.approve();
                    self.tick();
                }
                Status::Ready => self.tick(),
                _ => panic!("unexpected state on route to {target}: {:?}", self.run),
            }
        }
        panic!("failed to reach {target}: {:?}", self.run);
    }
    fn starts(&self, step: &str) -> usize {
        self.agent.starts.iter().filter(|s| s.step == step).count()
    }
    fn alert(&self) {
        assert!(
            !self.tracker.alerts.is_empty(),
            "owner must be alerted: {:?}",
            self.run
        );
    }
}

#[test]
fn hub_happy_path_sdlc() {
    let mut r = Rig::new();
    r.to("docs");
    r.finish(done(None));
    for _ in 0..4 {
        r.tick();
    }
    assert_eq!(r.run.status, Status::Done);
    assert_eq!(r.run.current, "docs");
    assert_eq!(r.host.opened, 1);
    assert_eq!(
        r.agent
            .starts
            .iter()
            .map(|s| s.step.as_str())
            .collect::<Vec<_>>(),
        ["plan", "red", "green", "review", "docs"]
    );
    assert_eq!(
        r.git
            .evidence
            .iter()
            .map(|(_, tag, _)| tag.as_str())
            .collect::<Vec<_>>(),
        ["red", "green"]
    );
    assert_eq!(r.run.pr.as_deref(), Some("PR-1"));
    let copy: Run = serde_json::from_str(&serde_json::to_string(&r.run).unwrap()).unwrap();
    assert_eq!(copy, r.run);
}
#[test]
fn hub_test_defect_routes_to_red_fix() {
    let mut r = Rig::new();
    r.to("green");
    let original = r.run.freezes["red"].clone();
    r.finish(blocked("test_defect"));
    r.to("red_fix");
    r.finish(done(None));
    r.to("green");
    assert_eq!(r.starts("green"), 2);
    assert_ne!(r.run.freezes["red"], original);
    assert_eq!(
        r.agent.starts.last().unwrap().freezes["red"],
        r.run.freezes["red"]
    );
    assert_eq!(
        r.git
            .evidence
            .iter()
            .filter(|(_, tag, _)| tag == "red")
            .count(),
        2
    );
    r.finish(done(None));
    r.to("review");
    assert_eq!(
        r.git
            .checks
            .iter()
            .rev()
            .find(|(step, _, _)| step == "green")
            .unwrap()
            .2["red"],
        r.run.freezes["red"]
    );
}
#[test]
fn hub_review_rounds_bounded() {
    let mut r = Rig::new();
    r.to("review");
    r.finish(done(Some("changes_needed")));
    r.to("green");
    r.finish(done(None));
    r.to("review");
    r.finish(done(Some("changes_needed")));
    for _ in 0..5 {
        r.tick();
    }
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
    assert_eq!(r.starts("review"), 2);
    assert_eq!(r.starts("green"), 2);
    assert_eq!(r.host.opened, 0);
}
#[test]
fn hub_transient_retry_then_alert() {
    let mut r = Rig::new();
    r.to("green");
    for _ in 0..3 {
        r.finish(blocked("transient"));
        for _ in 0..3 {
            r.tick();
        }
    }
    assert_eq!(r.starts("green"), 3);
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
    let count = r.agent.starts.len();
    for _ in 0..10 {
        r.tick();
    }
    assert_eq!(count, r.agent.starts.len());
}
#[test]
fn hub_invalid_result_is_unknown_block() {
    let bad = [
        None,
        Some("{"),
        Some("{}"),
        Some(
            r#"{"schema":2,"status":"done","summary":"x","artifacts":[],"block":null,"verdict":null,"questions":[]}"#,
        ),
        Some(
            r#"{"schema":1,"status":"banana","summary":"x","artifacts":[],"block":null,"verdict":null,"questions":[]}"#,
        ),
        Some(
            r#"{"schema":1,"status":"done","summary":"x","artifacts":[],"block":{"kind":"capability","reason":"x"},"verdict":null,"questions":[]}"#,
        ),
        Some(
            r#"{"schema":1,"status":"block","summary":"x","artifacts":[],"block":null,"verdict":null,"questions":[]}"#,
        ),
    ];
    for result in bad {
        let mut r = Rig::new();
        r.to("green");
        r.finish(Poll::Finished {
            process_success: true,
            completion: true,
            result: result.map(str::to_owned),
        });
        assert_eq!(r.run.status, Status::Blocked, "result: {result:?}");
        assert_eq!(r.run.block.as_ref().unwrap().kind, BlockKind::Unknown);
        r.alert();
        assert_eq!(r.starts("review"), 0);
    }
}
#[test]
fn hub_manual_retry_keeps_automation() {
    let mut r = Rig::new();
    r.to("green");
    r.finish(blocked("capability"));
    r.alert();
    retry(&mut r.run, "installed missing tool").unwrap();
    r.to("green");
    r.finish(done(None));
    r.to("docs");
    r.finish(done(None));
    r.tick();
    assert_eq!(r.run.status, Status::Done);
    assert!(
        r.run
            .events
            .iter()
            .any(|e| e.kind == "retry" && e.reason == "installed missing tool")
    );
}
#[test]
fn hub_gate_blocks_successors() {
    let mut r = Rig::new();
    r.to("owner_go");
    let count = r.agent.starts.len();
    for _ in 0..12 {
        r.tick();
    }
    assert_eq!(r.agent.starts.len(), count);
    assert_eq!(r.run.status, Status::Gate);
    r.alert();
    assert!(override_step(&mut r.run, &r.workflow, "skip owner").is_err());
    r.approve();
    r.to("merge");
    let count = r.agent.starts.len();
    r.host.merged = false;
    for _ in 0..12 {
        r.tick();
    }
    assert_eq!(r.agent.starts.len(), count);
    assert_eq!(r.starts("docs"), 0);
    r.host.merged = true;
    r.to("docs");
}
#[test]
fn hub_advance_idempotent() {
    let mut r = Rig::new();
    r.to("green");
    let snapshot = r.run.clone();
    let starts = r.agent.starts.len();
    let alerts = r.tracker.alerts.len();
    for _ in 0..20 {
        r.tick();
    }
    assert_eq!(r.run, snapshot);
    assert_eq!(r.agent.starts.len(), starts);
    assert_eq!(r.tracker.alerts.len(), alerts);
    r.finish(done(None));
    r.to("merge");
    r.host.merged = false;
    let snapshot = r.run.clone();
    let alerts = r.tracker.alerts.len();
    for _ in 0..20 {
        r.tick();
    }
    assert_eq!(r.run, snapshot);
    assert_eq!(r.host.opened, 1);
    assert_eq!(r.tracker.alerts.len(), alerts);
}
#[test]
fn hub_never_silent_on_block() {
    for kind in [
        "owner_question",
        "test_defect",
        "capability",
        "scope_change",
        "unknown",
    ] {
        let mut r = Rig::new();
        r.to("green");
        let before = r.agent.starts.len();
        r.finish(blocked(kind));
        for _ in 0..5 {
            r.tick();
        }
        assert!(
            r.agent.starts.len() > before || !r.tracker.alerts.is_empty(),
            "silent block: {kind}"
        );
        if matches!(kind, "owner_question" | "capability" | "unknown") {
            assert_eq!(r.run.status, Status::Blocked);
        }
    }
}
#[test]
fn hub_manual_override_keeps_automation() {
    let mut r = Rig::new();
    r.to("green");
    r.finish(blocked("capability"));
    assert!(override_step(&mut r.run, &r.workflow, "").is_err());
    override_step(
        &mut r.run,
        &r.workflow,
        "owner accepts missing optional check",
    )
    .unwrap();
    r.to("docs");
    r.finish(done(None));
    r.tick();
    assert_eq!(r.run.status, Status::Done);
    assert_eq!(r.starts("green"), 1);
    assert!(
        r.run
            .events
            .iter()
            .any(|e| e.kind == "override" && e.reason == "owner accepts missing optional check")
    );
}
#[test]
fn hub_red_fix_max_runs_bounded() {
    let mut r = Rig::new();
    r.to("green");
    for _ in 0..2 {
        r.finish(blocked("test_defect"));
        r.to("red_fix");
        r.finish(done(None));
        r.to("green");
    }
    r.finish(blocked("test_defect"));
    for _ in 0..5 {
        r.tick();
    }
    assert_eq!(r.starts("red_fix"), 2);
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
}
#[test]
fn hub_check_failures_cannot_publish_success() {
    let mut r = Rig::new();
    r.to("green");
    r.git.fail_checks = 2;
    r.finish(done(None));
    r.to("green");
    r.finish(done(None));
    for _ in 0..4 {
        r.tick();
    }
    assert_eq!(r.starts("green"), 2);
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
    assert_eq!(r.starts("review"), 0);
    assert!(!r.git.evidence.iter().any(|(step, _, _)| step == "green"));
}
#[test]
fn hub_timeout_records_failure_before_late_success() {
    let mut r = Rig::new();
    r.to("green");
    r.clock.0 += 90 * 60;
    r.finish(done(None));
    for _ in 0..4 {
        r.tick();
    }
    assert_eq!(r.agent.stops.len(), 1);
    assert_eq!(r.starts("review"), 0);
    assert_eq!(r.starts("green"), 2);
    assert!(r.run.events.iter().any(|e| e.kind == "timeout"));
    assert!(!r.git.evidence.iter().any(|(step, _, _)| step == "green"));
}
#[test]
fn hub_unsettled_outcome_prevents_automatic_or_manual_resend() {
    let mut r = Rig::new();
    r.to("green");
    r.finish(Poll::Unsettled);
    for _ in 0..5 {
        r.tick();
    }
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
    assert!(retry(&mut r.run, "viewer died").is_err());
    assert!(override_step(&mut r.run, &r.workflow, "assume stopped").is_err());
    assert_eq!(r.starts("green"), 1);
    assert!(r.run.active.is_some());
    r.finish(done(None));
    for _ in 0..4 {
        r.tick();
    }
    assert_eq!(r.run.status, Status::Blocked);
    assert_eq!(r.starts("review"), 0);
    retry(&mut r.run, "worker settlement confirmed").unwrap();
    r.to("green");
    assert_eq!(r.starts("green"), 2);
}
#[test]
fn hub_timeout_unsettled_stop_cannot_retry() {
    let mut r = Rig::new();
    r.to("green");
    r.agent.unsettled_stop = true;
    r.clock.0 += 90 * 60;
    r.tick();
    for _ in 0..5 {
        r.tick();
    }
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
    assert_eq!(r.starts("green"), 1);
    assert!(retry(&mut r.run, "try again").is_err());
}
#[test]
fn hub_missing_completion_is_not_success() {
    let mut r = Rig::new();
    r.to("green");
    let Poll::Finished { result, .. } = done(None) else {
        unreachable!()
    };
    r.finish(Poll::Finished {
        process_success: true,
        completion: false,
        result,
    });
    for _ in 0..4 {
        r.tick();
    }
    assert_eq!(r.starts("review"), 0);
    r.alert();
    assert!(!r.git.evidence.iter().any(|(step, _, _)| step == "green"));
}
#[test]
fn hub_stall_alert_is_deduplicated_and_does_not_stop_work() {
    let mut r = Rig::new();
    r.to("green");
    r.clock.0 += 30 * 60;
    r.tick();
    r.alert();
    assert_eq!(r.run.status, Status::Running);
    assert!(r.agent.stops.is_empty());
    let count = r.tracker.alerts.len();
    for _ in 0..5 {
        r.tick();
    }
    assert_eq!(r.tracker.alerts.len(), count);
    r.finish(done(None));
    r.to("review");
}
#[test]
fn hub_different_family_is_required_before_review_start() {
    let mut r = Rig::new();
    r.agent.same_family = true;
    r.to("green");
    r.finish(done(None));
    for _ in 0..5 {
        r.tick();
    }
    assert_eq!(r.starts("review"), 0);
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
}
#[test]
fn hub_concurrency_limit_wait_is_visible_and_recovers() {
    let mut r = Rig::new();
    r.agent.unavailable = true;
    for _ in 0..4 {
        r.tick();
    }
    assert!(r.agent.starts.is_empty());
    r.alert();
    r.agent.unavailable = false;
    r.to("plan");
    assert_eq!(r.starts("plan"), 1);
}
#[test]
fn hub_code_host_failure_alerts_and_manual_retry_works() {
    let mut r = Rig::new();
    r.to("review");
    r.host.fail = true;
    r.finish(done(Some("clean")));
    for _ in 0..5 {
        r.tick();
    }
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
    assert_eq!(r.host.opened, 1);
    r.host.fail = false;
    retry(&mut r.run, "logged in").unwrap();
    r.to("merge");
    assert_eq!(r.host.opened, 2);
}
#[test]
fn hub_scope_change_requires_fresh_owner_go() {
    let mut r = Rig::new();
    r.to("green");
    r.finish(blocked("scope_change"));
    r.to("plan");
    r.finish(done(None));
    r.to("owner_go");
    for _ in 0..5 {
        r.tick();
    }
    assert_eq!(r.run.status, Status::Gate);
    assert_eq!(r.starts("green"), 1);
    assert_eq!(r.run.gate_requests["owner_go"], 2);
    r.approve();
    r.to("green");
    assert_eq!(r.starts("green"), 2);
}

#[test]
fn hub_review_requires_known_verdict() {
    for verdict in [None, Some("banana")] {
        let mut r = Rig::new();
        r.to("review");
        r.finish(done(verdict));
        assert_eq!(r.run.status, Status::Blocked);
        assert_eq!(r.run.block.as_ref().unwrap().kind, BlockKind::Unknown);
        r.alert();
        assert_eq!(r.host.opened, 0);
    }
}

#[test]
fn hub_failed_alert_delivery_is_retried_without_restarting_work() {
    let mut r = Rig::new();
    r.to("green");
    let delivered_before = r.tracker.alerts.len();
    r.tracker.fail_alerts = 1;
    let handle = r.run.active.as_ref().unwrap().handle.clone();
    r.agent
        .polls
        .entry(handle)
        .or_default()
        .push_back(blocked("capability"));
    let result = advance(
        &mut r.run,
        &r.workflow,
        &mut Dependencies {
            agent: &mut r.agent,
            tracker: &mut r.tracker,
            code_host: &mut r.host,
            git: &mut r.git,
            clock: &r.clock,
        },
    );
    assert!(result.is_err());
    assert_eq!(r.tracker.alerts.len(), delivered_before);
    r.tick();
    assert_eq!(r.tracker.alerts.len(), delivered_before + 1);
    assert_eq!(r.starts("green"), 1);
    for _ in 0..5 {
        r.tick();
    }
    assert_eq!(r.tracker.alerts.len(), delivered_before + 1);
    assert_eq!(r.run.status, Status::Blocked);
}

#[test]
fn hub_result_requires_every_schema_field() {
    let Poll::Finished {
        result: Some(text), ..
    } = done(None)
    else {
        unreachable!()
    };
    for field in [
        "schema",
        "status",
        "summary",
        "artifacts",
        "block",
        "verdict",
        "questions",
    ] {
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value.as_object_mut().unwrap().remove(field);
        let mut r = Rig::new();
        r.to("green");
        r.finish(Poll::Finished {
            process_success: true,
            completion: true,
            result: Some(value.to_string()),
        });
        assert_eq!(
            r.run.status,
            Status::Blocked,
            "omitted schema field: {field}"
        );
        assert_eq!(r.run.block.as_ref().unwrap().kind, BlockKind::Unknown);
        r.alert();
        assert_eq!(r.starts("review"), 0);
    }
}
#[test]
fn hub_result_rejects_unknown_fields() {
    for nested in [false, true] {
        let poll = if nested {
            blocked("capability")
        } else {
            done(None)
        };
        let Poll::Finished {
            result: Some(text), ..
        } = poll
        else {
            unreachable!()
        };
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        if nested {
            value["block"]["extra"] = serde_json::json!(true);
        } else {
            value["extra"] = serde_json::json!(true);
        }
        let mut r = Rig::new();
        r.to("green");
        r.finish(Poll::Finished {
            process_success: true,
            completion: true,
            result: Some(value.to_string()),
        });
        assert_eq!(r.run.status, Status::Blocked);
        assert_eq!(r.run.block.as_ref().unwrap().kind, BlockKind::Unknown);
        r.alert();
    }
}
#[test]
fn hub_non_review_verdict_is_unknown_block() {
    for verdict in ["clean", "changes_needed"] {
        let mut r = Rig::new();
        r.to("green");
        r.finish(done(Some(verdict)));
        assert_eq!(r.run.status, Status::Blocked);
        assert_eq!(r.run.block.as_ref().unwrap().kind, BlockKind::Unknown);
        r.alert();
        assert_eq!(r.starts("review"), 0);
    }
}
#[test]
fn hub_agent_start_failure_retries_are_bounded() {
    let mut r = Rig::new();
    r.agent.start_errors = (0..3)
        .map(|_| Block::new(BlockKind::Transient, "process spawn failed"))
        .collect();
    for _ in 0..8 {
        r.tick();
    }
    assert_eq!(r.starts("plan"), 3);
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
    assert!(r.run.active.is_none());
}
#[test]
fn hub_agent_poll_error_retains_unsettled_worker() {
    let mut r = Rig::new();
    r.to("green");
    r.agent.poll_error = true;
    for _ in 0..5 {
        r.tick();
    }
    assert_eq!(r.run.status, Status::Blocked);
    assert!(r.run.unsettled);
    assert!(r.run.active.is_some());
    r.alert();
    assert_eq!(r.starts("green"), 1);
    assert!(retry(&mut r.run, "connection failed").is_err());
    assert!(override_step(&mut r.run, &r.workflow, "assume dead").is_err());
}
#[test]
fn hub_agent_stop_error_cannot_resend_worker() {
    let mut r = Rig::new();
    r.to("green");
    r.agent.stop_error = true;
    r.clock.0 += 90 * 60;
    for _ in 0..5 {
        r.tick();
    }
    assert_eq!(r.run.status, Status::Blocked);
    assert!(r.run.unsettled);
    assert!(r.run.active.is_some());
    r.alert();
    assert_eq!(r.starts("green"), 1);
    assert_eq!(r.agent.stops.len(), 1);
    assert!(retry(&mut r.run, "try again").is_err());
}
#[test]
fn hub_git_evidence_failure_cannot_release_successor() {
    let mut r = Rig::new();
    r.to("red");
    r.git.fail_evidence = true;
    r.finish(done(None));
    for _ in 0..5 {
        r.tick();
    }
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
    assert!(!r.run.freezes.contains_key("red"));
    assert_eq!(r.starts("green"), 0);
}

#[test]
fn hub_transient_git_check_failures_retry_then_alert() {
    let mut r = Rig::new();
    r.to("green");
    r.git.check_error = Some(BlockKind::Transient);
    for _ in 0..20 {
        if r.run.status == Status::Blocked {
            break;
        }
        if r.run.status == Status::Running {
            r.finish(done(None));
        } else {
            r.tick();
        }
    }
    assert_eq!(
        r.git
            .checks
            .iter()
            .filter(|(step, _, _)| step == "green")
            .count(),
        3
    );
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
    assert_eq!(r.starts("review"), 0);
}
#[test]
fn hub_transient_git_evidence_failures_retry_then_alert() {
    let mut r = Rig::new();
    r.to("green");
    r.git.evidence_error = Some(BlockKind::Transient);
    for _ in 0..20 {
        if r.run.status == Status::Blocked {
            break;
        }
        if r.run.status == Status::Running {
            r.finish(done(None));
        } else {
            r.tick();
        }
    }
    assert_eq!(
        r.git
            .evidence
            .iter()
            .filter(|(step, _, _)| step == "green")
            .count(),
        3
    );
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
    assert_eq!(r.starts("review"), 0);
    assert!(!r.run.freezes.contains_key("green"));
}
#[test]
fn hub_transient_gate_observation_failures_retry_then_alert() {
    let mut r = Rig::new();
    r.to("owner_go");
    r.tracker.gate_error = true;
    let observed_before = r.tracker.gate_polls;
    for _ in 0..10 {
        r.tick();
    }
    assert_eq!(r.tracker.gate_polls, observed_before + 3);
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
    assert_eq!(r.starts("red"), 0);
}

#[test]
fn hub_gate_success_resets_retry_allowance_for_later_owner_go() {
    let mut r = Rig::new();
    r.to("owner_go");
    r.tracker.gate_errors = 1;
    r.tick();
    r.approve();
    r.to("green");
    r.finish(blocked("scope_change"));
    r.to("plan");
    r.finish(done(None));
    r.to("owner_go");
    let observed_before = r.tracker.gate_polls;
    let red_before = r.starts("red");
    r.tracker.gate_error = true;
    for _ in 0..10 {
        r.tick();
    }
    assert_eq!(
        r.tracker.gate_polls,
        observed_before + 3,
        "each successful gate visit must reset its transient retry allowance"
    );
    assert_eq!(r.run.status, Status::Blocked);
    r.alert();
    assert_eq!(r.starts("red"), red_before);
}
#[test]
fn hub_open_pr_success_clears_transient_retry_counter() {
    let mut r = Rig::new();
    r.to("review");
    r.host.transient_errors = 1;
    r.finish(done(Some("clean")));
    r.to("merge");
    assert_eq!(r.host.opened, 2);
    assert_eq!(r.run.pr.as_deref(), Some("PR-1"));
    assert!(
        !r.run.retries.contains_key("pr"),
        "successful built-in actions must clear their transient retry counter"
    );
}
