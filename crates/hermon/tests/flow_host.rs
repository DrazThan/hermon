//! Independent executable-level host acceptance for issue #124.
use std::process::Command;

#[cfg(unix)]
#[test]
fn foreground_hub_advances_queued_run_with_project_environment() {
    use hermon_flow::{
        control::{RunRecord, save},
        hub::{Event, Run, Status},
        intake::hash,
    };
    use serde_json::{Value, json};
    use std::{
        collections::BTreeMap,
        fs,
        io::{Read, Write},
        net::TcpListener,
        os::unix::fs::PermissionsExt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    let server = thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            let (mut stream, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("accept tracker fixture: {error}"),
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = vec![];
            let mut buffer = [0; 4096];
            let payload = loop {
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0, "incomplete tracker HTTP request");
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let length: usize = header
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse()
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + length])
                            .unwrap();
                    }
                }
            };
            let id = payload["variables"]["input"]["id"]
                .as_str()
                .unwrap_or("comment");
            let response = serde_json::to_string(&json!({"data":{
                "commentCreate":{"success":true,"comment":{"id":id}},
                "comment":null,
                "issue":{"id":"TICKET-124","title":"Fixture task","description":"Host test",
                    "comments":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}
            }}))
            .unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
        }
    });
    struct ServerGuard(Arc<AtomicBool>, Option<thread::JoinHandle<()>>);
    impl Drop for ServerGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
            if let Some(join) = self.1.take() {
                join.join().unwrap();
            }
        }
    }
    let _server = ServerGuard(stopped, Some(server));
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("fixture-agent-124");
    fs::write(&script, r##"#!/bin/sh
set -eu
test "$FLOW_HOST_FIXTURE" = "project-environment"
test -s "$1"
printf '%s' '{"schema":1,"status":"done","summary":"fixture completed","artifacts":[],"block":null,"verdict":null,"questions":[]}' > "${1%/*}/result.json"
"##).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let project = root.path().join("project.json");
    fs::write(&project, serde_json::to_vec(&json!({
        "repo":root.path(),"worktree_root":root.path().join("worktrees"),
        "branch_prefix":"flow","base":"main","start":"work",
        "env":{"PATH":format!("{}:/bin:/usr/bin",bin.display()),"FLOW_HOST_FIXTURE":"project-environment"},
        "bots":{"worker":{"Command":{"start":["fixture-agent-124","{task}"],"resume":null,
            "task_stdin":false,"protocol":"ResultFile","family":"fixture"}}},
        "commands":{},"paths":{},"tracker":{"endpoint":endpoint,"token_env":"HERMON_HOST_TEST_TOKEN",
            "owner_ids":["owner"],"owner_mention":"@owner"},
        "code_host":{"repo":"fixture/project","remote":"origin"},"templates":null
    })).unwrap()).unwrap();
    let data = root.path().join("data");
    let run_dir = data.join("runs/TICKET-124");
    fs::create_dir_all(&run_dir).unwrap();
    let source = "version: 1\nname: host-fixture\nsteps:\n  work:\n    bot: worker\n";
    let mut run = Run::new("TICKET-124", "work");
    run.events.push(Event {
        kind: "intake".into(),
        step: "work".into(),
        reason: "queued".into(),
    });
    let record = RunRecord {
        schema: 1,
        ticket: "TICKET-124".into(),
        workflow_name: "host-fixture".into(),
        workflow_hash: hash(source),
        workflow_source: source.into(),
        template_hashes: BTreeMap::new(),
        templates: BTreeMap::new(),
        project,
        worktree: root.path().into(),
        base: "main".into(),
        base_commit: "fixture".into(),
        branch: "flow/TICKET-124".into(),
        paused: false,
        cancelled: false,
        run,
        notices: vec![],
        started: true,
    };
    save(&run_dir, &record).unwrap();
    let tick = || {
        let out = Command::new(env!("CARGO_BIN_EXE_hermon"))
            .args(["hub", "--once", "--no-notify", "--data-dir"])
            .arg(&data)
            .env("HERMON_HOST_TEST_TOKEN", "synthetic-fixture-token")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<RunRecord>(&fs::read(run_dir.join("run.json")).unwrap()).unwrap()
    };
    let mut current = tick();
    assert_eq!(
        current.run.status,
        Status::Running,
        "queued run must dispatch"
    );
    assert_eq!(current.run.attempts.len(), 1);
    let deadline = Instant::now() + Duration::from_secs(10);
    while current.run.status == Status::Running && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
        current = tick();
    }
    assert_eq!(
        current.run.status,
        Status::Done,
        "host must persist agent completion: {:?}",
        current.run.block
    );
    assert_eq!(
        current.run.attempts.len(),
        1,
        "restart does not duplicate dispatch"
    );
    let journal = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    for kind in ["started", "step_done", "done"] {
        assert!(
            journal
                .lines()
                .any(|line| serde_json::from_str::<Value>(line).unwrap()["kind"] == kind),
            "missing {kind}: {journal}"
        );
    }
}

#[test]
fn foreground_hub_once_accepts_empty_data_directory() {
    let root = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_hermon"))
        .args(["hub", "--once", "--data-dir"])
        .arg(root.path())
        .arg("--no-notify")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new(env!("CARGO_BIN_EXE_hermon"))
        .args(["hub", "--once", "--data-dir"])
        .arg(root.path())
        .arg("--no-notify")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "one-shot exit releases the single-hub lock"
    );
}

#[test]
fn foreground_hub_exposes_documented_control_flags() {
    let out = Command::new(env!("CARGO_BIN_EXE_hermon"))
        .args(["hub", "--help"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let help = String::from_utf8_lossy(&out.stdout);
    for flag in ["--data-dir", "--once", "--no-notify"] {
        assert!(help.contains(flag), "missing {flag} in {help}");
    }
}
