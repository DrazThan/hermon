//! Public Hermes ACP v1 transport. A terminal prompt response, never viewer
//! death, establishes settlement of gateway-owned work.
use super::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader},
    process::ChildStdin,
    sync::mpsc::{self, Receiver, RecvTimeoutError},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HermesAcpConfig {
    pub profile: String,
    pub model_id: String,
    pub version: String,
    pub state_db: PathBuf,
}
impl HermesAcpConfig {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            !self.profile.is_empty()
                && self.profile.len() <= 100
                && self
                    .profile
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
            "invalid Hermes profile"
        );
        ensure!(
            !family(&self.model_id).is_empty(),
            "unsupported Hermes provider/model family"
        );
        ensure!(
            !self.version.is_empty() && self.version.len() <= 80,
            "pin the verified Hermes ACP version"
        );
        ensure!(
            self.state_db.is_absolute() && self.state_db.is_file(),
            "Hermes state_db must be an existing absolute file"
        );
        Ok(())
    }
}
pub(super) fn family(model_id: &str) -> &'static str {
    let Some((provider, model)) = model_id.split_once(':') else {
        return "";
    };
    match provider {
        "openai-codex" if model.starts_with("gpt-") => "openai",
        "xai-oauth" if model.starts_with("grok-") => "xai",
        "claude-subscription-directsdk-experimental"
            if model == "opus" || model.starts_with("claude-") =>
        {
            "anthropic"
        }
        _ => "",
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ModelUsage {
    model: String,
    billing_provider: String,
    api_call_count: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Receipt {
    pub profile: String,
    pub acp_session_id: String,
    pub current_session_id: String,
    pub model_id: String,
    pub family: String,
    pub models: Vec<ModelUsage>,
    pub settled: bool,
}
struct Client {
    owned: OwnedChild,
    stdin: Option<ChildStdin>,
    frames: Receiver<std::result::Result<Value, String>>,
    sequence: u64,
    log: File,
    directory: PathBuf,
    session_id: Option<String>,
}
impl Client {
    fn spawn(
        launch: &Launch,
        state: &mut State,
        directory: &Path,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<Self> {
        let mut command = Command::new(&launch.argv[0]);
        if let Some(env) = env {
            command.env_clear().envs(env);
        }
        let mut child = command
            .args(&launch.argv[1..])
            .current_dir(&launch.workdir)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(File::create(&state.attempt.stderr_path)?)
            .spawn()
            .context("start public Hermes ACP")?;
        state.attempt.process_group = Some(child.id());
        write_json(&directory.join("process.json"), state)?;
        let stdin = child.stdin.take().context("ACP stdin")?;
        let stdout = child.stdout.take().context("ACP stdout")?;
        let (sender, frames) = mpsc::sync_channel(128);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                let read = Read::by_ref(&mut reader)
                    .take(16 * 1024 * 1024 + 1)
                    .read_line(&mut line);
                let value = match read {
                    Ok(0) => Err("Hermes ACP viewer exited without a terminal response".into()),
                    Ok(n) if n > 16 * 1024 * 1024 => Err("oversized ACP frame".into()),
                    Ok(_) => serde_json::from_str(&line).map_err(|_| "invalid ACP frame".into()),
                    Err(_) => Err("unreadable ACP stream".into()),
                };
                let failed = value.is_err();
                if sender.send(value).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            owned: OwnedChild::new(child),
            stdin: Some(stdin),
            frames,
            sequence: 0,
            log: File::create(&state.attempt.log_path)?,
            directory: directory.to_owned(),
            session_id: None,
        })
    }
    fn send(&mut self, value: Value) -> Result<()> {
        let input = self.stdin.as_mut().context("ACP input closed")?;
        serde_json::to_writer(&mut *input, &value)?;
        input.write_all(b"\n")?;
        input.flush()?;
        Ok(())
    }
    fn request(&mut self, method: &str, params: Value) -> Result<u64> {
        self.sequence += 1;
        self.send(json!({"jsonrpc":"2.0","id":self.sequence,"method":method,"params":params}))?;
        Ok(self.sequence)
    }
    fn cancel(&mut self, session: &str) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}))
    }
    fn frame(&mut self) -> Result<Option<Value>> {
        match self.frames.recv_timeout(Duration::from_millis(20)) {
            Ok(Ok(value)) => {
                serde_json::to_writer(&mut self.log, &value)?;
                self.log.write_all(b"\n")?;
                self.log.flush()?;
                // Unrequested client capabilities and approvals fail closed.
                if value.get("method").is_some() && value.get("id").is_some() {
                    let response = if value["method"] == "session/request_permission" {
                        let outcome = self
                            .output_permission(&value["params"])
                            .map(|option| json!({"outcome":"selected","optionId":option}))
                            .unwrap_or_else(|| json!({"outcome":"cancelled"}));
                        json!({"jsonrpc":"2.0","id":value["id"],"result":{"outcome":outcome}})
                    } else {
                        json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32601,"message":"Client capability unavailable"}})
                    };
                    self.send(response)?;
                }
                Ok(Some(value))
            }
            Ok(Err(reason)) => bail!("{reason}"),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => bail!("ACP stream disconnected"),
        }
    }
    fn output_permission(&self, params: &Value) -> Option<String> {
        let session = self.session_id.as_deref()?;
        if params["sessionId"].as_str() != Some(session)
            || params["toolCall"]["rawInput"]["tool"] != "write_file"
        {
            return None;
        }
        let path = Path::new(params["toolCall"]["rawInput"]["arguments"]["path"].as_str()?);
        let name = path.file_name()?.to_str()?;
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || name.starts_with('.')
            || name.ends_with(".lock")
            || [
                "launch.json",
                "process.json",
                "task.md",
                "cancel",
                "log",
                "stderr",
                "worker.log",
                "owner-answer.json",
                "result.before-launch.json",
            ]
            .contains(&name)
        {
            return None;
        }
        if let Ok(metadata) = fs::symlink_metadata(path)
            && (!metadata.is_file() || metadata.file_type().is_symlink())
        {
            return None;
        }
        let parent = fs::canonicalize(path.parent()?).ok()?;
        if !parent.starts_with(&self.directory) {
            return None;
        }
        params["options"]
            .as_array()?
            .iter()
            .find(|option| option["kind"] == "allow_once")?
            .get("optionId")?
            .as_str()
            .map(str::to_owned)
    }
    fn wait(
        &mut self,
        id: u64,
        directory: &Path,
        launch: &Launch,
        state: &mut State,
        prompt: bool,
    ) -> Result<Value> {
        let mut cancellation = None;
        loop {
            state.attempt.cancelled |= directory.join("cancel").exists();
            state.attempt.timed_out |=
                now_ms().saturating_sub(launch.created_ms) >= launch.timeout_ms;
            if state.attempt.cancelled || state.attempt.timed_out {
                write_json(&directory.join("process.json"), state)?;
                ensure!(prompt, "ACP stopped before a prompt was submitted");
                if cancellation.is_none() {
                    let sid = state
                        .hermes
                        .as_ref()
                        .context("missing ACP identity")?
                        .acp_session_id
                        .clone();
                    self.cancel(&sid)?;
                    cancellation = Some(Instant::now());
                }
            }
            ensure!(
                !cancellation.is_some_and(|at: Instant| at.elapsed() >= Duration::from_secs(30)),
                "ACP cancellation was not confirmed; work remains unsettled"
            );
            if let Some(value) = self.frame()?
                && value.get("method").is_none()
                && value["id"] == id
            {
                ensure!(
                    value.get("error").is_none(),
                    "Hermes ACP request failed; inspect the bounded attempt log"
                );
                return value.get("result").cloned().context("missing ACP result");
            }
        }
    }
    fn rpc(
        &mut self,
        method: &str,
        params: Value,
        directory: &Path,
        launch: &Launch,
        state: &mut State,
    ) -> Result<Value> {
        let id = self.request(method, params)?;
        self.wait(id, directory, launch, state, false)
    }
    fn close(&mut self) -> Result<std::process::ExitStatus> {
        self.stdin.take();
        let end = Instant::now() + Duration::from_secs(1);
        while !self.owned.exited()? && Instant::now() < end {
            thread::sleep(Duration::from_millis(20));
        }
        // Called only after settlement or before prompt submission. These
        // signals stop our idle transport, never establish gateway settlement.
        self.owned.finish()
    }
}
fn identity(
    value: &Value,
    config: &HermesAcpConfig,
    previous: Option<&Receipt>,
) -> Result<Receipt> {
    let sid = value["sessionId"]
        .as_str()
        .or_else(|| previous.map(|r| r.acp_session_id.as_str()))
        .context("missing ACP session ID")?;
    ensure!(
        !sid.is_empty() && sid.len() <= 512 && !sid.chars().any(char::is_whitespace),
        "invalid ACP session ID"
    );
    let model = value["models"]["currentModelId"]
        .as_str()
        .context("missing ACP currentModelId")?;
    ensure!(
        model == config.model_id,
        "Hermes current model differs from verified configuration"
    );
    let current = value["_meta"]["hermes"]["sessionProvenance"]["currentHermesSessionId"]
        .as_str()
        .unwrap_or(sid);
    ensure!(
        !current.is_empty() && current.len() <= 512 && !current.chars().any(char::is_whitespace),
        "invalid Hermes session identity"
    );
    Ok(Receipt {
        profile: config.profile.clone(),
        acp_session_id: sid.into(),
        current_session_id: current.into(),
        model_id: model.into(),
        family: family(model).into(),
        models: vec![],
        settled: false,
    })
}
fn usage(config: &HermesAcpConfig, current: &str) -> Result<Vec<ModelUsage>> {
    let database = Connection::open_with_flags(
        &config.state_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("read Hermes usage store")?;
    database.busy_timeout(Duration::from_secs(1))?;
    let mut cursor = current.to_owned();
    let mut seen = BTreeSet::new();
    let mut models: BTreeMap<(String, String), u64> = BTreeMap::new();
    for depth in 0..100 {
        ensure!(
            seen.insert(cursor.clone()),
            "cyclic Hermes compression lineage"
        );
        let mut query = database.prepare("SELECT model,billing_provider,api_call_count FROM session_model_usage WHERE session_id=?1 AND task='' AND api_call_count>0")?;
        let rows = query.query_map([&cursor], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (model, provider, calls) = row?;
            ensure!(
                !family(&format!("{provider}:{model}")).is_empty()
                    && family(&format!("{provider}:{model}")) == family(&config.model_id),
                "Hermes main calls used an unknown or different model family"
            );
            let count = models.entry((model, provider)).or_default();
            *count = count
                .checked_add(u64::try_from(calls)?)
                .context("Hermes usage count overflow")?;
        }
        let parent: Option<String> = database
            .query_row(
                "SELECT parent_session_id FROM sessions WHERE id=?1",
                [&cursor],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some(parent) = parent else {
            break;
        };
        let reason: Option<String> = database
            .query_row(
                "SELECT end_reason FROM sessions WHERE id=?1",
                [&parent],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if reason.as_deref() != Some("compression") {
            break;
        }
        ensure!(
            depth < 99,
            "Hermes compression lineage exceeds 100 sessions"
        );
        cursor = parent;
    }
    ensure!(
        !models.is_empty(),
        "Hermes main-call model usage is unavailable"
    );
    Ok(models
        .into_iter()
        .map(|((model, billing_provider), api_call_count)| ModelUsage {
            model,
            billing_provider,
            api_call_count,
        })
        .collect())
}
fn capability_result(reason: &str) -> String {
    json!({"schema":1,"status":"block","summary":"Hermes ACP prerequisite needs attention","artifacts":[],"block":{"kind":"capability","reason":reason},"verdict":null,"questions":[]}).to_string()
}
pub(super) fn run(
    directory: &Path,
    launch: &Launch,
    state: &mut State,
    env: Option<&BTreeMap<String, String>>,
) -> Result<()> {
    let config = launch.hermes_acp.as_ref().context("missing ACP config")?;
    config.validate()?;
    let mut client = Client::spawn(launch, state, directory, env)?;
    let mut submitted = false;
    let mut terminal = false;
    let outcome = (|| -> Result<()> {
        let init = client.rpc("initialize", json!({"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"hermon","version":env!("CARGO_PKG_VERSION")}}), directory, launch, state)?;
        ensure!(
            init["protocolVersion"] == 1
                && init["agentInfo"]["name"] == "hermes-agent"
                && init["agentInfo"]["version"] == config.version,
            "Hermes ACP protocol/name/version differs from verified configuration"
        );
        let mut params = json!({"cwd":launch.workdir,"mcpServers":[]});
        let method = if let Some(session) = &launch.resume_session {
            params["sessionId"] = json!(session);
            "session/load"
        } else {
            "session/new"
        };
        let opened = client.rpc(method, params, directory, launch, state)?;
        let previous = launch.resume_session.as_ref().map(|id| Receipt {
            profile: config.profile.clone(),
            acp_session_id: id.clone(),
            current_session_id: id.clone(),
            model_id: config.model_id.clone(),
            family: family(&config.model_id).into(),
            models: vec![],
            settled: false,
        });
        state.hermes = Some(identity(&opened, config, previous.as_ref())?);
        client.session_id = state
            .hermes
            .as_ref()
            .map(|receipt| receipt.acp_session_id.clone());
        state.attempt.session_id = state.hermes.as_ref().map(|r| r.current_session_id.clone());
        write_json(&directory.join("process.json"), state)?;
        let sid = state.hermes.as_ref().unwrap().acp_session_id.clone();
        client.rpc(
            "session/set_mode",
            json!({"sessionId":sid,"modeId":"accept_edits"}),
            directory,
            launch,
            state,
        )?;
        let task = read_text(&launch.task).context("unreadable/oversized ACP task")?;
        // Persist identity before submission. A crash or lost acknowledgment is
        // retained as uncertain; no other worker can resend this attempt.
        write_json(&directory.join("process.json"), state)?;
        submitted = true;
        let id = client.request(
            "session/prompt",
            json!({"sessionId":sid,"prompt":[{"type":"text","text":task}]}),
        )?;
        let result = client.wait(id, directory, launch, state, true)?;
        let reason = result["stopReason"]
            .as_str()
            .context("missing ACP terminal stopReason")?;
        ensure!(
            [
                "end_turn",
                "cancelled",
                "max_tokens",
                "max_turn_requests",
                "refusal"
            ]
            .contains(&reason),
            "unknown ACP terminal stopReason"
        );
        terminal = true;
        state.hermes.as_mut().unwrap().settled = true;
        write_json(&directory.join("process.json"), state)?;
        if reason != "end_turn" || state.attempt.cancelled || state.attempt.timed_out {
            return Ok(());
        }
        let refreshed = client.rpc(
            "session/load",
            json!({"sessionId":sid,"cwd":launch.workdir,"mcpServers":[]}),
            directory,
            launch,
            state,
        )?;
        let mut receipt = identity(&refreshed, config, state.hermes.as_ref())?;
        receipt.settled = true;
        // The vendor's token writer is asynchronous. Wait only for main-call
        // evidence to appear; never create or mutate a vendor DB to fill it.
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            match usage(config, &receipt.current_session_id) {
                Ok(models) => {
                    receipt.models = models;
                    break;
                }
                Err(error) if Instant::now() >= end => return Err(error),
                Err(_) => thread::sleep(Duration::from_millis(40)),
            }
        }
        state.attempt.session_id = Some(receipt.current_session_id.clone());
        state.hermes = Some(receipt);
        state.result = Some(
            read_text(&directory.join("result.json"))
                .context("Hermes end_turn did not write the assigned result.json")?,
        );
        state.process_success = true;
        state.completion = true;
        Ok(())
    })();
    if let Err(error) = outcome {
        eprintln!("Hermes ACP: {error:#}");
        if submitted && !terminal {
            write_json(&directory.join("process.json"), state)?;
            return Err(error).context("Hermes turn outcome is unknown; do not resend");
        }
        if terminal && !state.attempt.cancelled && !state.attempt.timed_out {
            state.result = Some(capability_result(&format!("{error:#}")));
            state.process_success = true;
            state.completion = true;
        }
    }
    let exit = client.close()?;
    state.exit_code = exit.code();
    state.attempt.cancelled |= directory.join("cancel").exists();
    state.process_success &= !state.attempt.cancelled && !state.attempt.timed_out;
    state.completion &= !state.attempt.cancelled && !state.attempt.timed_out;
    state.finished = true;
    write_json(&directory.join("process.json"), state)
}

/// The same transport and provenance checks as a real attempt, in scratch and
/// without allocating a run/branch. Preserve evidence if settlement is unknown.
pub(crate) fn probe(
    config: &HermesAcpConfig,
    workdir: &Path,
    env: &BTreeMap<String, String>,
) -> Result<()> {
    config.validate()?;
    let scratch = tempfile::Builder::new()
        .prefix("hermon-hermes-acp-doctor-")
        .tempdir()?;
    let directory = fs::canonicalize(scratch.path())?;
    let task = directory.join("task.md");
    fs::write(
        &task,
        format!(
            "Doctor capability check. Do not change the repository or start other steps.\nWrite {} using this JSON schema: {{\"schema\":1,\"status\":\"done\",\"summary\":\"ready\",\"artifacts\":[],\"block\":null,\"verdict\":null,\"questions\":[]}}\n",
            directory.join("result.json").display()
        ),
    )?;
    let executable = executable_on_path(
        "hermes",
        workdir,
        Some(env.get("PATH").map(String::as_str).unwrap_or("")),
    )?;
    let launch = Launch {
        bot: config.profile.clone(),
        argv: vec![
            executable.to_string_lossy().into_owned(),
            "-p".into(),
            config.profile.clone(),
            "acp".into(),
        ],
        task,
        workdir: workdir.to_owned(),
        task_stdin: false,
        protocol: Protocol::ResultFile,
        timeout_ms: 30_000,
        created_ms: now_ms(),
        hermes_acp: Some(config.clone()),
        resume_session: None,
    };
    let mut state = State {
        attempt: ProcessAttempt {
            pid: std::process::id(),
            process_group: None,
            session_id: None,
            log_path: directory.join("log"),
            stderr_path: directory.join("stderr"),
            timed_out: false,
            cancelled: false,
        },
        finished: false,
        process_success: false,
        completion: false,
        result: None,
        exit_code: None,
        hermes: None,
    };
    write_json(&directory.join("launch.json"), &launch)?;
    write_json(&directory.join("process.json"), &state)?;
    let outcome = run(&directory, &launch, &mut state, Some(env));
    if outcome.is_err() && state.hermes.as_ref().is_some_and(|r| !r.settled) && !state.finished {
        let kept = scratch.keep();
        bail!(
            "Hermes ACP doctor outcome unknown; do not resend; evidence at {}",
            kept.display()
        );
    }
    outcome?;
    let result: Value = serde_json::from_str(
        state
            .result
            .as_deref()
            .context("Hermes ACP doctor did not produce its assigned result")?,
    )?;
    ensure!(
        state.process_success
            && state.completion
            && result["schema"] == 1
            && result["status"] == "done"
            && result["summary"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty())
            && result.get("block").is_some_and(Value::is_null),
        "Hermes ACP doctor probe failed: {}",
        result["block"]["reason"]
            .as_str()
            .unwrap_or("no verified completion")
    );
    Ok(())
}
