//! Best-effort Grok Build store reader (observed format, 2026-09-15).
//!
//! `root` is Grok home; roster integration reserves `G:`. Completion and
//! rewind events are unverified: shared silence timeouts determine liveness.
//! Cost stays unknown: follow-up requires producer evidence and a fixture for
//! costUsdTicks' scale. Cache/reasoning subcounts are not added to session totals.
//! Initial transcript history is bounded to 4 MiB; older outstanding calls may
//! therefore be unknown. Records over 256 KiB and unsupported content shapes
//! are skipped. No encrypted reasoning is rendered.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::{LastEvent, Replay, SessionMeta, Source, Tailer};
use crate::render::{Seg, Sem, StyledLine, clip, sanitize};

const WINDOW: u64 = 4 * 1024 * 1024;
const RECORD: usize = 256 * 1024;
const RECENT_SECONDS: f64 = 24.0 * 60.0 * 60.0;

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    identity: u64,
}
impl Stamp {
    fn new(m: &Metadata) -> Self {
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            m.ino()
        };
        #[cfg(not(unix))]
        let identity = 0;
        Self {
            len: m.len(),
            modified: m.modified().ok(),
            identity,
        }
    }
    fn seconds(&self) -> f64 {
        self.modified
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_secs_f64())
    }
}

// Check every store-relative component, including sidecars. Decoded cwd and
// summary IDs never participate in filesystem access.
fn safe(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut p = root.to_path_buf();
    for part in relative.components() {
        if !matches!(part, std::path::Component::Normal(_)) {
            return false;
        }
        p.push(part);
        if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
            return false;
        }
    }
    true
}
fn metadata(root: &Path, path: &Path) -> Option<Metadata> {
    if !safe(root, path) {
        return None;
    }
    fs::metadata(path).ok().filter(|m| m.is_file())
}

#[derive(Default)]
struct Sidecar {
    stamp: Option<Stamp>,
    value: Value,
}
impl Sidecar {
    fn update(&mut self, root: &Path, path: &Path) {
        let Some(m) = metadata(root, path) else {
            return;
        };
        let stamp = Stamp::new(&m);
        if self.stamp.as_ref() == Some(&stamp) || m.len() > WINDOW {
            return;
        }
        let value = File::open(path)
            .ok()
            .and_then(|f| serde_json::from_reader::<_, Value>(f.take(WINDOW)).ok());
        if let Some(value @ Value::Object(_)) = value {
            // A syntactically valid but incomplete accounting snapshot must
            // not erase the last good totals either.
            if path.file_name().is_some_and(|n| n == "usage.json")
                && (value["session"]["inputTokens"].as_u64().is_none()
                    || value["session"]["outputTokens"].as_u64().is_none())
            {
                return;
            }
            if value.as_object().is_some_and(|v| v.is_empty()) {
                return;
            }
            self.value = value;
            self.stamp = Some(stamp);
        }
    }
}

struct Reader {
    root: PathBuf,
    path: PathBuf,
    replay: u64,
    stamp: Option<Stamp>,
    offset: u64,
    pending: Vec<u8>,
    discard: bool,
    missing: bool,
    anchor: Vec<u8>,
}
impl Reader {
    fn new(root: PathBuf, path: PathBuf, replay: u64) -> Self {
        Self {
            root,
            path,
            replay: replay.min(WINDOW),
            stamp: None,
            offset: 0,
            pending: Vec::new(),
            discard: false,
            missing: false,
            anchor: Vec::new(),
        }
    }
    fn unavailable(&mut self) -> (bool, Option<&'static str>, Vec<Value>) {
        let notice = (!self.missing).then_some("· Grok transcript unavailable; waiting");
        self.missing = true;
        (false, notice, vec![])
    }
    fn read(&mut self) -> (bool, Option<&'static str>, Vec<Value>) {
        let Some(m) = metadata(&self.root, &self.path) else {
            return self.unavailable();
        };
        let stamp = Stamp::new(&m);
        let Ok(mut file) = File::open(&self.path) else {
            return self.unavailable();
        };
        let mut reset = self.stamp.as_ref().is_some_and(|old| {
            self.missing
                || old.identity != stamp.identity
                || stamp.len < self.offset
                || (old != &stamp && old.len == stamp.len)
        });
        if !reset && self.stamp.as_ref() == Some(&stamp) && self.offset == stamp.len {
            return (false, None, vec![]);
        }
        if !reset && !self.anchor.is_empty() {
            let mut check = vec![0; self.anchor.len()];
            reset = file
                .seek(SeekFrom::Start(self.offset - check.len() as u64))
                .is_err()
                || file.read_exact(&mut check).is_err()
                || check != self.anchor;
        }
        if self.stamp.is_none() || reset {
            self.offset = stamp.len.saturating_sub(if reset || self.missing {
                WINDOW
            } else {
                self.replay
            });
            self.pending.clear();
            self.anchor.clear();
            self.discard = false;
            if self.offset > 0 {
                let mut prev = [0];
                self.discard = file.seek(SeekFrom::Start(self.offset - 1)).is_err()
                    || file.read_exact(&mut prev).is_err()
                    || prev[0] != b'\n';
            }
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return self.unavailable();
        }
        let mut bytes = Vec::new();
        if (&mut file).take(WINDOW).read_to_end(&mut bytes).is_err() {
            return self.unavailable();
        }
        self.offset += bytes.len() as u64;
        let mut records = Vec::new();
        for byte in bytes {
            if byte == b'\n' {
                if !self.discard
                    && let Ok(v @ Value::Object(_)) = serde_json::from_slice(&self.pending)
                {
                    records.push(v);
                }
                self.pending.clear();
                self.discard = false;
            } else if !self.discard {
                if self.pending.len() == RECORD {
                    self.pending.clear();
                    self.discard = true;
                } else {
                    self.pending.push(byte);
                }
            }
        }
        self.anchor = vec![0; self.offset.min(64) as usize];
        if file
            .seek(SeekFrom::Start(self.offset - self.anchor.len() as u64))
            .is_err()
            || file.read_exact(&mut self.anchor).is_err()
        {
            self.anchor.clear();
        }
        self.stamp = Some(stamp);
        self.missing = false;
        (reset, reset.then_some("· Grok transcript reset"), records)
    }
}

fn string<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str().filter(|s| !s.trim().is_empty())
}
fn clean(s: &str) -> String {
    sanitize(&clip(s, 200))
}
fn timestamp(v: &Value, key: &str) -> f64 {
    string(v, key)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or(0.0, |t| t.timestamp_millis() as f64 / 1000.0)
}
fn decode(s: &str) -> String {
    let mut bytes = Vec::new();
    let mut it = s.as_bytes().iter().copied().peekable();
    while let Some(b) = it.next() {
        if b == b'%' {
            let mut next = it.clone();
            if let (Some(a), Some(b)) = (next.next(), next.next())
                && let (Some(a), Some(b)) = ((a as char).to_digit(16), (b as char).to_digit(16))
            {
                bytes.push((a * 16 + b) as u8);
                it = next;
                continue;
            }
        }
        bytes.push(b);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[derive(Default)]
struct Transcript {
    model: Option<String>,
    last_tool: Option<String>,
    last_line: String,
    event: Option<LastEvent>,
    pending: HashSet<String>,
}
impl Transcript {
    fn apply(&mut self, v: &Value, render: bool) -> Vec<StyledLine> {
        let mut out = Vec::new();
        let kind = string(v, "type").unwrap_or("");
        if kind == "assistant" {
            if let Some(model) = string(v, "model_id") {
                self.model = Some(clean(model));
            }
        } else if kind != "user" && kind != "tool_result" {
            return out;
        }
        if kind == "user" && v.get("synthetic_reason").is_some_and(|v| !v.is_null()) {
            return out;
        }
        if let Some(text) = string(v, "content") {
            self.last_line = clean(text);
            self.event = Some(match kind {
                "user" => LastEvent::User,
                "tool_result" => LastEvent::ToolResult,
                _ => LastEvent::AssistantText,
            });
            if render {
                let (sem, prefix) = match kind {
                    "user" => (Sem::User, "❯ "),
                    "tool_result" => (Sem::Dim, "◀ result "),
                    _ => (Sem::Plain, ""),
                };
                out.push(StyledLine(vec![Seg::new(
                    sem,
                    format!("{prefix}{}", clean(text)),
                )]));
            }
        }
        if kind == "tool_result" {
            if let Some(id) = string(v, "tool_call_id") {
                self.pending.remove(id);
            }
            self.event = Some(LastEvent::ToolResult);
        }
        if kind == "assistant"
            && let Some(calls) = v.get("tool_calls").and_then(Value::as_array)
        {
            for call in calls {
                let (Some(id), Some(name)) = (string(call, "id"), string(call, "name")) else {
                    continue;
                };
                if self.pending.len() < 4096 {
                    self.pending.insert(id.to_owned());
                }
                let name = clean(name);
                self.last_tool = Some(name.clone());
                self.event = Some(LastEvent::ToolUse(name.clone()));
                self.last_line = clean(&format!("▶ {name}"));
                if render {
                    let args = call.get("arguments").map_or(String::new(), |v| {
                        v.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| v.to_string())
                    });
                    out.push(StyledLine(vec![
                        Seg::new(Sem::Tool, format!("▶ {name} ")),
                        Seg::new(Sem::Dim, clip(&args, 120)),
                    ]));
                }
            }
        }
        out
    }
}

struct Session {
    summary: Sidecar,
    usage: Sidecar,
    reader: Reader,
    transcript: Transcript,
    cwd: String,
    id: String,
}
/// Read-only source rooted at Grok home, containing `sessions/`.
pub struct GrokSource {
    root: PathBuf,
    sessions: HashMap<PathBuf, Session>,
}
impl GrokSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            sessions: HashMap::new(),
        }
    }
}
impl Source for GrokSource {
    fn sessions(&mut self) -> Vec<SessionMeta> {
        let base = self.root.join("sessions");
        let mut paths = Vec::new();
        if safe(&self.root, &base)
            && let Ok(parents) = fs::read_dir(base)
        {
            for parent in parents
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            {
                if safe(&self.root, &parent.path())
                    && let Ok(children) = fs::read_dir(parent.path())
                {
                    paths.extend(
                        children
                            .flatten()
                            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                            .map(|e| e.path()),
                    );
                }
            }
        }
        paths.sort_by_cached_key(|p| {
            let time = ["summary.json", "usage.json", "chat_history.jsonl"]
                .iter()
                .filter_map(|n| metadata(&self.root, &p.join(n)))
                .filter_map(|m| m.modified().ok())
                .max();
            (std::cmp::Reverse(time), p.clone())
        });
        let present: HashSet<_> = paths.iter().collect();
        self.sessions.retain(|p, _| present.contains(p));
        let mut out = Vec::new();
        let mut ids = HashSet::new();
        for path in paths {
            let session = self
                .sessions
                .entry(path.clone())
                .or_insert_with(|| Session {
                    summary: Sidecar::default(),
                    usage: Sidecar::default(),
                    reader: Reader::new(self.root.clone(), path.join("chat_history.jsonl"), WINDOW),
                    transcript: Transcript::default(),
                    cwd: String::new(),
                    id: String::new(),
                });
            session
                .summary
                .update(&self.root, &path.join("summary.json"));
            session.usage.update(&self.root, &path.join("usage.json"));
            // Initialize only recent transcripts; keep following already tracked
            // ones. Historical sessions still expose their sidecar metadata.
            let transcript_time = metadata(&self.root, &session.reader.path)
                .map(|m| Stamp::new(&m).seconds())
                .unwrap_or(0.0);
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64());
            let (reset, _, records) =
                if session.reader.stamp.is_some() || now - transcript_time <= RECENT_SECONDS {
                    session.reader.read()
                } else {
                    (false, None, Vec::new())
                };
            if reset {
                session.transcript = Transcript::default();
            }
            for v in records {
                session.transcript.apply(&v, false);
            }
            let summary = &session.summary.value;
            let info = &summary["info"];
            session.id = string(info, "id")
                .or_else(|| string(summary, "id"))
                .unwrap_or_else(|| path.file_name().and_then(|s| s.to_str()).unwrap_or("?"))
                .to_owned();
            if !ids.insert(session.id.clone()) {
                continue;
            }
            session.cwd = string(info, "cwd")
                .or_else(|| string(summary, "cwd"))
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    decode(
                        path.parent()
                            .and_then(Path::file_name)
                            .and_then(|s| s.to_str())
                            .unwrap_or("?"),
                    )
                });
            let transcript = &session.transcript;
            let usage = &session.usage.value["session"];
            let last_ts = timestamp(summary, "updated_at")
                .max(timestamp(summary, "last_active_at"))
                .max(transcript_time);
            out.push(SessionMeta {
                id: session.id.clone(),
                started_at: timestamp(summary, "created_at"),
                ended: false,
                model: transcript
                    .model
                    .clone()
                    .unwrap_or_else(|| clean(string(summary, "current_model_id").unwrap_or("?"))),
                title: clean(
                    string(summary, "generated_title")
                        .or_else(|| string(summary, "session_summary"))
                        .unwrap_or(&session.cwd),
                ),
                in_tok: usage["inputTokens"].as_u64().unwrap_or(0),
                out_tok: usage["outputTokens"].as_u64().unwrap_or(0),
                cost: None,
                last_ts,
                turn_done: false,
                tool_pending: !transcript.pending.is_empty(),
                force_live: false,
                last_tool: transcript.last_tool.clone().unwrap_or_else(|| "-".into()),
                last_line: transcript.last_line.clone(),
                last_event: transcript.event.clone(),
            });
        }
        out.sort_by(|a, b| {
            b.last_ts
                .total_cmp(&a.last_ts)
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }
    fn last_tool(&mut self, session_id: &str) -> String {
        self.sessions()
            .into_iter()
            .find(|s| s.id == session_id)
            .map_or_else(|| "-".into(), |s| s.last_tool)
    }
    fn open_tailer(&self, session_id: &str, replay: Replay) -> Option<Box<dyn Tailer>> {
        let session = self
            .sessions
            .values()
            .filter(|s| s.id == session_id)
            .min_by_key(|s| &s.reader.path)?;
        Some(Box::new(GrokTailer {
            reader: Reader::new(self.root.clone(), session.reader.path.clone(), replay.bytes),
            transcript: Transcript::default(),
        }))
    }
}
struct GrokTailer {
    reader: Reader,
    transcript: Transcript,
}
impl Tailer for GrokTailer {
    fn poll(&mut self) -> Vec<StyledLine> {
        let (reset, notice, records) = self.reader.read();
        if reset {
            self.transcript = Transcript::default();
        }
        let mut out: Vec<_> = notice
            .into_iter()
            .map(|s| StyledLine(vec![Seg::new(Sem::Dim, s)]))
            .collect();
        for v in records {
            out.extend(self.transcript.apply(&v, true));
        }
        out
    }
}
