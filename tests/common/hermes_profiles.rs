//! Schema-backed Hermes profile fixtures, child-process isolation, and a
//! test-only lossless `hr1` decoder.
//!
//! The decoder is not a production helper and must not accept a raw vendor id
//! as a fallback. Shared helpers are unused in some integration crates; that
//! is expected.
#![allow(dead_code)]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::fixture_path;

pub const FIXTURE_TS: f64 = 1_800_000_000.0;
pub const FRESH_WINDOW: &str = "1000000000000";

const SOURCES: &[&str] = &["C", "H", "O", "X", "G", "Gm"];

/// One decoded `hr1:<hex-origin>:<source>:<hex-store>:<hex-full-vendor-id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hr1 {
    pub origin: Vec<u8>,
    pub source: String,
    pub store: Vec<u8>,
    pub vendor_id: Vec<u8>,
}

/// `<source>:hr1::<source>:<hex-store>:<hex-full-vendor-id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireId {
    pub outer_source: String,
    pub route: Hr1,
}

/// Strict lossless decode. Empty origin (`hr1::H:...`) is valid. Uppercase,
/// odd-length, or non-hex components are rejected. Raw ids are rejected.
pub fn decode_hr1(id: &str) -> Result<Hr1, String> {
    let parts: Vec<&str> = id.split(':').collect();
    if parts.len() != 5 {
        return Err(format!(
            "hr1 requires exactly 5 colon-separated components, got {} in {id:?}",
            parts.len()
        ));
    }
    if parts[0] != "hr1" {
        return Err(format!("first component must be hr1, got {:?}", parts[0]));
    }
    let origin = decode_hex("origin", parts[1])?;
    let source = parts[2];
    if !SOURCES.contains(&source) {
        return Err(format!("unknown source token {source:?}"));
    }
    let store = decode_hex("store", parts[3])?;
    let vendor_id = decode_hex("vendor", parts[4])?;
    Ok(Hr1 {
        origin,
        source: source.to_string(),
        store,
        vendor_id,
    })
}

/// Decode a newly emitted agent wire id. Outer and inner source must agree
/// and the inner origin must be empty. A second hex encoding of the hr1
/// envelope does not decode.
pub fn decode_wire_id(id: &str) -> Result<WireId, String> {
    let Some((outer, rest)) = id.split_once(':') else {
        return Err(format!("wire id has no outer source: {id:?}"));
    };
    if !SOURCES.contains(&outer) {
        return Err(format!("unknown outer source {outer:?}"));
    }
    let route = decode_hr1(rest)?;
    if route.source != outer {
        return Err(format!(
            "outer source {outer} disagrees with inner {}",
            route.source
        ));
    }
    if !route.origin.is_empty() {
        return Err(format!(
            "wire origin must be empty, got {}",
            encode_hex(&route.origin)
        ));
    }
    Ok(WireId {
        outer_source: outer.to_string(),
        route,
    })
}

/// Panic unless `id` is qualified hr1 with the given origin, source, and
/// full raw vendor bytes. Store bytes must be nonempty. No raw-id fallback.
pub fn require_hr1(id: &str, origin: &[u8], source: &str, vendor: &[u8]) -> Hr1 {
    let route = decode_hr1(id)
        .unwrap_or_else(|e| panic!("aggregate id must be qualified hr1, got {id:?}: {e}"));
    assert_eq!(route.origin, origin, "hr1 origin bytes in {id}");
    assert_eq!(route.source, source, "hr1 source token in {id}");
    assert_eq!(route.vendor_id, vendor, "hr1 full raw vendor id in {id}");
    assert!(
        !route.store.is_empty(),
        "hr1 store component must be nonempty and lossless, got {id}"
    );
    route
}

pub fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn local_hr1(source: &str, store: &[u8], vendor: &[u8]) -> String {
    format!(
        "hr1::{}:{}:{}",
        source,
        encode_hex(store),
        encode_hex(vendor)
    )
}

pub fn wire_session_id(source: &str, store: &[u8], vendor: &[u8]) -> String {
    format!("{source}:{}", local_hr1(source, store, vendor))
}

fn decode_hex(what: &str, text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err(format!("{what} hex length {} is odd", text.len()));
    }
    if text.chars().any(|c| !is_canonical_hex(c)) {
        return Err(format!("{what} hex is not canonical lowercase: {text:?}"));
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_val(bytes[i]);
        let lo = hex_val(bytes[i + 1]);
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn is_canonical_hex(c: char) -> bool {
    c.is_ascii_digit() || ('a'..='f').contains(&c)
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        _ => 0,
    }
}

/// Canonical filesystem path of a Hermes `state.db`, as OS path bytes.
/// This is the store identity these tests require for local Hermes routes.
pub fn store_bytes(db: &Path) -> Vec<u8> {
    let canon = fs::canonicalize(db).unwrap_or_else(|e| panic!("canonicalize {db:?}: {e}"));
    os_path_bytes(&canon)
}

pub fn expected_local_id(db: &Path, vendor: &str) -> String {
    local_hr1("H", &store_bytes(db), vendor.as_bytes())
}

fn os_path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

pub struct Home {
    pub dir: TempDir,
}

impl Home {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn main_db(&self) -> PathBuf {
        self.path().join(".hermes").join("state.db")
    }

    pub fn profile_db(&self, slug: &str) -> PathBuf {
        self.path()
            .join(".hermes")
            .join("profiles")
            .join(slug)
            .join("state.db")
    }

    pub fn main_log(&self) -> PathBuf {
        self.path().join(".hermes").join("logs").join("agent.log")
    }

    pub fn profile_log(&self, slug: &str) -> PathBuf {
        self.path()
            .join(".hermes")
            .join("profiles")
            .join(slug)
            .join("logs")
            .join("agent.log")
    }
}

pub fn prepare_dirs(home: &Path) {
    for rel in ["xdg-config", "xdg-data", "xdg-runtime", "tmp", "absent"] {
        let path = home.join(rel);
        fs::create_dir_all(&path).unwrap_or_else(|e| panic!("mkdir {path:?}: {e}"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o700));
        }
    }
}

pub fn scratch_temp() -> TempDir {
    let base = std::env::temp_dir();
    tempfile::Builder::new()
        .prefix("hermon-111-")
        .tempdir_in(&base)
        .unwrap_or_else(|e| panic!("create temp dir in {base:?}: {e}"))
}

pub fn empty_home() -> Home {
    let dir = scratch_temp();
    prepare_dirs(dir.path());
    Home { dir }
}

/// Apply the captured Hermes schema and close the connection.
pub fn create_db(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| panic!("mkdir {parent:?}: {e}"));
    }
    let schema = fs::read_to_string(fixture_path("hermes_schema.sql"))
        .unwrap_or_else(|e| panic!("read schema: {e}"));
    let conn = Connection::open(path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    conn.execute_batch(&schema)
        .unwrap_or_else(|e| panic!("apply schema to {path:?}: {e}"));
    conn.close().expect("close schema connection");
}

/// One finished session at [`FIXTURE_TS`]. The connection is closed before return.
pub fn seed_finished(path: &Path, id: &str, title: &str, body: &str) {
    create_db(path);
    let conn = Connection::open(path).unwrap_or_else(|e| panic!("reopen {path:?}: {e}"));
    conn.execute(
        "INSERT INTO sessions (id, source, model, title, started_at, ended_at,
                               input_tokens, output_tokens)
         VALUES (?1, 'cli', 'fixture-model', ?2, ?3, ?3, 3, 1)",
        params![id, title, FIXTURE_TS],
    )
    .unwrap_or_else(|e| panic!("insert session {id}: {e}"));
    conn.execute(
        "INSERT INTO messages (session_id, role, content, finish_reason, timestamp)
         VALUES (?1, 'assistant', ?2, 'stop', ?3)",
        params![id, body, FIXTURE_TS],
    )
    .unwrap_or_else(|e| panic!("insert message {id}: {e}"));
    conn.close().expect("close seed connection");
}

pub fn seed_main_and_worker() -> Home {
    let home = empty_home();
    seed_finished(
        &home.main_db(),
        "sess-main-fixture",
        "main-fixture",
        "main-body",
    );
    seed_finished(
        &home.profile_db("worker"),
        "sess-worker-fixture",
        "worker-fixture",
        "worker-body",
    );
    home
}

pub fn write_log(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| panic!("mkdir {parent:?}: {e}"));
    }
    fs::write(path, body).unwrap_or_else(|e| panic!("write log {path:?}: {e}"));
}

pub fn api_log_line(hhmmss: &str, sid6: &str, n: u32) -> String {
    format!(
        "{hhmmss},123 INFO [hermes.{sid6}] agent.conversation_loop: API call #{n}: model=claude-sonnet-5 provider=anthropic in={n}0 out={n} cached=0 latency=1.25s\n"
    )
}

pub fn set_mtime(path: &Path, unix_secs: u64) {
    let file = File::options()
        .write(true)
        .open(path)
        .unwrap_or_else(|e| panic!("open {path:?} for mtime: {e}"));
    file.set_modified(UNIX_EPOCH + Duration::from_secs(unix_secs))
        .unwrap_or_else(|e| panic!("set mtime {path:?}: {e}"));
}

pub fn child_env(home: &Path) -> Vec<(&str, PathBuf)> {
    vec![
        ("HOME", home.to_path_buf()),
        ("XDG_CONFIG_HOME", home.join("xdg-config")),
        ("XDG_DATA_HOME", home.join("xdg-data")),
        ("XDG_RUNTIME_DIR", home.join("xdg-runtime")),
        ("TMPDIR", home.join("tmp")),
    ]
}

/// Existing source-root flags pointed at absent paths inside `home`.
/// `--hermes-db`, `--hermes-home`, and `--bot` are never included.
pub fn absent_source_args(home: &Path, include_log: bool) -> Vec<String> {
    let absent = home.join("absent");
    let mut args = vec![
        "--claude-dir".into(),
        absent.join("claude").display().to_string(),
        "--opencode-db".into(),
        absent.join("opencode.db").display().to_string(),
        "--codex-dir".into(),
        absent.join("codex").display().to_string(),
        "--grok-dir".into(),
        absent.join("grok").display().to_string(),
        "--gemini-dir".into(),
        absent.join("gemini").display().to_string(),
    ];
    if include_log {
        args.push("--hermes-log".into());
        args.push(absent.join("agent.log").display().to_string());
    }
    args
}

pub fn discovery_ls_args(home: &Path) -> Vec<String> {
    let mut args = vec!["ls".into(), "--fresh-window".into(), FRESH_WINDOW.into()];
    args.extend(absent_source_args(home, true));
    args
}

#[derive(Debug)]
pub struct CmdOut {
    pub status: Option<ExitStatus>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl CmdOut {
    pub fn success(&self) -> bool {
        self.status.is_some_and(|s| s.success()) && !self.timed_out
    }

    pub fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

/// Run `hermon` with child-only HOME/XDG isolation. Does not touch this
/// process's HOME. Kills the child if `deadline` elapses.
pub fn hermon(home: &Path, args: &[String], deadline: Duration) -> CmdOut {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hermon"));
    cmd.args(args)
        .env("NO_COLOR", "1")
        .env_remove("HERMES_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in child_env(home) {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().expect("spawn hermon");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        let _ = stderr.read_to_end(&mut err);
        let _ = tx.send((out, err));
    });
    let timed_out = wait_child(&mut child, deadline);
    let status = if timed_out {
        let _ = child.kill();
        child.wait().ok()
    } else {
        child.wait().ok()
    };
    let (out, err) = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    CmdOut {
        status,
        stdout: String::from_utf8_lossy(&out).into_owned(),
        stderr: String::from_utf8_lossy(&err).into_owned(),
        timed_out,
    }
}

fn wait_child(child: &mut Child, deadline: Duration) -> bool {
    let start = Instant::now();
    loop {
        if child.try_wait().ok().flatten().is_some() {
            return false;
        }
        if start.elapsed() >= deadline {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// A long-lived `hermon agent` with stdin held open and stdout lines queued.
pub struct Agent {
    child: Option<Child>,
    stdin: Option<std::process::ChildStdin>,
    lines: Receiver<String>,
}

impl Agent {
    pub fn spawn(home: &Path, extra: &[String]) -> Self {
        let mut args = vec!["agent".into(), "--interval".into(), "0.2".into()];
        args.extend(absent_source_args(home, true));
        args.extend(extra.iter().cloned());
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hermon"));
        cmd.args(&args)
            .env("NO_COLOR", "1")
            .env_remove("HERMES_HOME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in child_env(home) {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().expect("spawn hermon agent");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("agent stdout");
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Agent {
            child: Some(child),
            stdin,
            lines,
        }
    }

    pub fn write_line(&mut self, line: &str) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = writeln!(stdin, "{line}");
            let _ = stdin.flush();
        }
    }

    /// Collect lines until `pred` is true or `deadline` elapses.
    pub fn recv_until(
        &self,
        deadline: Duration,
        mut pred: impl FnMut(&str) -> bool,
    ) -> Vec<String> {
        let end = Instant::now() + deadline;
        let mut got = Vec::new();
        while Instant::now() < end {
            let remaining = end.saturating_duration_since(Instant::now());
            match self
                .lines
                .recv_timeout(remaining.min(Duration::from_millis(50)))
            {
                Ok(line) => {
                    let done = pred(&line);
                    got.push(line);
                    if done {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        got
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn file_bytes(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
