//! Independent supplemental review of ambiguous mutations and fail-closed replies.
use hermon_flow::{
    hub::Tracker,
    templates::Templates,
    tracker::{LinearConfig, LinearTracker},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
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
#[derive(Clone, Copy)]
enum Failure {
    LostResponse,
    Graphql,
    Unsuccessful,
    MissingId,
}
struct Data {
    failure: Option<Failure>,
    input: Option<Value>,
    mutations: usize,
    reads: usize,
}
struct Fake {
    url: String,
    data: Arc<Mutex<Data>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Fake {
    fn new(failure: Failure) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/graphql", listener.local_addr().unwrap());
        let data = Arc::new(Mutex::new(Data {
            failure: Some(failure),
            input: None,
            mutations: 0,
            reads: 0,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let data2 = data.clone();
        let stop2 = stop.clone();
        let thread = thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => Self::serve(stream, &data2),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("fake accept {e}"),
                }
            }
        });
        Self {
            url,
            data,
            stop,
            thread: Some(thread),
        }
    }
    fn serve(mut stream: TcpStream, state: &Mutex<Data>) {
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = vec![];
        let mut chunk = [0; 4096];
        let (header, length) = loop {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
            if let Some(i) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                let text = String::from_utf8_lossy(&bytes[..i]);
                let size = text
                    .lines()
                    .find_map(|l| {
                        l.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                break (i + 4, size);
            }
        };
        while bytes.len() < header + length {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
        }
        let request: Value = serde_json::from_slice(&bytes[header..header + length]).unwrap();
        let query = request["query"].as_str().unwrap();
        let mut data = state.lock().unwrap();
        let response = if query.contains("commentCreate") {
            data.mutations += 1;
            let input = request["variables"]["input"].clone();
            match data.failure.take() {
                Some(Failure::LostResponse) => {
                    data.input = Some(input);
                    return;
                }
                Some(Failure::Graphql) => {
                    json!({"errors":[{"message":"secret-central-token"}],"data":{"commentCreate":{"success":true,"comment":{"id":"forged"}}}})
                }
                Some(Failure::Unsuccessful) => {
                    json!({"data":{"commentCreate":{"success":false,"comment":{"id":"forged"}}}})
                }
                Some(Failure::MissingId) => {
                    json!({"data":{"commentCreate":{"success":true,"comment":{}}}})
                }
                None => {
                    data.input = Some(input.clone());
                    json!({"data":{"commentCreate":{"success":true,"comment":{"id":input["id"]}}}})
                }
            }
        } else if query.contains("comments") {
            let parent = data.input.as_ref().unwrap()["id"].clone();
            json!({"data":{"issue":{"comments":{"nodes":[{"id":"reply","body":"GO","createdAt":"2026-10-29T12:00:00.000Z","user":null,"parent":{"id":parent}}],"pageInfo":{"hasNextPage":false,"endCursor":"end"}}}}})
        } else {
            data.reads += 1;
            let comment = data
                .input
                .as_ref()
                .map(|input| json!({"id":input["id"],"body":input["body"]}));
            json!({"data":{"comment":comment}})
        };
        drop(data);
        let body = response.to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
    }
    fn config(&self) -> LinearConfig {
        LinearConfig {
            endpoint: self.url.clone(),
            token: "secret-central-token".into(),
            issue_id: "issue-121".into(),
            owner_ids: BTreeSet::from(["owner".into()]),
            owner_mention: "[Owner](https://linear.app/test/profiles/owner)".into(),
        }
    }
    fn tracker(&self, dir: &std::path::Path) -> LinearTracker {
        LinearTracker::new(self.config(), Templates::load(None, None).unwrap(), dir).unwrap()
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn values() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("workflow".into(), "sdlc".into()),
        ("base_branch".into(), "main".into()),
        ("branch".into(), "run".into()),
        ("run_status_cmd".into(), "hermon status".into()),
    ])
}
#[test]
fn tracker_ambiguous_post_recovers_reserved_comment_after_restart() {
    let fake = Fake::new(Failure::LostResponse);
    let dir = tempfile::tempdir().unwrap();
    {
        let mut tracker = fake.tracker(dir.path());
        assert!(
            tracker
                .post_comment("started.md", "run-121", "started", &values())
                .is_err()
        );
    }
    let reserved = fake.data.lock().unwrap().input.as_ref().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut tracker = fake.tracker(dir.path());
    assert_eq!(
        tracker
            .post_comment("started.md", "run-121", "started", &values())
            .unwrap(),
        reserved
    );
    let data = fake.data.lock().unwrap();
    assert_eq!(
        data.mutations, 1,
        "lost response must not create a duplicate comment"
    );
    assert_eq!(data.reads, 1);
}
#[test]
fn tracker_error_payloads_never_latch_success_or_disclose_token() {
    for failure in [Failure::Graphql, Failure::Unsuccessful, Failure::MissingId] {
        let fake = Fake::new(failure);
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = fake.tracker(dir.path());
        let error = tracker
            .post_comment("started.md", "run-121", "started", &values())
            .unwrap_err();
        assert!(!format!("{error:#}").contains("secret-central-token"));
        let id = tracker
            .post_comment("started.md", "run-121", "started", &values())
            .unwrap();
        assert_ne!(id, "forged");
        assert_eq!(fake.data.lock().unwrap().mutations, 2);
        assert!(
            tracker
                .post_comment(
                    "started.md",
                    "run-121",
                    "started",
                    &BTreeMap::from([
                        ("workflow".into(), "different".into()),
                        ("base_branch".into(), "main".into()),
                        ("branch".into(), "run".into()),
                        ("run_status_cmd".into(), "status".into())
                    ])
                )
                .is_err()
        );
        let mut config = fake.config();
        config.issue_id = "other-issue".into();
        assert!(
            LinearTracker::new(config, Templates::load(None, None).unwrap(), dir.path()).is_err()
        );
        let mut config = fake.config();
        config.owner_ids = BTreeSet::from(["other-owner".into()]);
        assert!(
            LinearTracker::new(config, Templates::load(None, None).unwrap(), dir.path()).is_err()
        );
    }
}
#[test]
fn gate_nullable_author_never_authorizes_go() {
    // Consume the one injected failure with an unrelated post, then allow a GO request.
    let fake = Fake::new(Failure::Graphql);
    let dir = tempfile::tempdir().unwrap();
    let mut tracker = fake.tracker(dir.path());
    assert!(
        tracker
            .post_comment("started.md", "run-121", "started", &values())
            .is_err()
    );
    assert!(!tracker.gate_approved("run-121", "owner_go", 1).unwrap());
}

#[test]
fn template_literal_marker_cannot_replace_appended_authority() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("started.md"),
        "Owner template with copied marker: <!-- hermon:run=old;request=old -->\n{{workflow}}\n",
    )
    .unwrap();
    let templates = Templates::load(Some(dir.path()), None).unwrap();
    let rendered=templates.render("started.md",&values(),"owner","run-121","started").expect("a copied marker inside an owner template must be neutralized before appending the current marker");
    assert!(rendered.starts_with("Owner template with copied marker:"));
    assert_eq!(rendered.matches("<!-- hermon:").count(), 1);
    assert!(rendered.ends_with("<!-- hermon:run=run-121;request=started -->"));
}
