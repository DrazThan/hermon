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
        if params["sessionId"].as_str() != Some(session) {
            return None;
        }
        let input = &params["toolCall"]["rawInput"];
        let arguments = &input["arguments"];
        match input["tool"].as_str()? {
            "write_file" => {
                self.output_path(Path::new(arguments["path"].as_str()?))?;
            }
            "patch" if arguments["mode"] == "patch" => {
                for target in canonical_update_targets(arguments["patch"].as_str()?)? {
                    let path = Path::new(target);
                    self.output_path(path)?;
                    // Patch approval is intentionally narrower than write_file:
                    // no creation, lexical aliases, or symlinks within the attempt.
                    if target.trim() != target
                        || target
                            .split('/')
                            .skip(1)
                            .any(|part| part.is_empty() || part == "." || part == "..")
                    {
                        return None;
                    }
                    // The system temp root may itself be an alias (e.g. /var).
                    // Locate the outermost spelling of our canonical attempt,
                    // then reject every symlink beneath that boundary.
                    let root = path
                        .ancestors()
                        .skip(1)
                        .filter(|ancestor| {
                            fs::canonicalize(ancestor).ok().as_ref() == Some(&self.directory)
                        })
                        .last()?;
                    let relative = path.strip_prefix(root).ok()?;
                    let mut current = root.to_owned();
                    for part in relative.components() {
                        let name = part.as_os_str().to_str()?;
                        if name.starts_with('.') || name.ends_with(".lock") {
                            return None;
                        }
                        current.push(part);
                        if fs::symlink_metadata(&current)
                            .ok()?
                            .file_type()
                            .is_symlink()
                        {
                            return None;
                        }
                    }
                    if !fs::symlink_metadata(path).ok()?.is_file() {
                        return None;
                    }
                }
            }
            _ => return None,
        }
        params["options"]
            .as_array()?
            .iter()
            .find(|option| option["kind"] == "allow_once")?
            .get("optionId")?
            .as_str()
            .map(str::to_owned)
    }
    fn output_path(&self, path: &Path) -> Option<()> {
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
        Some(())
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
                if prompt && failed_admission(&value) {
                    state
                        .hermes
                        .as_mut()
                        .context("missing ACP identity")?
                        .settled = true;
                    bail!(
                        "Hermes confirmed a failed admitted turn; inspect the attempt log before retry"
                    );
                }
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

/// The pinned public ACP adapter emits this only after its admitted turn's
/// canonical receipt is terminal/failed. Other RPC errors remain ambiguous.
fn failed_admission(frame: &Value) -> bool {
    frame.get("method").is_none()
        && frame.get("result").is_none()
        && frame["error"]["code"] == -32603
        && frame["error"]["message"] == "Internal error"
        && frame["error"]["data"]["details"] == "admitted_turn_failed"
}

/// Recover only the exact failed prompt from an older adapter's complete trace.
/// No transport is opened and no candidate result is accepted.
pub(super) fn reconcile_failure(
    directory: &Path,
    launch: &Launch,
    state: &mut State,
) -> Result<()> {
    let config = launch
        .hermes_acp
        .as_ref()
        .context("not a Hermes ACP attempt")?;
    config.validate()?;
    let receipt = state
        .hermes
        .as_ref()
        .context("missing ACP session identity")?;
    ensure!(
        receipt.profile == config.profile
            && receipt.model_id == config.model_id
            && receipt.family == family(&config.model_id)
            && state.attempt.session_id.as_deref() == Some(&receipt.current_session_id),
        "launch and session receipt disagree"
    );
    ensure!(
        state.attempt.log_path == directory.join("log"),
        "foreign ACP trace path"
    );
    let file = File::open(directory.join("log"))?;
    ensure!(
        file.metadata()?.len() <= 128 * 1024 * 1024,
        "ACP trace exceeds recovery limit"
    );
    let mut reader = BufReader::new(file);
    let mut expected = 1_u64;
    let mut failed = false;
    loop {
        let mut line = String::new();
        let n = Read::by_ref(&mut reader)
            .take(16 * 1024 * 1024 + 1)
            .read_line(&mut line)?;
        if n == 0 {
            break;
        }
        ensure!(
            !failed && n <= 16 * 1024 * 1024 && line.ends_with('\n'),
            "incomplete or trailing ACP trace"
        );
        let frame: Value = serde_json::from_str(&line)?;
        ensure!(frame["jsonrpc"] == "2.0", "invalid ACP trace frame");
        if frame.get("method").is_some() {
            continue;
        }
        ensure!(
            frame["id"].as_u64() == Some(expected),
            "unmatched ACP response"
        );
        match expected {
            1 => ensure!(
                frame.get("error").is_none()
                    && frame["result"]["protocolVersion"] == 1
                    && frame["result"]["agentInfo"]["name"] == "hermes-agent"
                    && frame["result"]["agentInfo"]["version"] == config.version,
                "unverified ACP initialization"
            ),
            2 => {
                ensure!(frame.get("error").is_none(), "failed ACP session open");
                let previous = match &launch.resume_session {
                    Some(session) => {
                        ensure!(
                            session == &receipt.acp_session_id,
                            "foreign resume identity"
                        );
                        Some(receipt)
                    }
                    None => None,
                };
                let opened = identity(&frame["result"], config, previous)?;
                ensure!(
                    opened.acp_session_id == receipt.acp_session_id
                        && opened.current_session_id == receipt.current_session_id,
                    "foreign ACP session"
                );
            }
            3 => ensure!(
                frame.get("error").is_none() && frame["result"].is_object(),
                "unconfirmed ACP mode"
            ),
            4..=7 => {
                if failed_admission(&frame) {
                    failed = true;
                } else {
                    ensure!(
                        frame.get("error").is_none() && frame["result"]["stopReason"] == "end_turn",
                        "ambiguous ACP prompt outcome"
                    );
                }
            }
            _ => bail!("unexpected ACP response sequence"),
        }
        expected += 1;
    }
    ensure!(failed, "no confirmed failed admission in ACP trace");
    state.hermes.as_mut().unwrap().settled = true;
    state.result = Some(capability_result(
        "Hermes confirmed a failed admitted turn; recorded trace reconciled without accepting a result or resending work",
    ));
    state.attempt.cancelled |= directory.join("cancel").exists();
    state.process_success = !state.attempt.cancelled && !state.attempt.timed_out;
    state.completion = state.process_success;
    state.finished = true;
    Ok(())
}
/// Accept only the unambiguous Update File subset of Hermes' permissive parser.
/// Inspect every raw header; titles and displayed diffs are never authority.
fn canonical_update_targets(patch: &str) -> Option<Vec<&str>> {
    if patch.contains(['\r', '\0']) {
        return None;
    }
    let body = patch
        .strip_suffix('\n')
        .unwrap_or(patch)
        .strip_prefix("*** Begin Patch\n")?
        .strip_suffix("\n*** End Patch")?;
    let mut targets = Vec::new();
    let mut hunk = false;
    let mut content = false;
    for line in body.split('\n') {
        if let Some(target) = line.strip_prefix("*** Update File: ") {
            if target.is_empty() || (!targets.is_empty() && !content) {
                return None;
            }
            targets.push(target);
            hunk = false;
            content = false;
        } else if line == "@@"
            || line.strip_prefix("@@ ").is_some_and(|hint| {
                hint.strip_suffix(" @@")
                    .is_some_and(|text| !text.is_empty() && text.trim() == text)
            })
        {
            if targets.is_empty() || (hunk && !content) {
                return None;
            }
            hunk = true;
            content = false;
        } else if hunk && line.starts_with([' ', '+', '-']) {
            content = true;
        } else {
            return None;
        }
    }
    (!targets.is_empty() && content).then_some(targets)
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
        let mut prompt = task;
        for turn in 0..4 {
            if turn > 0 {
                state.attempt.cancelled |= directory.join("cancel").exists();
                state.attempt.timed_out |=
                    now_ms().saturating_sub(launch.created_ms) >= launch.timeout_ms;
                if state.attempt.cancelled || state.attempt.timed_out {
                    client.cancel(&sid)?;
                    return Ok(());
                }
            }
            // A model may end a conversational turn while a background check
            // is still running. Keep the same owned attempt and session until
            // it delivers the file contract, without resetting its deadline.
            terminal = false;
            state.hermes.as_mut().unwrap().settled = false;
            write_json(&directory.join("process.json"), state)?;
            let id = client.request(
                "session/prompt",
                json!({"sessionId":sid,"prompt":[{"type":"text","text":prompt}]}),
            )?;
            let result = match client.wait(id, directory, launch, state, true) {
                Ok(result) => result,
                Err(error) => {
                    terminal = state.hermes.as_ref().is_some_and(|receipt| receipt.settled);
                    return Err(error);
                }
            };
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
            if directory.join("result.json").try_exists()? {
                break;
            }
            ensure!(
                turn < 3,
                "Hermes ended four turns without the assigned result.json; inspect existing background jobs before retrying"
            );
            prompt = format!(
                "Continue only the current workflow step and its existing task contract. The assigned result.json at {} is still missing. If a background job is running, wait on its existing process handle for the actual outcome; do not relaunch it. Complete and validate the declared artifacts, then write result.json using the task's schema. Report any block or owner question through that file. No owner approval is implied: do not pass gates, start successor steps, merge or deploy.",
                directory.join("result.json").display()
            );
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

#[cfg(test)]
mod patch_tests {
    use super::canonical_update_targets;

    #[test]
    fn patch_content_markers_cannot_inject_operations() {
        let patch = "*** Begin Patch\n*** Update File: /owned/plan.md\n@@ heading @@\n-*** Delete File: /outside\n+*** Begin Patch\n *** End Patch\n@@\n-old\n+new\n*** End Patch\n";
        assert_eq!(
            canonical_update_targets(patch),
            Some(vec!["/owned/plan.md"])
        );
    }

    #[test]
    fn every_target_and_hunk_requires_explicit_nonempty_content() {
        for body in [
            "",
            "*** Update File: /owned/plan.md\n-old\n+new",
            "*** Update File: /owned/plan.md\n@@",
            "*** Update File: /owned/plan.md\n@@\n@@\n+new",
            "*** Update File: /owned/plan.md\n@@\n+new\n@@",
            "*** Update File: /owned/plan.md\n*** Update File: /owned/notes.md\n@@\n+new",
            "*** Update File: /owned/plan.md\n@@\n+new\n*** Update File: /owned/notes.md",
        ] {
            assert!(
                canonical_update_targets(&format!("*** Begin Patch\n{body}\n*** End Patch"))
                    .is_none(),
                "accepted {body:?}"
            );
        }
    }

    #[test]
    fn parser_rejects_ambiguous_markers_and_unparsed_text() {
        let valid =
            "*** Begin Patch\n*** Update File: /owned/plan.md\n@@\n-old\n+new\n*** End Patch";
        for patch in [
            format!("prefix\n{valid}"),
            format!("{valid}\nsuffix"),
            format!("{valid}\n\n"),
            valid.replace('\n', "\r\n"),
            valid.replace("+new", "+new\0"),
            valid.replace("@@", "@@ invalid"),
            valid.replace("@@", "@@  padded @@"),
            valid.replace("+new", "*** Begin Patch\n+new"),
            valid.replace("+new", "*** Move File: /owned/plan.md -> /outside"),
            valid.replace("+new", "*** End of File"),
        ] {
            assert!(
                canonical_update_targets(&patch).is_none(),
                "accepted {patch:?}"
            );
        }
    }
}
