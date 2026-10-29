//! Durable, detached process supervision. Only the worker signals its own children.
use crate::hub::{Agent, Block, BlockKind, Poll, Settlement, StartRequest};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Protocol {
    ResultFile,
    CodexJson,
    ClaudeJson,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandConfig {
    pub start: Vec<String>,
    pub resume: Option<Vec<String>>,
    pub task_stdin: bool,
    pub protocol: Protocol,
    pub family: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BotConfig {
    Command(CommandConfig),
    Hermes { profile: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessAttempt {
    /// Detached supervisor PID, never used by the hub as signal authority.
    pub pid: u32,
    pub process_group: Option<u32>,
    pub session_id: Option<String>,
    pub log_path: PathBuf,
    pub stderr_path: PathBuf,
    pub timed_out: bool,
    pub cancelled: bool,
}
#[derive(Serialize, Deserialize)]
struct Launch {
    bot: String,
    argv: Vec<String>,
    task: PathBuf,
    workdir: PathBuf,
    task_stdin: bool,
    protocol: Protocol,
    timeout_ms: u64,
    created_ms: u64,
}
#[derive(Serialize, Deserialize)]
struct State {
    attempt: ProcessAttempt,
    finished: bool,
    process_success: bool,
    completion: bool,
    result: Option<String>,
    exit_code: Option<i32>,
}

pub struct ProcessAgent {
    root: PathBuf,
    workdir: PathBuf,
    worker: PathBuf,
    bots: BTreeMap<String, BotConfig>,
    timeout: Duration,
    resumes: BTreeMap<String, String>,
}
impl ProcessAgent {
    pub fn new(
        root: impl AsRef<Path>,
        workdir: impl AsRef<Path>,
        worker: impl AsRef<Path>,
        bots: BTreeMap<String, BotConfig>,
        timeout: Duration,
    ) -> Result<Self> {
        ensure!(!timeout.is_zero(), "attempt timeout must be positive");
        fs::create_dir_all(root.as_ref())?;
        let workdir = fs::canonicalize(workdir)?;
        ensure!(workdir.is_dir(), "worktree must be a directory");
        Ok(Self {
            root: fs::canonicalize(root)?,
            workdir,
            worker: fs::canonicalize(worker)?,
            bots,
            timeout,
            resumes: BTreeMap::new(),
        })
    }
    fn directory(&self, request: &StartRequest) -> Result<PathBuf> {
        for part in [&request.run_id, &request.step] {
            let mut components = Path::new(part).components();
            ensure!(
                matches!(components.next(), Some(Component::Normal(_)))
                    && components.next().is_none(),
                "run and step must each be a single path component"
            );
        }
        ensure!(request.number > 0, "attempt numbers start at one");
        let directory = fs::canonicalize(
            self.root
                .join(&request.run_id)
                .join("steps")
                .join(&request.step)
                .join(request.number.to_string()),
        )?;
        ensure!(
            directory.starts_with(&self.root),
            "attempt outside run root"
        );
        Ok(directory)
    }
    fn resolve(&self, handle: &str) -> Result<PathBuf> {
        let directory = fs::canonicalize(handle)?;
        ensure!(
            directory.starts_with(&self.root) && directory != self.root,
            "handle outside run root"
        );
        Ok(directory)
    }
    pub fn attempt(&self, handle: &str) -> Result<ProcessAttempt> {
        let directory = self.resolve(handle)?;
        let mut state: State = read_json(&directory.join("process.json"))?;
        state.attempt.cancelled |= directory.join("cancel").exists();
        Ok(state.attempt)
    }
    pub fn set_resume(&mut self, request: &StartRequest, session: Option<String>) {
        let key = format!("{}/{}/{}", request.run_id, request.step, request.number);
        if let Some(session) = session.filter(|s| !s.is_empty()) {
            self.resumes.insert(key, session);
        } else {
            self.resumes.remove(&key);
        }
    }
    fn start_inner(&mut self, request: &StartRequest) -> Result<String> {
        let directory = self.directory(request)?;
        let handle = directory
            .to_str()
            .context("attempt path must be UTF-8")?
            .to_owned();
        // The lock spans reservation and spawn. A reservation without a receipt
        // remains uncertain after a crash and is never automatically relaunched.
        let _lock = lock(&directory.join("launch.lock"), false)?.context("launch lock")?;
        if directory.join("launch.json").exists() {
            return Ok(handle);
        }
        let bot_lock = self.root.join(format!(
            ".bot-{}.lock",
            request
                .bot
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ));
        let _capacity = lock(&bot_lock, false)?.context("bot capacity lock")?;
        let config = match self.bots.get(&request.bot).context("unknown bot")? {
            BotConfig::Command(config) => config,
            BotConfig::Hermes { .. } => bail!(
                "Hermes requires a verified scoped cancellation capability; shared gateway supervision is unavailable"
            ),
        };
        ensure!(
            self.available(&request.bot),
            "bot already has an unresolved attempt"
        );
        let task = fs::canonicalize(directory.join("task.md"))?;
        let _input = File::open(&task)?;
        let key = format!("{}/{}/{}", request.run_id, request.step, request.number);
        let session = self.resumes.get(&key);
        let template = session.and(config.resume.as_ref()).unwrap_or(&config.start);
        ensure!(!template.is_empty(), "empty agent argv");
        let mut argv: Vec<String> = template
            .iter()
            .map(|arg| substitute(arg, &task, &self.workdir, session.map(String::as_str)))
            .collect::<Result<_>>()?;
        argv[0] = executable(&argv[0], &self.workdir)?
            .to_str()
            .context("executable path must be UTF-8")?
            .to_owned();
        let launch = Launch {
            bot: request.bot.clone(),
            argv,
            task,
            workdir: self.workdir.clone(),
            task_stdin: config.task_stdin,
            protocol: config.protocol.clone(),
            timeout_ms: u64::try_from(self.timeout.as_millis())?.max(1),
            created_ms: now_ms(),
        };
        let mut command = Command::new(&self.worker);
        command
            .arg(&directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(directory.join("worker.log"))?)
            .current_dir(&self.workdir);
        // SAFETY: setsid is async-signal-safe; no allocation or locking occurs
        // between fork and exec. The new worker cannot retain the host terminal.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        // Complete fallible local preparation before reserving the launch.
        // Known no-launch failures remain retryable once their cause is fixed.
        write_json(&directory.join("launch.json"), &launch)?;
        match command.spawn() {
            Ok(mut child) => {
                thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(error) => {
                fs::remove_file(directory.join("launch.json"))?;
                return Err(error).context("start detached worker");
            }
        }
        Ok(handle)
    }
    fn poll_inner(&self, handle: &str) -> Result<Poll> {
        let directory = self.resolve(handle)?;
        let state_path = directory.join("process.json");
        if !state_path.exists() {
            let launch: Launch = read_json(&directory.join("launch.json"))?;
            ensure!(
                now_ms().saturating_sub(launch.created_ms) < 5_000,
                "supervisor did not establish a process receipt; outcome requires reconciliation"
            );
            return Ok(Poll::Running);
        }
        let state: State = read_json(&state_path)?;
        if state.finished {
            let cancelled = state.attempt.cancelled || directory.join("cancel").exists();
            return Ok(Poll::Finished {
                process_success: state.process_success && !cancelled && !state.attempt.timed_out,
                completion: state.completion && !cancelled && !state.attempt.timed_out,
                result: state.result,
            });
        }
        // A lifetime lock authenticates the worker without trusting PID reuse.
        // Re-read after acquiring: it may have committed the receipt between
        // our first read and releasing its lock.
        if let Some(_lock) = lock(&directory.join("worker.lock"), true)? {
            let latest: State = read_json(&state_path)?;
            if latest.finished {
                let cancelled = latest.attempt.cancelled || directory.join("cancel").exists();
                return Ok(Poll::Finished {
                    process_success: latest.process_success
                        && !cancelled
                        && !latest.attempt.timed_out,
                    completion: latest.completion && !cancelled && !latest.attempt.timed_out,
                    result: latest.result,
                });
            }
            bail!(
                "supervisor is dead without a completion receipt; reconcile owned work before retry"
            );
        }
        Ok(Poll::Running)
    }
}
impl Agent for ProcessAgent {
    fn family(&self, bot: &str) -> String {
        match self.bots.get(bot) {
            Some(BotConfig::Command(config)) => config.family.clone(),
            Some(BotConfig::Hermes { .. }) => "hermes".into(),
            None => String::new(),
        }
    }
    fn available(&self, bot: &str) -> bool {
        // Capacity covers every run in this adapter's configured root. Unknown
        // receipts conservatively retain capacity until host reconciliation.
        fn entries(path: &Path) -> Result<Vec<PathBuf>> {
            fs::read_dir(path)?.map(|e| Ok(e?.path())).collect()
        }
        let inspect = || -> Result<bool> {
            for run in entries(&self.root)? {
                let steps = run.join("steps");
                if !steps.is_dir() {
                    continue;
                }
                for step in entries(&steps)? {
                    for attempt in entries(&step)? {
                        let launch_path = attempt.join("launch.json");
                        if !launch_path.exists() {
                            continue;
                        }
                        let launch: Launch = read_json(&launch_path)?;
                        if launch.bot == bot {
                            let state: Option<State> = if attempt.join("process.json").exists() {
                                Some(read_json(&attempt.join("process.json"))?)
                            } else {
                                None
                            };
                            if !state.is_some_and(|s| s.finished) {
                                return Ok(false);
                            }
                        }
                    }
                }
            }
            Ok(true)
        };
        self.bots.contains_key(bot) && inspect().unwrap_or(false)
    }
    fn start(&mut self, request: &StartRequest) -> std::result::Result<String, Block> {
        self.start_inner(request)
            .map_err(|e| Block::new(BlockKind::Capability, format!("{e:#}")))
    }
    fn poll(&mut self, handle: &str) -> std::result::Result<Poll, Block> {
        self.poll_inner(handle)
            .map_err(|e| Block::new(BlockKind::Transient, format!("{e:#}")))
    }
    fn stop(&mut self, handle: &str) -> std::result::Result<Settlement, Block> {
        let stop = || -> Result<Settlement> {
            if matches!(self.poll_inner(handle)?, Poll::Finished { .. }) {
                return Ok(Settlement::Confirmed);
            }
            let directory = self.resolve(handle)?;
            // Durable before the worker can signal; repeating this is harmless.
            write_json(&directory.join("cancel"), &true)?;
            Ok(Settlement::Unsettled)
        };
        stop().map_err(|e| Block::new(BlockKind::Transient, format!("{e:#}")))
    }
}

pub fn hermes_argv(profile: &str, task: &Path, resume: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "hermes".into(),
        "-p".into(),
        profile.into(),
        "chat".into(),
        "-Q".into(),
        "--query-file".into(),
        task.to_string_lossy().into_owned(),
    ];
    if let Some(session) = resume {
        argv.extend(["--resume".into(), session.into()]);
    }
    argv
}
pub fn hermes_session_id(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix("session_id: "))
        .rfind(|id| !id.is_empty() && !id.chars().any(char::is_whitespace))
        .map(str::to_owned)
}
fn substitute(arg: &str, task: &Path, workdir: &Path, session: Option<&str>) -> Result<String> {
    // Expand once: placeholder-like text inside a value stays literal.
    let mut rest = arg;
    let mut output = String::new();
    while let Some(offset) = rest.find('{') {
        output.push_str(&rest[..offset]);
        rest = &rest[offset..];
        if let Some(after) = rest.strip_prefix("{task}") {
            output.push_str(task.to_str().context("task path must be UTF-8")?);
            rest = after;
        } else if let Some(after) = rest.strip_prefix("{workdir}") {
            output.push_str(workdir.to_str().context("workdir must be UTF-8")?);
            rest = after;
        } else if let Some(after) = rest.strip_prefix("{session}") {
            output.push_str(session.context("session placeholder without an explicit session")?);
            rest = after;
        } else {
            output.push('{');
            rest = &rest[1..];
        }
    }
    output.push_str(rest);
    Ok(output)
}
fn executable(program: &str, workdir: &Path) -> Result<PathBuf> {
    let candidates: Vec<PathBuf> =
        if Path::new(program).components().count() > 1 || Path::new(program).is_absolute() {
            vec![workdir.join(program)]
        } else {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|p| workdir.join(p).join(program))
                .collect()
        };
    for path in candidates {
        if let Ok(meta) = path.metadata()
            && meta.is_file()
            && meta.permissions().mode() & 0o111 != 0
        {
            return Ok(fs::canonicalize(path)?);
        }
    }
    bail!("agent executable not found or not executable: {program}")
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_reader(File::open(path).with_context(|| format!("open {}", path.display()))?)
        .with_context(|| format!("parse {}", path.display()))
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let directory = path.parent().context("missing parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}
fn lock(path: &Path, nonblocking: bool) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let flags = libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 };
    // SAFETY: the fd belongs to the live File and flags are flock constants.
    if unsafe { libc::flock(file.as_raw_fd(), flags) } == 0 {
        return Ok(Some(file));
    }
    let error = std::io::Error::last_os_error();
    if nonblocking && error.kind() == std::io::ErrorKind::WouldBlock {
        return Ok(None);
    }
    Err(error.into())
}
fn read_text(path: &Path) -> Option<String> {
    let mut data = String::new();
    File::open(path)
        .ok()?
        .take(16 * 1024 * 1024 + 1)
        .read_to_string(&mut data)
        .ok()?;
    (data.len() <= 16 * 1024 * 1024).then_some(data)
}
fn protocol_observation(protocol: &Protocol, stdout: &str) -> (Option<String>, bool) {
    let mut session = None;
    let mut completion = matches!(protocol, Protocol::ResultFile);
    for value in stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
    {
        match protocol {
            Protocol::ResultFile => {}
            Protocol::CodexJson => match value["type"].as_str() {
                Some("thread.started") => {
                    session = value["thread_id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned);
                    completion = false;
                }
                Some("turn.started" | "turn.failed" | "error") => completion = false,
                Some("turn.completed") => completion = true,
                _ => {}
            },
            Protocol::ClaudeJson => {
                if let Some(id) = value["session_id"].as_str().filter(|id| !id.is_empty()) {
                    session = Some(id.to_owned());
                }
                if value["type"] == "result" {
                    completion =
                        value["is_error"] == false && value["terminal_reason"] == "completed";
                }
            }
        }
    }
    (session, completion)
}
/// Owns a still-unreaped child, keeping its PID/group identity reserved until
/// the last group signal. All error paths clean up that same owned group.
pub(crate) struct OwnedChild {
    child: Child,
    reaped: bool,
}
impl OwnedChild {
    pub(crate) fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }
    pub(crate) fn signal(&self, signal: i32) -> Result<()> {
        // SAFETY: child is retained unreaped, pgid was assigned by spawn, and
        // cannot be reused while this process still owns the child PID.
        if unsafe { libc::kill(-(self.child.id() as i32), signal) } == -1 {
            let error = std::io::Error::last_os_error();
            // Darwin reports EPERM for a group containing only zombies. Verify
            // that case explicitly rather than interpreting EPERM as settlement.
            if error.raw_os_error() == Some(libc::EPERM)
                && self.exited()?
                && group_is_zombies(self.child.id())?
            {
                return Ok(());
            }
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error).context("signal owned process group");
            }
        }
        Ok(())
    }
    pub(crate) fn exited(&self) -> Result<bool> {
        // SAFETY: siginfo_t is a plain C struct; waitid initializes it. WNOWAIT
        // observes exit without reaping, preserving identity for group cleanup.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } == -1
        {
            return Err(std::io::Error::last_os_error()).context("observe child without reaping");
        }
        Ok(unsafe { info.si_pid() } != 0)
    }
    pub(crate) fn finish(&mut self) -> Result<std::process::ExitStatus> {
        // Retain the leader's PID until the whole owned group settles, including
        // descendants whose parent exited successfully before them.
        self.signal(libc::SIGKILL)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !group_is_zombies(self.child.id())? {
            ensure!(
                Instant::now() < deadline,
                "owned process group has not settled after SIGKILL"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let exit = self.child.wait()?;
        self.reaped = true;
        Ok(exit)
    }
}
fn group_is_zombies(group: u32) -> Result<bool> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pgid=,stat="])
        .output()?;
    ensure!(
        output.status.success(),
        "could not inspect owned process group"
    );
    let group = group.to_string();
    for line in String::from_utf8(output.stdout)?.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() == Some(group.as_str())
            && !fields.next().is_some_and(|stat| stat.starts_with('Z'))
        {
            return Ok(false);
        }
    }
    Ok(true)
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.signal(libc::SIGKILL);
            let _ = self.child.wait();
        }
    }
}
#[doc(hidden)]
pub fn worker_main() -> Result<()> {
    let directory = fs::canonicalize(
        std::env::args_os()
            .nth(1)
            .context("missing attempt directory")?,
    )?;
    let _lock =
        lock(&directory.join("worker.lock"), true)?.context("worker already owns attempt")?;
    ensure!(
        !directory.join("process.json").exists(),
        "attempt already launched"
    );
    let launch: Launch = read_json(&directory.join("launch.json"))?;
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
    };
    write_json(&directory.join("process.json"), &state)?;
    // A fresh launch must earn fresh result evidence. Keep any file left in
    // the prepared attempt directory for diagnosis, but never deliver it as
    // output from the process we are about to start.
    let result_path = directory.join("result.json");
    match fs::rename(&result_path, directory.join("result.before-launch.json")) {
        Ok(()) => {
            File::open(&directory)?.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("archive pre-existing result"),
    }
    let mut command = Command::new(&launch.argv[0]);
    command
        .args(&launch.argv[1..])
        .current_dir(&launch.workdir)
        .process_group(0)
        .stdin(if launch.task_stdin {
            Stdio::from(File::open(&launch.task)?)
        } else {
            Stdio::null()
        })
        .stdout(File::create(&state.attempt.log_path)?)
        .stderr(File::create(&state.attempt.stderr_path)?);
    let child = command.spawn().context("spawn agent argv")?;
    let mut owned = OwnedChild::new(child);
    state.attempt.process_group = Some(owned.child.id());
    write_json(&directory.join("process.json"), &state)?;
    loop {
        state.attempt.cancelled = directory.join("cancel").exists();
        state.attempt.timed_out = now_ms().saturating_sub(launch.created_ms) >= launch.timeout_ms;
        if state.attempt.cancelled || state.attempt.timed_out {
            // Deadline/cancellation is committed before any signal, including
            // when the agent handles TERM by writing a result and exiting zero.
            write_json(&directory.join("process.json"), &state)?;
            owned.signal(libc::SIGTERM)?;
            thread::sleep(Duration::from_millis(150));
            break;
        }
        if owned.exited()? {
            break;
        }
        if let Some(stdout) = read_text(&state.attempt.log_path) {
            let (session, _) = protocol_observation(&launch.protocol, &stdout);
            if state.attempt.session_id != session {
                state.attempt.session_id = session;
                write_json(&directory.join("process.json"), &state)?;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    // Also clean descendants left behind by a normally exiting agent. Keep the
    // leader unreaped through this last signal so its group cannot be reused.
    // Signal delivery is asynchronous. Keep both ownership and the leader PID
    // until every member is gone or a zombie; a stuck tool cannot release a
    // retry merely because its leader exited. Failure retains an unresolved
    // receipt and the hub must reconcile instead of resending.
    let exit = owned.finish()?;
    let stdout = read_text(&state.attempt.log_path);
    let (session, completion) = stdout
        .as_deref()
        .map(|s| protocol_observation(&launch.protocol, s))
        .unwrap_or((None, false));
    state.attempt.session_id = session;
    state.attempt.cancelled |= directory.join("cancel").exists();
    state.result = read_text(&directory.join("result.json"));
    state.exit_code = exit.code();
    state.process_success = exit.success() && !state.attempt.timed_out && !state.attempt.cancelled;
    state.completion = completion
        && state.result.is_some()
        && !state.attempt.timed_out
        && !state.attempt.cancelled;
    state.finished = true;
    write_json(&directory.join("process.json"), &state)
}
