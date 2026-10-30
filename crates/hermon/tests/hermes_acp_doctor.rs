//! Independent CLI preflight acceptance for the opt-in public Hermes ACP adapter.
#![cfg(unix)]
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../hermon-flow/tests/fixtures/hermes-acp.py"
);
const MODEL: &str = "openai-codex:gpt-6.1-sol-900k";
struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    workflow: PathBuf,
    worktrees: PathBuf,
    config: Value,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().to_owned();
        let repo = root.join("fixture repo");
        let bin = root.join("bin");
        let logs = root.join("fake ACP logs");
        for path in [&repo, &bin, &logs] {
            fs::create_dir_all(path).unwrap();
        }
        let python = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
        let wrapper = bin.join("hermes");
        fs::write(
            &wrapper,
            format!("#!/bin/sh\nexec '{}' '{}' \"$@\"\n", python, FAKE),
        )
        .unwrap();
        fs::set_permissions(wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let gh = bin.join("gh");
        fs::write(&gh, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(gh, fs::Permissions::from_mode(0o755)).unwrap();
        let worktrees = root.join("never allocated worktrees");
        let state = root.join("fake state.db");
        assert!(Command::new(&python)
            .args(["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('CREATE TABLE sessions(id TEXT PRIMARY KEY,parent_session_id TEXT,end_reason TEXT)'); c.execute('CREATE TABLE session_model_usage(session_id TEXT,model TEXT,billing_provider TEXT,task TEXT,api_call_count INTEGER)'); c.commit()"])
            .arg(&state).status().unwrap().success());
        let config = json!({
            "repo":repo,"worktree_root":worktrees,"branch_prefix":"flow","base":"main","start":"work",
            "env":{"PATH":format!("{}:/opt/homebrew/bin:/usr/bin:/bin", bin.display()),"HOME":root,
                "ACP_SCENARIO":"done","ACP_STATE_DB":state,"ACP_FIXTURE_LOG":logs,"ACP_MODEL_ID":MODEL},
            "bots":{"worker":{"HermesAcp":{"profile":"sdlc-implementer","model_id":MODEL,"version":"0.22.4","state_db":state}}},
            "commands":{},"paths":{},
            "tracker":{"endpoint":"http://127.0.0.1:1/graphql","token_env":"HERMON_ACP_DOCTOR_TEST_TOKEN","owner_ids":["fixture-owner"],"owner_mention":"@owner"},
            "code_host":{"repo":"fixture/project","remote":"origin"}
        });
        let workflow = root.join("workflow.yaml");
        fs::write(
            &workflow,
            "version: 1\nname: acp-doctor-fixture\nsteps:\n  work:\n    bot: worker\n",
        )
        .unwrap();
        let project = root.join("project.json");
        Self {
            _root: temporary,
            root,
            project,
            workflow,
            worktrees,
            config,
        }
    }
    fn report(&self) -> Value {
        fs::write(&self.project, serde_json::to_vec(&self.config).unwrap()).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_hermon"))
            .args([
                "doctor",
                self.workflow.to_str().unwrap(),
                "--project",
                self.project.to_str().unwrap(),
                "--json",
            ])
            .env(
                "HERMON_ACP_DOCTOR_TEST_TOKEN",
                "fixture-token-no-live-access",
            )
            .env("HOME", &self.root)
            .env("XDG_DATA_HOME", self.root.join("app-data"))
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "refused tracker endpoint must keep overall preflight failed"
        );
        let report: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|error|panic!("doctor must report individual findings for supported ACP bindings: {error}; stderr: {}",String::from_utf8_lossy(&out.stderr)));
        assert_eq!(report["schema"], 1);
        assert!(
            !self.worktrees.exists(),
            "doctor must not create a worktree"
        );
        assert!(
            !self.root.join("app-data/hermon/runs").exists(),
            "doctor must not create a run"
        );
        report
    }
}
fn finding<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("missing {name} finding: {report}"))
}

#[test]
fn doctor_accepts_verified_acp_bot_while_unrelated_tracker_auth_stays_failed() {
    let fixture = Fixture::new();
    let report = fixture.report();
    assert_eq!(
        finding(&report, "bot worker")["ok"],
        true,
        "actual ACP task, result and main usage must establish bot readiness: {report}"
    );
    assert_eq!(finding(&report, "tracker auth")["ok"], false);
}

#[test]
fn doctor_rejects_end_turn_without_main_session_usage() {
    let mut fixture = Fixture::new();
    fixture.config["env"]["ACP_USAGE_MODE"] = json!("none");
    let report = fixture.report();
    assert_eq!(
        finding(&report, "bot worker")["ok"],
        false,
        "end_turn without main usage cannot establish a verified bot: {report}"
    );
    assert!(
        !finding(&report, "bot worker")["fix"]
            .as_str()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn doctor_requires_actual_task_result_file_even_after_verified_end_turn() {
    let mut fixture = Fixture::new();
    fixture.config["env"]["ACP_RESULT_PATH"] = json!(fixture.root.join("unrelated-result.json"));
    let report = fixture.report();
    assert_eq!(
        finding(&report, "bot worker")["ok"],
        false,
        "transport completion must not fabricate the doctor's missing result: {report}"
    );
    assert!(
        fixture.root.join("unrelated-result.json").is_file(),
        "fake must have completed and written its result elsewhere"
    );
}
