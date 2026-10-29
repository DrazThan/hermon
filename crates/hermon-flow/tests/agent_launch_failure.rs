//! A known failure before spawning must not reserve phantom worker capacity.
#![cfg(unix)]
use hermon_flow::{agent::*, hub::*};
use std::{
    collections::BTreeMap,
    fs, thread,
    time::{Duration, Instant},
};

#[test]
fn agent_known_logfile_failure_can_be_retried_after_repair() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("runs");
    let workdir = dir.path().join("work");
    let attempt = root.join("review/steps/work/1");
    fs::create_dir_all(&attempt).unwrap();
    fs::create_dir_all(&workdir).unwrap();
    fs::write(attempt.join("task.md"), "task").unwrap();
    fs::create_dir(attempt.join("worker.log")).unwrap();
    let request = StartRequest {
        run_id: "review".into(),
        step: "work".into(),
        bot: "fixture".into(),
        number: 1,
        freezes: BTreeMap::new(),
    };
    let config = CommandConfig {
        start: vec![
            "python3".into(),
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/agent.py").into(),
            "done".into(),
            "{task}".into(),
            "{workdir}".into(),
        ],
        resume: None,
        task_stdin: false,
        protocol: Protocol::ResultFile,
        family: "fixture".into(),
    };
    let mut agent = ProcessAgent::new(
        &root,
        &workdir,
        env!("CARGO_BIN_EXE_hermon-flow-worker"),
        BTreeMap::from([("fixture".into(), BotConfig::Command(config))]),
        Duration::from_secs(3),
    )
    .unwrap();
    assert!(
        agent.start(&request).is_err(),
        "logfile directory prevents worker spawn"
    );
    fs::remove_dir(attempt.join("worker.log")).unwrap();
    assert!(
        agent.available("fixture"),
        "a known no-launch failure must not retain capacity"
    );
    let handle = agent
        .start(&request)
        .expect("repaired logfile must permit retry");
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        match agent.poll(&handle).unwrap() {
            Poll::Running => {}
            Poll::Finished {
                process_success,
                completion,
                ..
            } => {
                assert!(process_success && completion);
                break;
            }
            observed => panic!("expected repaired launch completion, got {observed:?}"),
        }
        assert!(Instant::now() < deadline, "repaired launch never started");
        thread::sleep(Duration::from_millis(20));
    }
}
