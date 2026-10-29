//! Independent, frozen Slice 6 acceptance tests. All network peers are local fakes.
use hermon_flow::{
    hub::*,
    store::RunStore,
    templates::{Templates, marker},
    tracker::{LinearConfig, LinearTracker},
    workflow::Workflow,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

// Destroy the old adapter before reopening its durable state. This generic
// helper also supports the initial behavior-free RED stub.
fn restart_boundary<T>(adapter: T) {
    drop(adapter);
}

const OWNER: &str = "[Owner](https://linear.app/test/profiles/owner-id)";
const NAMES: [&str; 7] = [
    "started.md",
    "question.md",
    "go_request.md",
    "answer_ack.md",
    "step_done.md",
    "alert.md",
    "outcome.md",
];
#[derive(Default)]
struct State {
    calls: Vec<(String, Value)>,
    posts: Vec<Value>,
    comments: Vec<Value>,
    pages: BTreeMap<String, Value>,
}
struct Server {
    endpoint: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/graphql", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let state2 = state.clone();
        let stop2 = stop.clone();
        let worker = thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => Self::serve(stream, &state2),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("fake Linear accept: {e}"),
                }
            }
        });
        Self {
            endpoint,
            state,
            stop,
            worker: Some(worker),
        }
    }
    fn serve(mut stream: TcpStream, shared: &Mutex<State>) {
        // Accepted sockets may inherit O_NONBLOCK on macOS. Header reads must
        // wait for the client bytes rather than race the nonblocking accept loop.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = vec![];
        let mut chunk = [0; 4096];
        let (header_end, length) = loop {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0, "incomplete HTTP headers");
            bytes.extend_from_slice(&chunk[..n]);
            if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..index]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                    })
                    .expect("JSON content length");
                break (index + 4, length);
            }
        };
        while bytes.len() < header_end + length {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
        }
        let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
        let request: Value =
            serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
        let query = request["query"].as_str().unwrap();
        let mut state = shared.lock().unwrap();
        state.calls.push((headers, request.clone()));
        let response = if query.contains("commentCreate") {
            let input = request["variables"]["input"].clone();
            state.posts.push(input.clone());
            json!({"data":{"commentCreate":{"success":true,"comment":{"id":format!("thread-{}", state.posts.len()),"body":input["body"],"user":{"id":"central-app"},"parent":null}}}})
        } else if query.contains("comments") {
            let after = request["variables"]["after"].as_str().unwrap_or("");
            let page = state.pages.get(after).cloned().unwrap_or_else(|| {
                let since = request["variables"]["filter"]["createdAt"]["gte"]
                    .as_str()
                    .unwrap_or("");
                let mut nodes = state
                    .comments
                    .iter()
                    .filter(|c| c["createdAt"].as_str().unwrap() >= since)
                    .cloned()
                    .collect::<Vec<_>>();
                nodes.sort_by(|a, b| b["createdAt"].as_str().cmp(&a["createdAt"].as_str()));
                // A terminal Relay cursor points past all existing (older) rows.
                if !after.is_empty() {
                    nodes.clear();
                }
                page(nodes, Some("terminal-relay"), false)
            });
            json!({"data":{"issue":{"id":"issue-1","comments":page},"comments":page}})
        } else if query.contains("issues") {
            json!({"data":{"issues":{"nodes":[{"id":"issue-1","title":"Fixture issue","description":"Synthetic description"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}})
        } else {
            json!({"data":{"issue":{"id":"issue-1","title":"Fixture issue","description":"Synthetic description"},"viewer":{"id":"central-app","isMe":true}}})
        };
        drop(state);
        let body = response.to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    }
    fn tracker(&self, dir: &std::path::Path) -> LinearTracker {
        LinearTracker::new(
            LinearConfig {
                endpoint: self.endpoint.clone(),
                token: "central-test-token".into(),
                issue_id: "issue-1".into(),
                owner_ids: BTreeSet::from(["owner-id".into(), "second-owner".into()]),
                owner_mention: OWNER.into(),
            },
            Templates::load(None, None).unwrap(),
            dir,
        )
        .unwrap()
    }
    fn set_page(&self, after: &str, comments: Vec<Value>, end: Option<&str>, more: bool) {
        if more || after == "page-two" {
            self.state
                .lock()
                .unwrap()
                .pages
                .insert(after.into(), page(comments, end, more));
        } else {
            self.state.lock().unwrap().comments = comments;
        }
    }
    fn posts(&self) -> Vec<Value> {
        self.state.lock().unwrap().posts.clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn page(nodes: Vec<Value>, end: Option<&str>, more: bool) -> Value {
    json!({"nodes":nodes,"pageInfo":{"endCursor":end,"hasNextPage":more}})
}
fn reply(id: &str, body: &str, owner: &str, parent: Option<&str>) -> Value {
    json!({"id":id,"body":body,"createdAt":"2026-10-29T12:00:00.000Z","user":{"id":owner},"parent":parent.map(|id|json!({"id":id}))})
}
fn values() -> BTreeMap<String, String> {
    [
        "workflow",
        "base_branch",
        "branch",
        "run_status_cmd",
        "bot",
        "step",
        "question",
        "technical",
        "options",
        "artifact_links",
        "answer",
        "summary",
        "reason",
        "pr",
        "merge_commit",
        "docs",
    ]
    .into_iter()
    .map(|key| (key.into(), format!("sample-{key}")))
    .collect()
}
fn auth(headers: &str) -> Option<&str> {
    headers.lines().find_map(|line| {
        line.split_once(':')
            .filter(|(key, _)| key.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.trim())
    })
}

#[test]
fn tracker_post_marker_and_mention() {
    let server = Server::new();
    let dir = tempfile::tempdir().unwrap();
    let mut tracker = server.tracker(dir.path());
    for (index, kind) in NAMES.iter().enumerate() {
        let request = format!("request-{index}");
        let id = tracker
            .post_comment(kind, "run-121", &request, &values())
            .unwrap();
        assert_eq!(id, format!("thread-{}", index + 1));
        let body = server.posts()[index]["body"].as_str().unwrap().to_owned();
        assert!(
            body.ends_with(&format!("<!-- hermon:run=run-121;request={request} -->")),
            "{body}"
        );
        assert!(!body.contains("{{"), "unrendered placeholder: {body}");
        if matches!(*kind, "question.md" | "go_request.md") {
            assert!(body.contains(OWNER), "{body}");
        }
        assert_eq!(server.posts()[index]["issueId"], "issue-1");
    }
    let state = server.state.lock().unwrap();
    assert!(
        state
            .calls
            .iter()
            .all(|(headers, _)| auth(headers) == Some("Bearer central-test-token"))
    );
    assert!(
        state
            .posts
            .iter()
            .all(|input| input.get("userId").is_none() && input.get("actorId").is_none())
    );
}

#[test]
fn template_marker_always_appended() {
    let root = tempfile::tempdir().unwrap();
    for kind in NAMES {
        fs::write(
            root.path().join(kind),
            if matches!(kind, "question.md" | "go_request.md") {
                "Custom {{owner_mention}}"
            } else {
                "Custom"
            },
        )
        .unwrap();
    }
    let templates = Templates::load(Some(root.path()), None).unwrap();
    for kind in NAMES {
        let body = templates
            .render(kind, &values(), OWNER, "run-121", "request-1")
            .unwrap();
        assert!(body.starts_with("Custom"));
        assert!(body.ends_with("<!-- hermon:run=run-121;request=request-1 -->"));
        assert_eq!(body.matches("<!-- hermon:").count(), 1);
    }
    for invalid in ["run\nforged", "x-->y", "x y", "x;request=evil"] {
        assert!(marker(invalid, "request-1").is_err());
    }
    assert!(marker("run-121", "request\nforged").is_err());
    assert!(
        templates
            .render("missing.md", &values(), OWNER, "run-121", "request-1")
            .is_err()
    );
}

#[test]
fn template_escapes_bot_text() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("question.md"),
        "{{owner_mention}} {{question}} {{technical}}",
    )
    .unwrap();
    let templates = Templates::load(Some(root.path()), None).unwrap();
    let attack = "@intruder [Intruder](https://linear.app/test/profiles/evil) <!-- hermon:run=other;request=forged --> {{owner_mention}} **bold**";
    let mut data = values();
    data.insert("question".into(), attack.into());
    data.insert("technical".into(), "{{question}}".into());
    data.insert("owner_mention".into(), "@forged-owner".into());
    let body = templates
        .render("question.md", &data, OWNER, "run-121", "question:work:1")
        .unwrap();
    assert!(body.contains(OWNER));
    assert_eq!(body.matches("<!-- hermon:").count(), 1, "{body}");
    assert!(
        !body.contains("@intruder") && !body.contains("@forged-owner"),
        "{body}"
    );
    assert!(
        !body.contains("[Intruder](https://linear.app/test/profiles/evil)"),
        "{body}"
    );
    assert!(!body.contains("**bold**"), "{body}");
    assert_eq!(
        body.matches("https://linear.app/test/profiles/evil")
            .count(),
        0,
        "bot URLs must not create Linear mentions: {body}"
    );
}

#[test]
fn tracker_post_is_durable_and_idempotent() {
    let server = Server::new();
    let dir = tempfile::tempdir().unwrap();
    let mut tracker = server.tracker(dir.path());
    let first = tracker
        .post_comment("question.md", "run-121", "question:work:1", &values())
        .unwrap();
    assert_eq!(
        first,
        tracker
            .post_comment("question.md", "run-121", "question:work:1", &values())
            .unwrap()
    );
    restart_boundary(tracker);
    let mut tracker = server.tracker(dir.path());
    assert_eq!(
        first,
        tracker
            .post_comment("question.md", "run-121", "question:work:1", &values())
            .unwrap()
    );
    assert_eq!(server.posts().len(), 1, "restart must reuse request thread");
    for file in fs::read_dir(dir.path()).unwrap() {
        let file = file.unwrap();
        if file.file_type().unwrap().is_file() {
            assert!(
                !String::from_utf8_lossy(&fs::read(file.path()).unwrap())
                    .contains("central-test-token")
            );
        }
    }
}

#[test]
fn gate_go_exact_owner_reply() {
    for (body, expected) in [
        ("GO", true),
        ("go", false),
        (" GO", false),
        ("GO\n", false),
        ("GO please", false),
        ("**GO**", false),
        ("Looks good", false),
    ] {
        let server = Server::new();
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = server.tracker(dir.path());
        assert!(!tracker.gate_approved("run-121", "owner_go", 1).unwrap());
        assert!(
            server.posts()[0]["body"]
                .as_str()
                .unwrap()
                .contains("<!-- hermon:run=run-121;request=gate:owner_go:1 -->")
        );
        server.set_page(
            "",
            vec![reply("reply-1", body, "owner-id", Some("thread-1"))],
            Some("cursor-1"),
            false,
        );
        assert_eq!(
            tracker.gate_approved("run-121", "owner_go", 1).unwrap(),
            expected,
            "body {body:?}"
        );
        if expected {
            server.set_page("cursor-1", vec![], Some("cursor-1"), false);
            assert!(tracker.gate_approved("run-121", "owner_go", 1).unwrap());
        }
    }
}

#[test]
fn gate_go_ignores_non_owner() {
    let server = Server::new();
    let dir = tempfile::tempdir().unwrap();
    let mut tracker = server.tracker(dir.path());
    assert!(!tracker.gate_approved("run-121", "owner_go", 1).unwrap());
    server.set_page(
        "",
        vec![
            reply("foreign", "GO", "bot", Some("thread-1")),
            reply("other-thread", "GO", "owner-id", Some("thread-other")),
            reply("root", "GO", "owner-id", None),
        ],
        Some("cursor-1"),
        false,
    );
    assert!(!tracker.gate_approved("run-121", "owner_go", 1).unwrap());
    server.set_page(
        "cursor-1",
        vec![reply("authorized", "GO", "second-owner", Some("thread-1"))],
        Some("cursor-2"),
        false,
    );
    assert!(tracker.gate_approved("run-121", "owner_go", 1).unwrap());
    assert!(
        !tracker.gate_approved("run-121", "owner_go", 2).unwrap(),
        "old request cannot approve new gate visit"
    );
    assert_eq!(server.posts().len(), 2);
}

#[test]
fn gate_reply_cursor_survives_restart() {
    let server = Server::new();
    let dir = tempfile::tempdir().unwrap();
    server.set_page(
        "",
        vec![reply("noise", "GO", "bot", Some("thread-1"))],
        Some("cursor-1"),
        false,
    );
    let mut tracker = server.tracker(dir.path());
    assert!(!tracker.gate_approved("run-121", "owner_go", 1).unwrap());
    restart_boundary(tracker);
    server.set_page(
        "cursor-1",
        vec![reply("owner-reply", "GO", "owner-id", Some("thread-1"))],
        Some("cursor-2"),
        false,
    );
    let before = server.state.lock().unwrap().calls.len();
    let mut tracker = server.tracker(dir.path());
    assert!(tracker.gate_approved("run-121", "owner_go", 1).unwrap());
    restart_boundary(tracker);
    let state = server.state.lock().unwrap();
    let polls: Vec<_> = state.calls[before..]
        .iter()
        .filter(|(_, r)| r["query"].as_str().unwrap().contains("comments"))
        .collect();
    assert!(!polls.is_empty());
    assert!(
        polls[0].1["variables"]["after"].is_null(),
        "terminal Relay cursors must not skip newer replies"
    );
    assert_eq!(
        polls[0].1["variables"]["filter"]["createdAt"]["gte"], "2026-10-29T12:00:00.000Z",
        "restart must retain its incremental watermark"
    );
    assert_eq!(state.posts.len(), 1);
    drop(state);
    let mut tracker = server.tracker(dir.path());
    assert!(
        tracker.gate_approved("run-121", "owner_go", 1).unwrap(),
        "accepted reply must survive another restart"
    );
}

#[test]
fn tracker_reads_issue_labels_and_comment_pages() {
    let server = Server::new();
    let dir = tempfile::tempdir().unwrap();
    let mut tracker = server.tracker(dir.path());
    let issue = tracker.read_issue().unwrap();
    assert_eq!(issue.id, "issue-1");
    assert_eq!(issue.title, "Fixture issue");
    assert_eq!(issue.description, "Synthetic description");
    assert_eq!(tracker.find_labeled("ready").unwrap()[0].id, "issue-1");
    server.set_page(
        "known-cursor",
        vec![reply("comment-1", "answer", "owner-id", Some("thread-1"))],
        Some("next-cursor"),
        false,
    );
    let page = tracker.list_comments_since(None).unwrap();
    assert!(
        page.next_cursor.is_some(),
        "polling must return an opaque resume cursor"
    );
    let next = tracker
        .list_comments_since(page.next_cursor.as_deref())
        .unwrap();
    assert_eq!(
        next.comments[0].id, "comment-1",
        "timestamp ties must remain eligible"
    );
    assert_eq!(page.comments[0].id, "comment-1");
    assert_eq!(page.comments[0].body, "answer");
    assert_eq!(page.comments[0].user_id, "owner-id");
    assert_eq!(page.comments[0].parent_id.as_deref(), Some("thread-1"));
    let state = server.state.lock().unwrap();
    assert!(
        state
            .calls
            .iter()
            .any(|(_, r)| r["variables"].to_string().contains("ready")),
        "label must be in GraphQL variables"
    );
}

#[derive(Default)]
struct FakeAgent {
    polls: VecDeque<Poll>,
    resumes: Vec<(String, String)>,
    starts: usize,
}
impl Agent for FakeAgent {
    fn family(&self, _: &str) -> String {
        "fixture".into()
    }
    fn available(&self, _: &str) -> bool {
        true
    }
    fn start(&mut self, _: &StartRequest) -> Result<String, Block> {
        self.starts += 1;
        Ok(format!("session-handle-{}", self.starts))
    }
    fn poll(&mut self, _: &str) -> Result<Poll, Block> {
        Ok(self.polls.pop_front().unwrap_or(Poll::Running))
    }
    fn stop(&mut self, _: &str) -> Result<Settlement, Block> {
        Ok(Settlement::Confirmed)
    }
    fn resume_after_answer(
        &mut self,
        previous_handle: &str,
        request: &StartRequest,
        answer: &str,
    ) -> Result<String, Block> {
        self.resumes.push((previous_handle.into(), answer.into()));
        self.start(request)
    }
}
struct FakeGit;
impl Git for FakeGit {
    fn check(&mut self, _: &str, _: &[String], _: &BTreeMap<String, String>) -> Result<(), Block> {
        Ok(())
    }
    fn evidence(&mut self, _: &str, _: &str, _: u32) -> Result<String, Block> {
        Ok("tag".into())
    }
}
struct FakeHost;
impl CodeHost for FakeHost {
    fn open_pr(&mut self, _: &str) -> Result<String, Block> {
        Ok("pr".into())
    }
    fn merged(&mut self, _: &str) -> Result<bool, Block> {
        Ok(false)
    }
}
struct FakeClock;
impl Clock for FakeClock {
    fn now(&self) -> u64 {
        0
    }
}
#[test]
fn question_answer_resumes_session() {
    let server = Server::new();
    let dir = tempfile::tempdir().unwrap();
    let mut tracker = server.tracker(&dir.path().join("tracker"));
    let workflow = Workflow::from_str(
        "fixture.yaml",
        "version: 1\nname: question-test\nsteps:\n  work:\n    bot: worker\n",
    )
    .unwrap();
    let mut run = Run::new("run-121", "work");
    let mut agent = FakeAgent::default();
    let mut git = FakeGit;
    let mut host = FakeHost;
    let tick = |run: &mut Run,
                agent: &mut FakeAgent,
                tracker: &mut LinearTracker,
                git: &mut FakeGit,
                host: &mut FakeHost| {
        advance(
            run,
            &workflow,
            &mut Dependencies {
                agent,
                tracker,
                git,
                code_host: host,
                clock: &FakeClock,
            },
        )
        .unwrap()
    };
    tick(&mut run, &mut agent, &mut tracker, &mut git, &mut host);
    agent.polls.push_back(Poll::Finished{process_success:true,completion:true,result:Some(json!({"schema":1,"status":"block","summary":"needs answer","artifacts":[],"block":{"kind":"owner_question","reason":"Need owner decision"},"verdict":null,"questions":["Which color should we use?"]}).to_string())});
    tick(&mut run, &mut agent, &mut tracker, &mut git, &mut host);
    assert_eq!(run.status, Status::Blocked);
    assert_eq!(agent.starts, 1);
    let posts = server.posts();
    let question = posts
        .iter()
        .position(|p| {
            p["body"]
                .as_str()
                .unwrap()
                .contains("Which color should we use?")
        })
        .expect("question text must be posted");
    assert!(posts[question]["body"].as_str().unwrap().contains(OWNER));
    let parent = format!("thread-{}", question + 1);
    let store = RunStore::new(dir.path().join("run")).unwrap();
    store.write_run(&run).unwrap();
    restart_boundary(tracker);
    let mut run: Run = store.read_run().unwrap().unwrap();
    let mut tracker = server.tracker(&dir.path().join("tracker"));
    // Another actor and a reply in another thread cannot resume the worker.
    server.set_page(
        "",
        vec![
            reply("fake-answer", "red", "bot", Some(&parent)),
            reply("wrong-thread", "red", "owner-id", Some("elsewhere")),
        ],
        Some("noise-cursor"),
        false,
    );
    tick(&mut run, &mut agent, &mut tracker, &mut git, &mut host);
    assert_eq!(run.status, Status::Blocked);
    assert!(agent.resumes.is_empty());
    server.set_page(
        "noise-cursor",
        vec![reply(
            "owner-answer",
            "Use blue.",
            "owner-id",
            Some(&parent),
        )],
        Some("answer-cursor"),
        false,
    );
    tick(&mut run, &mut agent, &mut tracker, &mut git, &mut host);
    assert_eq!(run.status, Status::Running);
    assert_eq!(
        agent.resumes,
        vec![("session-handle-1".into(), "Use blue.".into())]
    );
    assert_eq!(agent.starts, 2);
    let posts = server.posts();
    assert!(
        posts
            .iter()
            .any(|p| p["body"].as_str().unwrap().contains("Applied your answer")),
        "answer acknowledgement must be posted"
    );
    tick(&mut run, &mut agent, &mut tracker, &mut git, &mut host);
    assert_eq!(agent.resumes.len(), 1, "reply applies once");
}

#[test]
fn gate_paginates_newest_first_without_losing_later_inserts() {
    let server = Server::new();
    let dir = tempfile::tempdir().unwrap();
    let mut newest = reply("newer-noise", "no", "bot", Some("thread-1"));
    newest["createdAt"] = json!("2026-10-29T13:00:00.000Z");
    server.set_page("", vec![newest.clone()], Some("page-two"), true);
    server.set_page(
        "page-two",
        vec![reply("older-noise", "no", "bot", Some("thread-1"))],
        Some("end-old"),
        false,
    );
    let mut tracker = server.tracker(dir.path());
    assert!(!tracker.gate_approved("run-121", "owner_go", 1).unwrap());
    restart_boundary(tracker);
    server.state.lock().unwrap().pages.clear();
    let mut arrived = reply("later-owner", "GO", "owner-id", Some("thread-1"));
    arrived["createdAt"] = json!("2026-10-29T14:00:00.000Z");
    server.set_page("", vec![arrived, newest], Some("end-new"), false);
    let before = server.state.lock().unwrap().calls.len();
    let mut tracker = server.tracker(dir.path());
    assert!(tracker.gate_approved("run-121", "owner_go", 1).unwrap());
    let state = server.state.lock().unwrap();
    let queries = state.calls[before..]
        .iter()
        .filter(|(_, r)| r["query"].as_str().unwrap().contains("comments"))
        .collect::<Vec<_>>();
    assert_eq!(
        queries[0].1["variables"]["filter"]["createdAt"]["gte"], "2026-10-29T13:00:00.000Z",
        "watermark must retain max timestamp across newest-first pages"
    );
    assert!(queries[0].1["variables"]["after"].is_null());
}
