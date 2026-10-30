//! Independently frozen opt-in Hermes ACP metadata and runtime-provenance tests.
#![cfg(unix)]
use hermon_flow::{
    agent::{BotConfig, ProcessAgent},
    hub::{Agent, BlockKind, Poll, StartRequest},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};

const WORKER: &str = env!("CARGO_BIN_EXE_hermon-flow-worker");
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hermes-acp.py");
const MODEL: &str = "openai-codex:gpt-5.4";

struct Fixture {
    _root: tempfile::TempDir,
    runs: PathBuf,
    work: PathBuf,
    attempt: PathBuf,
    db: PathBuf,
    env: BTreeMap<String, String>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let runs = root.path().join("runs");
        let work = root.path().join("work");
        let bin = root.path().join("bin");
        let attempt = runs.join("ACP-PROVENANCE/steps/work/1");
        for path in [&work, &bin, &attempt] {
            fs::create_dir_all(path).unwrap();
        }
        let db = root.path().join("state.db");
        let python = Command::new("/usr/bin/which")
            .arg("python3")
            .output()
            .unwrap();
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
        let wrapper = bin.join("hermes");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec '{}' '{}' \"$@\"\n",
                python.replace('\'', "'\\''"),
                FIXTURE
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            attempt.join("task.md"),
            format!(
                "Provenance fixture task.\nFIXTURE_RESULT_PATH={}\n",
                serde_json::to_string(&attempt.join("result.json")).unwrap()
            ),
        )
        .unwrap();
        let env = BTreeMap::from([
            ("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())),
            ("ACP_SCENARIO".into(), "done".into()),
            ("ACP_MODEL_ID".into(), MODEL.into()),
            ("ACP_VERSION".into(), "0.22.4".into()),
            ("ACP_USAGE_MODE".into(), "preseeded".into()),
            ("ACP_STATE_DB".into(), db.display().to_string()),
            ("ACP_FIXTURE_LOG".into(), attempt.display().to_string()),
            (
                "ACP_RESULT_PATH".into(),
                attempt.join("result.json").display().to_string(),
            ),
            (
                "ACP_LOG".into(),
                attempt.join("acp.jsonl").display().to_string(),
            ),
            ("ACP_SESSION_ID".into(), "acp-fixture-provenance".into()),
            (
                "ACP_CURRENT_SESSION_ID".into(),
                "hermes-fixture-provenance".into(),
            ),
        ]);
        Self {
            _root: root,
            runs,
            work,
            attempt,
            db,
            env,
        }
    }
    fn config(&self, model: &str, version: &str) -> serde_json::Result<BotConfig> {
        serde_json::from_value(
            json!({"HermesAcp": {"profile":"provenance-profile", "model_id":model, "version":version, "state_db":self.db}}),
        )
    }
    fn agent(&self, model: &str) -> ProcessAgent {
        let config = self
            .config(model, "0.22.4")
            .expect("explicit opt-in ACP configuration must load");
        ProcessAgent::new(
            &self.runs,
            &self.work,
            WORKER,
            BTreeMap::from([("worker".into(), config)]),
            Duration::from_secs(5),
        )
        .unwrap()
        .with_env(self.env.clone())
    }
    fn request(&self) -> StartRequest {
        StartRequest {
            run_id: "ACP-PROVENANCE".into(),
            step: "work".into(),
            bot: "worker".into(),
            number: 1,
            freezes: BTreeMap::new(),
        }
    }
    fn seed(&self, sessions: Value, rows: Value) {
        let code = r#"import sqlite3,json,sys
c=sqlite3.connect(sys.argv[1])
c.execute('CREATE TABLE sessions (id TEXT PRIMARY KEY,parent_session_id TEXT,end_reason TEXT)')
c.execute('CREATE TABLE session_model_usage (session_id TEXT,model TEXT,billing_provider TEXT,task TEXT,api_call_count INTEGER)')
c.executemany('INSERT INTO sessions VALUES (?,?,?)',json.loads(sys.argv[2]))
c.executemany('INSERT INTO session_model_usage VALUES (?,?,?,?,?)',json.loads(sys.argv[3]))
c.commit();c.close()
"#;
        let output = Command::new("python3")
            .args(["-c", code])
            .arg(&self.db)
            .arg(sessions.to_string())
            .arg(rows.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn normal_seed(&self, rows: Value) {
        self.seed(json!([["hermes-fixture-provenance", null, null]]), rows);
    }
    fn receipt(&self) -> Value {
        serde_json::from_slice(&fs::read(self.attempt.join("process.json")).unwrap()).unwrap()
    }
}
fn finish(agent: &mut ProcessAgent, handle: &str) -> Result<Poll, hermon_flow::hub::Block> {
    let deadline = Instant::now() + Duration::from_secs(9);
    loop {
        let poll = agent.poll(handle)?;
        if !matches!(poll, Poll::Running) {
            return Ok(poll);
        }
        assert!(Instant::now() < deadline, "ACP fixture did not settle");
        thread::sleep(Duration::from_millis(20));
    }
}
fn result(observed: Poll) -> Value {
    match observed {
        Poll::Finished {
            process_success,
            completion,
            result,
        } => {
            assert!(
                process_success && completion,
                "settled end_turn transport should produce a verified result or capability block"
            );
            serde_json::from_str(
                &result.expect("settled ACP turn must supply host-validated result"),
            )
            .unwrap()
        }
        other => panic!("expected settled end_turn, got {other:?}"),
    }
}
fn assert_capability_block(observed: Poll) {
    let result = result(observed);
    assert_eq!(
        result["status"], "block",
        "invalid provenance cannot accept done: {result}"
    );
    assert_eq!(result["block"]["kind"], "capability");
    assert!(!result["block"]["reason"].as_str().unwrap().is_empty());
}

#[test]
fn acp_known_provider_model_pairs_supply_real_model_families() {
    for (model, family) in [
        (MODEL, "openai"),
        (
            "claude-subscription-directsdk-experimental:opus",
            "anthropic",
        ),
        (
            "claude-subscription-directsdk-experimental:claude-opus-4-6",
            "anthropic",
        ),
        ("xai-oauth:grok-4", "xai"),
    ] {
        let fixture = Fixture::new();
        let agent = fixture.agent(model);
        assert_eq!(
            agent.family("worker"),
            family,
            "different-family review must use the provider/model family"
        );
    }
}

#[test]
fn acp_unknown_provider_model_pair_never_launches_or_claims_family() {
    for model in [
        "unknown:gpt-5.4",
        "openai-codex:opus",
        "xai-oauth:gpt-5.4",
        "claude-subscription-directsdk-experimental:grok-4",
        "openai-codex:",
        "gpt-5.4",
    ] {
        let fixture = Fixture::new();
        if let Ok(config) = fixture.config(model, "0.22.4") {
            let mut agent = ProcessAgent::new(
                &fixture.runs,
                &fixture.work,
                WORKER,
                BTreeMap::from([("worker".into(), config)]),
                Duration::from_secs(5),
            )
            .unwrap()
            .with_env(fixture.env.clone());
            assert!(
                agent.family("worker").is_empty(),
                "unknown provider/model must not claim a known family"
            );
            assert_eq!(
                agent
                    .start(&fixture.request())
                    .expect_err("unsupported provider/model must reject before launch")
                    .kind,
                BlockKind::Capability
            );
        }
        assert!(
            !fixture.attempt.join("acp.jsonl").exists(),
            "unknown model reached Hermes"
        );
    }
}

#[test]
fn acp_initialize_version_mismatch_cannot_start_a_session_or_prompt() {
    let mut fixture = Fixture::new();
    fixture.env.insert("ACP_VERSION".into(), "0.22.5".into());
    fixture.normal_seed(json!([]));
    let mut agent = fixture.agent(MODEL);
    if let Ok(handle) = agent.start(&fixture.request())
        && let Ok(Poll::Finished {
            completion, result, ..
        }) = finish(&mut agent, &handle)
    {
        assert!(
                !completion
                    || result
                        .as_deref()
                        .is_some_and(
                            |s| serde_json::from_str::<Value>(s).unwrap()["status"] == "block"
                        ),
                "version mismatch accepted success"
            );
    }
    let log = fs::read_to_string(fixture.attempt.join("acp.jsonl")).unwrap();
    assert!(log.contains("initialize"));
    assert!(
        !log.contains("session/new"),
        "wrong agent version started a session: {log}"
    );
    assert!(!log.contains("session/prompt"));
}

#[test]
fn acp_current_model_mismatch_cannot_send_a_prompt() {
    let mut fixture = Fixture::new();
    fixture
        .env
        .insert("ACP_MODEL_ID".into(), "xai-oauth:grok-4".into());
    fixture.normal_seed(json!([]));
    let mut agent = fixture.agent(MODEL);
    if let Ok(handle) = agent.start(&fixture.request())
        && let Ok(Poll::Finished {
            completion, result, ..
        }) = finish(&mut agent, &handle)
    {
        assert!(
                !completion
                    || result
                        .as_deref()
                        .is_some_and(
                            |s| serde_json::from_str::<Value>(s).unwrap()["status"] == "block"
                        ),
                "model mismatch accepted success"
            );
    }
    let log = fs::read_to_string(fixture.attempt.join("acp.jsonl")).unwrap();
    assert!(log.contains("session/new"));
    assert!(
        !log.contains("session/prompt"),
        "mismatched currentModelId reached prompt: {log}"
    );
}

#[test]
fn acp_end_turn_accepts_only_recorded_main_model_usage_and_preserves_readonly_db() {
    let fixture = Fixture::new();
    fixture.normal_seed(json!([
        [
            "hermes-fixture-provenance",
            "gpt-5.4",
            "openai-codex",
            "",
            2
        ],
        [
            "hermes-fixture-provenance",
            "grok-4",
            "xai-oauth",
            "title_generation",
            3
        ],
        ["unrelated-session", "grok-4", "xai-oauth", "", 9]
    ]));
    let db_before = fs::read(&fixture.db).unwrap();
    let mut agent = fixture.agent(MODEL);
    let handle = agent.start(&fixture.request()).unwrap();
    assert_eq!(
        result(finish(&mut agent, &handle).unwrap())["status"],
        "done"
    );
    assert_eq!(
        fs::read(&fixture.db).unwrap(),
        db_before,
        "provenance inspection changed the vendor DB"
    );
    assert_eq!(
        agent.attempt(&handle).unwrap().session_id.as_deref(),
        Some("hermes-fixture-provenance")
    );
    let receipt = fixture.receipt();
    assert_eq!(receipt["hermes"]["profile"], "provenance-profile");
    assert_eq!(
        receipt["hermes"]["acp_session_id"],
        "acp-fixture-provenance"
    );
    assert_eq!(
        receipt["hermes"]["current_session_id"],
        "hermes-fixture-provenance"
    );
    assert_eq!(receipt["hermes"]["model_id"], MODEL);
    assert_eq!(receipt["hermes"]["family"], "openai");
    assert_eq!(receipt["hermes"]["settled"], true);
    assert_eq!(
        receipt["hermes"]["models"],
        json!([{"model":"gpt-5.4","billing_provider":"openai-codex","api_call_count":2}])
    );
}

#[test]
fn acp_end_turn_rejects_missing_auxiliary_zero_foreign_or_mixed_main_usage() {
    for rows in [
        json!([]),
        json!([[
            "hermes-fixture-provenance",
            "gpt-5.4",
            "openai-codex",
            "",
            0
        ]]),
        json!([[
            "hermes-fixture-provenance",
            "gpt-5.4",
            "openai-codex",
            "title_generation",
            1
        ]]),
        json!([["foreign-session", "gpt-5.4", "openai-codex", "", 1]]),
        json!([
            [
                "hermes-fixture-provenance",
                "gpt-5.4",
                "openai-codex",
                "",
                1
            ],
            ["hermes-fixture-provenance", "grok-4", "xai-oauth", "", 1]
        ]),
        json!([[
            "hermes-fixture-provenance",
            "gpt-5.4",
            "mystery-provider",
            "",
            1
        ]]),
    ] {
        let fixture = Fixture::new();
        fixture.normal_seed(rows);
        let mut agent = fixture.agent(MODEL);
        let handle = agent.start(&fixture.request()).unwrap();
        assert_capability_block(finish(&mut agent, &handle).unwrap());
    }
}

#[test]
fn acp_main_usage_can_follow_only_compression_parent_lineage() {
    for (reason, expected) in [("compression", true), ("normal", false)] {
        let fixture = Fixture::new();
        fixture.seed(
            json!([
                ["hermes-fixture-provenance", "prior-session", null],
                ["prior-session", null, reason]
            ]),
            json!([["prior-session", "gpt-5.4", "openai-codex", "", 1]]),
        );
        let mut agent = fixture.agent(MODEL);
        let handle = agent.start(&fixture.request()).unwrap();
        let outcome = finish(&mut agent, &handle).unwrap();
        if expected {
            assert_eq!(result(outcome)["status"], "done")
        } else {
            assert_capability_block(outcome)
        }
    }
}

#[test]
fn acp_missing_vendor_db_is_a_capability_block_without_creating_it() {
    let fixture = Fixture::new();
    assert!(!fixture.db.exists());
    let mut agent = fixture.agent(MODEL);
    match agent.start(&fixture.request()) {
        Err(block) => assert_eq!(block.kind, BlockKind::Capability),
        Ok(handle) => assert_capability_block(finish(&mut agent, &handle).unwrap()),
    }
    assert!(
        !fixture.db.exists(),
        "readonly provenance inspection must not create a vendor DB"
    );
}

#[test]
fn acp_compression_lineage_limit_and_cycles_cannot_become_usage_evidence() {
    for cycle in [false, true] {
        let fixture = Fixture::new();
        let mut sessions = vec![json!(["hermes-fixture-provenance", "parent-0", null])];
        for index in 0..102 {
            let next = if index == 101 {
                if cycle {
                    Some("parent-0".to_owned())
                } else {
                    None
                }
            } else {
                Some(format!("parent-{}", index + 1))
            };
            sessions.push(json!([format!("parent-{index}"), next, "compression"]));
        }
        fixture.seed(
            json!(sessions),
            json!([["parent-101", "gpt-5.4", "openai-codex", "", 1]]),
        );
        let mut agent = fixture.agent(MODEL);
        let handle = agent.start(&fixture.request()).unwrap();
        assert_capability_block(finish(&mut agent, &handle).unwrap());
    }
}
