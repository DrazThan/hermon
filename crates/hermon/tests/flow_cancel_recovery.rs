//! Independent regression for crash recovery after durable cancellation intent.
use hermon_flow::{hub::*, workflow::Workflow};
use std::collections::BTreeMap;

struct AgentFixture {
    stops: usize,
    confirmed: bool,
}
impl Agent for AgentFixture {
    fn family(&self, _: &str) -> String {
        "fixture".into()
    }
    fn available(&self, _: &str) -> bool {
        true
    }
    fn start(&mut self, _: &StartRequest) -> Result<String, Block> {
        panic!("cancelled run cannot dispatch work")
    }
    fn poll(&mut self, handle: &str) -> Result<Poll, Block> {
        assert_eq!(handle, "owned-before-crash");
        Ok(Poll::Running)
    }
    fn stop(&mut self, handle: &str) -> Result<Settlement, Block> {
        assert_eq!(handle, "owned-before-crash");
        self.stops += 1;
        Ok(if self.confirmed {
            Settlement::Confirmed
        } else {
            Settlement::Unsettled
        })
    }
}
struct TrackerFixture;
impl Tracker for TrackerFixture {
    fn gate_approved(&mut self, _: &str, _: &str, _: u32) -> Result<bool, Block> {
        panic!("cancelled run cannot poll approval gates")
    }
    fn alert(&mut self, _: &str, _: &Alert) -> anyhow::Result<()> {
        Ok(())
    }
}
struct HostFixture;
impl CodeHost for HostFixture {
    fn open_pr(&mut self, _: &str) -> Result<String, Block> {
        panic!("cancelled run cannot publish a PR")
    }
    fn merged(&mut self, _: &str) -> Result<bool, Block> {
        panic!("cancelled run cannot pass a merge gate")
    }
}
struct GitFixture;
impl Git for GitFixture {
    fn check(&mut self, _: &str, _: &[String], _: &BTreeMap<String, String>) -> Result<(), Block> {
        panic!("cancelled run cannot accept checks")
    }
    fn evidence(&mut self, _: &str, _: &str, _: u32) -> Result<String, Block> {
        panic!("cancelled run cannot write evidence")
    }
}
struct ClockFixture;
impl Clock for ClockFixture {
    fn now(&self) -> u64 {
        10
    }
}
fn cancelled() -> Run {
    let mut run = Run::new("CANCEL-123", "work");
    run.cancelled = true;
    run.status = Status::Blocked;
    run.unsettled = true;
    run.block = Some(Block::new(BlockKind::Capability, "Owner cancelled run"));
    run.active = Some(Attempt {
        step: "work".into(),
        number: 1,
        handle: "owned-before-crash".into(),
        started_at: 0,
        family: "fixture".into(),
        freezes: BTreeMap::new(),
    });
    run
}
fn advance(run: &mut Run, agent: &mut AgentFixture) {
    let workflow = Workflow::from_str(
        "fixture",
        "version: 1\nname: cancel-recovery\nsteps:\n  work:\n    bot: worker\n    next: successor\n  successor:\n    bot: worker\n",
    )
    .unwrap();
    hermon_flow::hub::advance(
        run,
        &workflow,
        &mut Dependencies {
            agent,
            tracker: &mut TrackerFixture,
            code_host: &mut HostFixture,
            git: &mut GitFixture,
            clock: &ClockFixture,
        },
    )
    .unwrap();
}

#[test]
fn cancelled_core_retries_owned_stop_after_crash_before_stop_request() {
    // This is the persisted state if the CLI exits after saving cancellation
    // intent but before reaching the ProcessAgent::stop call.
    let mut run = cancelled();
    let mut agent = AgentFixture {
        stops: 0,
        confirmed: false,
    };
    advance(&mut run, &mut agent);
    assert_eq!(
        agent.stops, 1,
        "durable cancellation must retry the owned stop"
    );
    assert!(run.active.is_some());
    assert!(run.unsettled);
    advance(&mut run, &mut agent);
    assert_eq!(
        agent.stops, 2,
        "an unsettled cancellation retries idempotently"
    );
    assert!(run.cancelled);
    assert_eq!(run.status, Status::Blocked);
    assert_eq!(run.current, "work");
    assert!(!run.events.iter().any(|e| e.kind == "step_done"));
}

#[test]
fn cancelled_core_clears_handle_after_stop_confirms_settlement() {
    let mut run = cancelled();
    let mut agent = AgentFixture {
        stops: 0,
        confirmed: true,
    };
    advance(&mut run, &mut agent);
    assert_eq!(agent.stops, 1);
    assert!(
        run.active.is_none(),
        "confirmed stop must release the old handle"
    );
    assert!(!run.unsettled);
    assert!(run.cancelled);
    assert_eq!(run.status, Status::Blocked);
    assert_eq!(run.current, "work");
    assert!(!run.events.iter().any(|e| e.kind == "step_done"));
}
