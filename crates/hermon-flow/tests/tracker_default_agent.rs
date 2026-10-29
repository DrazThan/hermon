//! Independent fail-closed owner-answer contract for adapters without support.
use hermon_flow::hub::{Agent, Block, BlockKind, Poll, Settlement, StartRequest};
use std::collections::BTreeMap;
#[derive(Default)]
struct UnsupportedAgent {
    starts: usize,
}
impl Agent for UnsupportedAgent {
    fn family(&self, _: &str) -> String {
        "unsupported-fixture".into()
    }
    fn available(&self, _: &str) -> bool {
        true
    }
    fn start(&mut self, _: &StartRequest) -> Result<String, Block> {
        self.starts += 1;
        Ok("fresh-session-without-answer".into())
    }
    fn poll(&mut self, _: &str) -> Result<Poll, Block> {
        Ok(Poll::Running)
    }
    fn stop(&mut self, _: &str) -> Result<Settlement, Block> {
        Ok(Settlement::Confirmed)
    }
}
#[test]
fn agent_without_owner_answer_support_cannot_silently_start_fresh() {
    let mut agent = UnsupportedAgent::default();
    let request = StartRequest {
        run_id: "run-121".into(),
        step: "work".into(),
        bot: "unsupported".into(),
        number: 2,
        freezes: BTreeMap::new(),
    };
    let block = agent
        .resume_after_answer("previous-settled-handle", &request, "Use blue.")
        .expect_err(
            "an adapter must explicitly carry owner context before resuming or starting fresh",
        );
    assert_eq!(block.kind, BlockKind::Capability);
    assert!(!block.reason.trim().is_empty());
    assert_eq!(
        agent.starts, 0,
        "unsupported owner-answer delivery must not launch a context-free replacement"
    );
}
