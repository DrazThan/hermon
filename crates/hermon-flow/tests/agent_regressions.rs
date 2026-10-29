//! Independent supplemental RED regressions from production review.
#![cfg(unix)]
use hermon_flow::{agent::*, hub::*};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

const WORKER: &str = env!("CARGO_BIN_EXE_hermon-flow-worker");
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/agent.py");

fn make_agent(root: &Path, workdir: &Path, argv: Vec<String>) -> ProcessAgent {
    let bot = CommandConfig {
        start: argv,
        resume: None,
        task_stdin: false,
        protocol: Protocol::ResultFile,
        family: "fixture".into(),
    };
    ProcessAgent::new(
        root,
        workdir,
        WORKER,
        BTreeMap::from([("fixture".into(), BotConfig::Command(bot))]),
        Duration::from_secs(3),
    )
    .unwrap()
}
fn prepare(root: &Path) -> (StartRequest, PathBuf) {
    let directory = root.join("review/steps/work/1");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("task.md"), "new attempt").unwrap();
    (
        StartRequest {
            run_id: "review".into(),
            step: "work".into(),
            bot: "fixture".into(),
            number: 1,
            freezes: BTreeMap::new(),
        },
        directory,
    )
}
fn finished(agent: &mut ProcessAgent, handle: &str) -> Poll {
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let observed = agent.poll(handle).unwrap();
        if !matches!(observed, Poll::Running) {
            return observed;
        }
        assert!(Instant::now() < deadline, "review fixture did not finish");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn agent_preexisting_result_is_not_new_completion_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("runs");
    let workdir = dir.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    let (request, directory) = prepare(&root);
    fs::write(
        directory.join("result.json"),
        serde_json::json!({"schema":1,"status":"done","summary":"old result","artifacts":[],
            "block":null,"verdict":null,"questions":[]})
        .to_string(),
    )
    .unwrap();
    let mut agent = make_agent(
        &root,
        &workdir,
        vec![
            "python3".into(),
            FIXTURE.into(),
            "missing".into(),
            "{task}".into(),
            "{workdir}".into(),
        ],
    );
    let handle = agent.start(&request).unwrap();
    match finished(&mut agent, &handle) {
        Poll::Finished {
            completion, result, ..
        } => {
            assert!(
                !completion,
                "pre-existing result cannot complete a new process"
            );
            assert!(
                result.is_none(),
                "old result must not be delivered to the hub"
            );
        }
        observed => panic!("expected settled process: {observed:?}"),
    }
}

#[test]
fn agent_relative_executable_resolves_in_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("runs");
    let workdir = dir.path().join("work tree");
    fs::create_dir_all(&workdir).unwrap();
    let (request, _) = prepare(&root);
    let wrapper = workdir.join("review-agent");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec python3 '{}' done \"$@\"\n",
            FIXTURE.replace('\'', "'\\''")
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let mut agent = make_agent(
        &root,
        &workdir,
        vec!["./review-agent".into(), "{task}".into(), "{workdir}".into()],
    );
    let handle = agent
        .start(&request)
        .expect("worktree-relative executable must launch");
    match finished(&mut agent, &handle) {
        Poll::Finished {
            process_success,
            completion,
            ..
        } => assert!(process_success && completion),
        observed => panic!("expected completion: {observed:?}"),
    }
}
