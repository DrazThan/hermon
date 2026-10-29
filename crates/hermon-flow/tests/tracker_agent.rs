//! Independently authored supplemental Slice 6 process resume acceptance.
#![cfg(unix)]
use hermon_flow::{
    agent::{BotConfig, CommandConfig, ProcessAgent, Protocol},
    hub::{Agent, Poll, StartRequest},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
const WORKER: &str = env!("CARGO_BIN_EXE_hermon-flow-worker");
const SCRIPT: &str = r#"import json,sys
from pathlib import Path
task=Path(sys.argv[1])
(task.parent/'observed.json').write_text(json.dumps({'extra':sys.argv[2:],'stdin':sys.stdin.read(),'task':task.read_text()}))
(task.parent/'result.json').write_text(json.dumps({'schema':1,'status':'done','summary':'fixture done','artifacts':[],'block':None,'verdict':None,'questions':[]}))
print(json.dumps({'type':'thread.started','thread_id':'persisted-session-121'}),flush=True)
print(json.dumps({'type':'turn.completed'}),flush=True)
"#;
fn request(root: &Path, number: u32) -> (StartRequest, PathBuf) {
    let directory = root.join("run-121/steps/work").join(number.to_string());
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("task.md"),
        if number == 1 {
            "Original instruction: choose owner color.\n"
        } else {
            "Continue the original work.\n"
        },
    )
    .unwrap();
    (
        StartRequest {
            run_id: "run-121".into(),
            step: "work".into(),
            bot: "fixture".into(),
            number,
            freezes: BTreeMap::new(),
        },
        directory,
    )
}
fn agent(root: &Path, workdir: &Path, script: &Path, resumable: bool) -> ProcessAgent {
    let argv = vec![
        "python3".into(),
        script.to_str().unwrap().into(),
        "{task}".into(),
    ];
    let config = CommandConfig {
        start: argv.clone(),
        resume: resumable.then(|| [argv, vec!["resume".into(), "{session}".into()]].concat()),
        task_stdin: true,
        protocol: if resumable {
            Protocol::CodexJson
        } else {
            Protocol::ResultFile
        },
        family: "fixture".into(),
    };
    ProcessAgent::new(
        root,
        workdir,
        WORKER,
        BTreeMap::from([("fixture".into(), BotConfig::Command(config))]),
        Duration::from_secs(5),
    )
    .unwrap()
}
fn finish(agent: &mut ProcessAgent, handle: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match agent.poll(handle).unwrap() {
            Poll::Running => {
                assert!(
                    Instant::now() < deadline,
                    "owned synthetic process must settle"
                );
                thread::sleep(Duration::from_millis(20));
            }
            Poll::Finished {
                process_success,
                completion,
                ..
            } => {
                assert!(process_success && completion);
                return;
            }
            other => panic!("unexpected process observation {other:?}"),
        }
    }
}
fn scenario(resumable: bool) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("records");
    let work = dir.path().join("work");
    fs::create_dir_all(&work).unwrap();
    let script = dir.path().join("agent.py");
    fs::write(&script, SCRIPT).unwrap();
    let (first, _) = request(&root, 1);
    let mut initial = agent(&root, &work, &script, resumable);
    let previous = initial.start(&first).unwrap();
    finish(&mut initial, &previous);
    if resumable {
        assert_eq!(
            initial.attempt(&previous).unwrap().session_id.as_deref(),
            Some("persisted-session-121")
        );
    }
    drop(initial);
    let (second, second_dir) = request(&root, 2);
    let mut restarted = agent(&root, &work, &script, resumable);
    let handle = restarted
        .resume_after_answer(&previous, &second, "Use blue; literal $() and {{braces}}.")
        .unwrap();
    finish(&mut restarted, &handle);
    let observed: Value =
        serde_json::from_slice(&fs::read(second_dir.join("observed.json")).unwrap()).unwrap();
    assert!(
        observed["task"]
            .as_str()
            .unwrap()
            .contains("Use blue; literal $() and {{braces}}."),
        "the owner answer must enter the new task: {observed}"
    );
    assert!(
        observed["task"]
            .as_str()
            .unwrap()
            .contains("Continue the original work.")
    );
    assert_eq!(
        observed["stdin"], observed["task"],
        "task-file stdin receives complete owner context"
    );
    if resumable {
        assert_eq!(
            observed["extra"],
            serde_json::json!(["resume", "persisted-session-121"]),
            "resume must use the prior explicit session"
        );
    } else {
        assert_eq!(observed["extra"], serde_json::json!([]));
        assert!(
            observed["task"]
                .as_str()
                .unwrap()
                .contains("Original instruction: choose owner color."),
            "a fresh session must receive prior context"
        );
    }
    let before = fs::read(second_dir.join("task.md")).unwrap();
    assert_eq!(
        restarted
            .resume_after_answer(&previous, &second, "Use blue; literal $() and {{braces}}.")
            .unwrap(),
        handle
    );
    assert_eq!(
        fs::read(second_dir.join("task.md")).unwrap(),
        before,
        "reconciliation must not append the same owner answer twice"
    );
}
#[test]
fn agent_owner_answer_resumes_explicit_session_after_restart() {
    scenario(true)
}
#[test]
fn agent_owner_answer_fresh_fallback_carries_context() {
    scenario(false)
}
