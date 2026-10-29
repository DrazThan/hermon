//! Read-only support for Codex's private rollout format (CLI 0.154.0).
//!
//! Only `sessions/` is visited; no config, credentials, or process probes.
//! Directory and file symlinks are rejected, including on subsequent tail polls.
//! Records over 1 MiB are skipped through the next newline. Each poll reads at
//! most 8 MiB; large histories converge over successive polls. ID sets stop
//! accepting new entries at 16,384 rather than growing without bound.
//!
//! Reconciliation: response messages are canonical. Event-only messages wait
//! until the end of the available batch; if no canonical message for that role
//! has appeared, that role selects the event stream for the rest of the turn.
//! A later canonical copy is suppressed. IDs deduplicate within a turn; text
//! is never a dedup key. This intentionally cannot switch streams midway through
//! an ID-less turn. Replay parses the prefix silently to restore this state.
//! Completion's last_agent_message is used only if neither stream had an answer.
//!
//! Accounting counts only the rollout's own thread (explicit foreign thread IDs
//! are ignored). Thread snapshots outrank total snapshots, which outrank unique
//! response usage. Turn totals and cached/reasoning subcounts are never added.
//! No function-call variants are inferred from the public streaming API.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::{LastEvent, Replay, SessionMeta, Source, Tailer};
use crate::render::{Seg, Sem, StyledLine, clip, parse_ts, sanitize};

const MAX_RECORD: usize = 1024 * 1024;
const MAX_READ: u64 = 8 * 1024 * 1024;
const MAX_IDS: usize = 16_384;

fn string(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_owned()
}
fn label(s: &str) -> String {
    clip(&sanitize(s), 200)
}
fn line(sem: Sem, text: impl Into<String>) -> StyledLine {
    StyledLine(vec![Seg::new(sem, text)])
}
fn seconds(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn filename_id(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let Some(id) = stem.get(stem.len().saturating_sub(36)..) else {
        return String::new();
    };
    if id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
    {
        id.to_owned()
    } else {
        String::new()
    }
}

#[derive(Default)]
struct Decoder {
    id: String,
    cwd: String,
    model: String,
    title: String,
    started: Option<f64>,
    last_ts: f64,
    last_line: String,
    last_tool: String,
    tool_stream: u8,
    commands: Vec<(String, String, String, bool, u64)>,
    last_event: Option<LastEvent>,
    turn: String,
    done: bool,
    pending: HashSet<String>,
    seen: HashSet<String>,
    // 0 undecided, 1 canonical, 2 event-only; indexed user/assistant.
    streams: [u8; 2],
    fallbacks: Vec<(usize, String, String, bool, u64)>,
    sequence: u64,
    last_semantic: u64,
    completion_text: Option<(String, bool, u64)>,
    usage_rank: u8,
    usage: (u64, u64),
    responses: HashSet<String>,
}
impl Decoder {
    fn new(path: &Path) -> Self {
        Self {
            id: filename_id(path),
            ..Self::default()
        }
    }
    fn meta(&self, mtime: f64) -> Option<SessionMeta> {
        if self.id.is_empty() {
            return None;
        }
        Some(SessionMeta {
            id: self.id.clone(),
            started_at: self.started.unwrap_or(mtime),
            ended: false,
            model: if self.model.is_empty() {
                "unknown".into()
            } else {
                self.model.clone()
            },
            title: if self.title.is_empty() {
                label(&self.cwd)
            } else {
                self.title.clone()
            },
            in_tok: self.usage.0,
            out_tok: self.usage.1,
            cost: None,
            last_ts: self.last_ts.max(mtime),
            turn_done: self.done,
            tool_pending: !self.pending.is_empty(),
            force_live: false,
            last_tool: if self.last_tool.is_empty() {
                "-".into()
            } else {
                self.last_tool.clone()
            },
            last_line: self.last_line.clone(),
            last_event: self.last_event.clone(),
        })
    }
    fn unique(&mut self, id: String) -> bool {
        if id.is_empty() {
            return true;
        }
        if self.seen.contains(&id) {
            return false;
        }
        if id.len() > 512 || self.seen.len() >= MAX_IDS {
            return false;
        }
        self.seen.insert(id);
        true
    }
    fn new_turn(&mut self, id: String, out: &mut Vec<(u64, StyledLine)>) {
        self.flush(out);
        self.turn = id;
        self.done = false;
        self.pending.clear();
        self.seen.clear();
        self.streams = [0; 2];
        self.tool_stream = 0;
    }
    fn message(
        &mut self,
        role: usize,
        text: String,
        id: String,
        canonical: bool,
        emit: bool,
        out: &mut Vec<(u64, StyledLine)>,
    ) {
        if text.trim().is_empty() || id.len() > 512 || (!id.is_empty() && self.seen.contains(&id)) {
            return;
        }
        // Codex injects these as user-role context; they are not prompts.
        if role == 0
            && [
                "<environment_context>",
                "<permissions instructions>",
                "# AGENTS.md instructions",
                "<INSTRUCTIONS>",
            ]
            .iter()
            .any(|p| text.trim_start().starts_with(p))
        {
            return;
        }
        if role == 0 && self.done {
            self.new_turn(String::new(), out);
        }
        if (canonical && self.streams[role] == 2) || (!canonical && self.streams[role] == 1) {
            return;
        }
        // Another prompt in the selected stream is new activity, even when
        // the prior turn never produced task_complete. Explicit task_started
        // already cleared selection, so the first prompt retains its turn ID.
        if role == 0 && self.streams[0] != 0 {
            self.new_turn(String::new(), out);
        }
        if canonical {
            self.streams[role] = 1;
            self.display_message(role, text, id, emit, out);
        } else if self.streams[role] != 1 && self.fallbacks.len() < 256 {
            self.fallbacks
                .push((role, clip(&text, 16_384), id, emit, self.sequence));
        }
    }
    fn display_message(
        &mut self,
        role: usize,
        text: String,
        id: String,
        emit: bool,
        out: &mut Vec<(u64, StyledLine)>,
    ) {
        if !self.unique(id) {
            return;
        }
        if role == 0 {
            self.done = false;
            if self.title.is_empty() {
                self.title = label(&text);
            }
            if self.sequence >= self.last_semantic {
                self.last_event = Some(LastEvent::User);
            }
        } else if self.sequence >= self.last_semantic {
            self.last_event = Some(LastEvent::AssistantText);
        }
        if self.sequence >= self.last_semantic {
            self.last_line = label(&text);
            self.last_semantic = self.sequence;
        }
        if emit {
            out.push((
                self.sequence,
                line(
                    if role == 0 { Sem::User } else { Sem::Plain },
                    clip(&text, 16_384),
                ),
            ));
        }
    }
    fn flush(&mut self, out: &mut Vec<(u64, StyledLine)>) {
        let done = self.done;
        let sequence = self.sequence;
        for (role, text, id, emit, seq) in std::mem::take(&mut self.fallbacks) {
            if self.streams[role] != 1 {
                self.streams[role] = 2;
                self.sequence = seq;
                self.display_message(role, text, id, emit, out);
            }
        }
        if let Some((text, emit, seq)) = self.completion_text.take()
            && self.streams[1] == 0
            && !text.is_empty()
        {
            self.streams[1] = 2;
            self.sequence = seq;
            self.display_message(1, text, String::new(), emit, out);
        }
        for (id, command, output, emit, seq) in std::mem::take(&mut self.commands) {
            if self.tool_stream == 1 {
                continue;
            }
            self.tool_stream = 2;
            if !self.unique(if id.is_empty() {
                id
            } else {
                format!("command:{id}")
            }) {
                continue;
            }
            if seq >= self.last_semantic {
                self.last_event = Some(LastEvent::ToolResult);
                self.last_semantic = seq;
                self.last_line = output.clone();
            }
            if emit {
                out.push((
                    seq,
                    line(Sem::Dim, format!("▶ command {command} · {output}")),
                ));
            }
        }
        self.done = done;
        self.sequence = sequence;
    }
    fn accounting(&mut self, p: &Value) {
        let thread = string(p, "thread_id");
        if !thread.is_empty() && thread != self.id {
            return;
        }
        let (rank, usage) = if p.get("thread_token_usage").is_some_and(Value::is_object) {
            (3, &p["thread_token_usage"])
        } else if p.get("total_token_usage").is_some_and(Value::is_object) {
            (2, &p["total_token_usage"])
        } else {
            (1, &p["usage"])
        };
        if rank < self.usage_rank || !usage.is_object() {
            return;
        }
        let totals = (
            usage["input_tokens"].as_u64().unwrap_or(0),
            usage["output_tokens"].as_u64().unwrap_or(0),
        );
        if rank == 1 {
            let response = string(p, "response_id");
            if response.is_empty()
                || response.len() > 512
                || self.responses.len() >= MAX_IDS
                || !self.responses.insert(response)
            {
                return;
            }
            self.usage.0 = self.usage.0.saturating_add(totals.0);
            self.usage.1 = self.usage.1.saturating_add(totals.1);
        } else {
            self.usage = totals;
        }
        self.usage_rank = rank;
    }
    fn record(&mut self, v: Value, emit: bool, out: &mut Vec<(u64, StyledLine)>) {
        self.sequence += 1;
        if let Some(ts) = v["timestamp"]
            .as_str()
            .and_then(|s| parse_ts(s).ok())
            .or_else(|| v["timestamp"].as_f64())
        {
            self.started.get_or_insert(ts);
            self.last_ts = self.last_ts.max(ts);
        }
        let p = &v["payload"];
        match v["type"].as_str().unwrap_or("") {
            "session_meta" => {
                let id = string(p, "session_id");
                let id = if id.is_empty() { string(p, "id") } else { id };
                if !id.is_empty() {
                    self.id = id;
                }
                self.cwd = string(p, "cwd");
                let model = string(p, "model");
                if !model.is_empty() {
                    self.model = label(&model);
                }
            }
            "turn_context" => {
                let model = string(p, "model");
                if !model.is_empty() {
                    self.model = label(&model);
                }
                let turn = string(p, "turn_id");
                if !turn.is_empty() && self.turn.is_empty() {
                    self.turn = turn;
                }
            }
            "token_usage_record" => self.accounting(p),
            "response_item" => match p["type"].as_str().unwrap_or("") {
                "message" => {
                    let role = match p["role"].as_str() {
                        Some("user") => 0,
                        Some("assistant") => 1,
                        _ => return,
                    };
                    let text = message_text(p);
                    self.message(role, text, string(p, "id"), true, emit, out);
                }
                "custom_tool_call" => {
                    if self.tool_stream == 0 {
                        self.tool_stream = 1;
                    }
                    let id = string(p, "call_id");
                    if !self.unique(format!("call:{id}")) {
                        return;
                    }
                    let name = label(&string(p, "name"));
                    if !self.done && self.pending.len() < MAX_IDS {
                        self.pending.insert(id);
                    }
                    self.last_tool = if name.is_empty() { "-".into() } else { name };
                    self.last_event = Some(LastEvent::ToolUse(self.last_tool.clone()));
                    self.last_semantic = self.sequence;
                    self.last_line = label(&string(p, "input"));
                    if emit && self.tool_stream != 2 {
                        out.push((
                            self.sequence,
                            line(
                                Sem::Tool,
                                format!("▶ {} {}", self.last_tool, self.last_line),
                            ),
                        ));
                    }
                }
                "custom_tool_call_output" => {
                    if self.tool_stream == 0 {
                        self.tool_stream = 1;
                    }
                    let id = string(p, "call_id");
                    if !self.unique(format!("result:{id}")) {
                        return;
                    }
                    self.pending.remove(&id);
                    self.last_event = Some(LastEvent::ToolResult);
                    self.last_semantic = self.sequence;
                    self.last_line = label(&string(p, "output"));
                    if emit && self.tool_stream != 2 {
                        out.push((
                            self.sequence,
                            line(Sem::Dim, format!("  {}", self.last_line)),
                        ));
                    }
                }
                _ => {}
            },
            "event_msg" => match p["type"].as_str().unwrap_or("") {
                "task_started" => {
                    let turn = string(p, "turn_id");
                    if turn.is_empty() || turn != self.turn || self.done {
                        self.new_turn(turn, out);
                    }
                }
                "task_complete" => {
                    let turn = string(p, "turn_id");
                    if turn == self.turn {
                        self.done = true;
                        self.pending.clear();
                        self.completion_text =
                            Some((string(p, "last_agent_message"), emit, self.sequence));
                    }
                }
                "token_count" => {
                    let thread = string(p, "thread_id");
                    if thread.is_empty() || thread == self.id {
                        self.accounting(&p["info"]);
                    }
                }
                "item_completed" => {
                    let item = &p["item"];
                    let role = match item["type"].as_str() {
                        Some("UserMessage") => 0,
                        Some("AgentMessage") => 1,
                        Some("CommandExecution") => {
                            if self.tool_stream != 1 && self.commands.len() < 256 {
                                self.commands.push((
                                    string(item, "id"),
                                    label(&string(item, "command")),
                                    label(&string(item, "aggregated_output")),
                                    emit,
                                    self.sequence,
                                ));
                            }
                            return;
                        }
                        _ => return,
                    };
                    self.message(
                        role,
                        message_text(item),
                        string(item, "id"),
                        false,
                        emit,
                        out,
                    );
                }
                _ => {}
            },
            _ => {}
        }
    }
}
fn message_text(p: &Value) -> String {
    if let Some(text) = p["text"].as_str() {
        return text.into();
    }
    if let Some(text) = p["content"].as_str() {
        return text.into();
    }
    p["content"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|v| {
                    matches!(
                        v["type"].as_str(),
                        Some("input_text" | "output_text" | "text")
                    )
                })
                .filter_map(|v| v["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[derive(PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(unix)]
    changed: (i64, i64),
}
impl Stamp {
    fn new(m: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: m.len(),
            modified: m.modified().ok(),
            #[cfg(unix)]
            identity: (m.dev(), m.ino()),
            #[cfg(unix)]
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
    fn replaced(&self, old: &Self) -> bool {
        #[cfg(unix)]
        if self.identity != old.identity {
            return true;
        }
        self.len < old.len || (self.len == old.len && self != old)
    }
}

/// Validates every path component on each open, so a replaced directory cannot
/// redirect an existing tailer outside the configured store.
fn safe_file(root: &Path, path: &Path) -> std::io::Result<File> {
    let relative = path.strip_prefix(root).map_err(std::io::Error::other)?;
    let mut current = root.to_path_buf();
    if fs::symlink_metadata(&current)?.file_type().is_symlink() {
        return Err(std::io::Error::other("symlink store"));
    }
    for part in relative.components() {
        current.push(part);
        if fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Err(std::io::Error::other("symlink rollout"));
        }
    }
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(std::io::Error::other("not a rollout file"));
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a rollout file"));
    }
    Ok(file)
}

struct Reader {
    root: PathBuf,
    path: PathBuf,
    stamp: Option<Stamp>,
    offset: u64,
    start: u64,
    buffer: Vec<u8>,
    skipping: bool,
    anchor: Vec<u8>,
    unavailable: bool,
    malformed: bool,
    cutoff: u64,
    decoder: Decoder,
}
impl Reader {
    fn new(root: &Path, path: &Path, cutoff: u64) -> Self {
        Self {
            root: root.into(),
            path: path.into(),
            stamp: None,
            offset: 0,
            start: 0,
            buffer: Vec::new(),
            skipping: false,
            anchor: Vec::new(),
            unavailable: false,
            malformed: false,
            cutoff,
            decoder: Decoder::new(path),
        }
    }
    fn reset(&mut self) {
        self.offset = 0;
        self.start = 0;
        self.buffer.clear();
        self.skipping = false;
        self.anchor.clear();
        self.decoder = Decoder::new(&self.path);
        self.cutoff = 0;
        self.malformed = false;
    }
    fn poll(&mut self) -> std::io::Result<Vec<StyledLine>> {
        let mut out = Vec::new();
        let mut semantic = Vec::new();
        let mut file = safe_file(&self.root, &self.path)?;
        let stamp = Stamp::new(&file.metadata()?);
        let mut reset =
            self.unavailable || self.stamp.as_ref().is_some_and(|old| stamp.replaced(old));
        if !reset && !self.anchor.is_empty() && self.stamp.as_ref() != Some(&stamp) {
            file.seek(SeekFrom::Start(self.offset - self.anchor.len() as u64))?;
            let mut check = vec![0; self.anchor.len()];
            reset = file.read_exact(&mut check).is_err() || check != self.anchor;
        }
        if reset {
            self.reset();
            out.push(line(Sem::Dim, "· rollout replaced/truncated; restarting"));
        }
        self.unavailable = false;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        (&mut file).take(MAX_READ).read_to_end(&mut bytes)?;
        for byte in &bytes {
            self.offset += 1;
            if *byte == b'\n' {
                if !self.skipping {
                    match serde_json::from_slice::<Value>(&self.buffer) {
                        Ok(v) if v.is_object() => {
                            self.malformed = false;
                            self.decoder
                                .record(v, self.start >= self.cutoff, &mut semantic);
                        }
                        _ => {
                            if !self.malformed && self.start >= self.cutoff {
                                out.push(line(Sem::Dim, "· parse-skip"));
                            }
                            self.malformed = true;
                        }
                    }
                }
                self.buffer.clear();
                self.skipping = false;
                self.start = self.offset;
            } else if !self.skipping {
                if self.buffer.len() == MAX_RECORD {
                    self.buffer.clear();
                    self.skipping = true;
                    if !self.malformed && self.start >= self.cutoff {
                        out.push(line(Sem::Dim, "· oversized record skipped"));
                    }
                    self.malformed = true;
                } else {
                    self.buffer.push(*byte);
                }
            }
        }
        if !bytes.is_empty() {
            let n = self.offset.min(64) as usize;
            file.seek(SeekFrom::Start(self.offset - n as u64))?;
            self.anchor.resize(n, 0);
            file.read_exact(&mut self.anchor)?;
        }
        self.stamp = Some(stamp);
        self.decoder.flush(&mut semantic);
        semantic.sort_by_key(|(seq, _)| *seq);
        out.extend(semantic.into_iter().map(|(_, line)| line));
        Ok(out)
    }
}
impl Tailer for Reader {
    fn poll(&mut self) -> Vec<StyledLine> {
        match Reader::poll(self) {
            Ok(out) => out,
            Err(_) => {
                let first = !self.unavailable;
                self.unavailable = true;
                if first {
                    vec![line(Sem::Dim, "· rollout unavailable; waiting")]
                } else {
                    Vec::new()
                }
            }
        }
    }
}

/// Codex home directory, with rollouts under `sessions/**/*.jsonl`.
/// Reserved integration prefix: `X:` (registration belongs to integration).
pub struct CodexSource {
    root: PathBuf,
    files: HashMap<PathBuf, Reader>,
    ids: HashMap<String, PathBuf>,
}
impl CodexSource {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().join("sessions"),
            files: HashMap::new(),
            ids: HashMap::new(),
        }
    }
}
fn discover(root: &Path, depth: usize, out: &mut Vec<(PathBuf, f64)>) {
    if depth > 32
        || fs::symlink_metadata(root).map_or(true, |m| !m.is_dir() || m.file_type().is_symlink())
    {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            discover(&entry.path(), depth + 1, out);
        } else if kind.is_file() && entry.path().extension().is_some_and(|s| s == "jsonl") {
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(seconds)
                .unwrap_or(0.0);
            out.push((entry.path(), mtime));
        }
    }
}
impl Source for CodexSource {
    fn sessions(&mut self) -> Vec<SessionMeta> {
        let mut paths = Vec::new();
        discover(&self.root, 0, &mut paths);
        paths.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let found: HashSet<_> = paths.iter().map(|(p, _)| p.clone()).collect();
        self.files.retain(|p, _| found.contains(p));
        self.ids.clear();
        let mut sessions = Vec::new();
        for (path, mtime) in paths {
            let reader = self
                .files
                .entry(path.clone())
                .or_insert_with(|| Reader::new(&self.root, &path, u64::MAX));
            if reader.poll().is_err() {
                reader.unavailable = true;
                continue;
            }
            if let Some(meta) = reader.decoder.meta(mtime)
                && !self.ids.contains_key(&meta.id)
            {
                self.ids.insert(meta.id.clone(), path);
                sessions.push(meta);
            }
        }
        sessions
    }
    fn last_tool(&mut self, session_id: &str) -> String {
        self.ids
            .get(session_id)
            .and_then(|p| self.files.get(p))
            .map(|r| r.decoder.last_tool.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-".into())
    }
    fn open_tailer(&self, session_id: &str, replay: Replay) -> Option<Box<dyn Tailer>> {
        let path = self.ids.get(session_id)?;
        let len = safe_file(&self.root, path).ok()?.metadata().ok()?.len();
        Some(Box::new(Reader::new(
            &self.root,
            path,
            len.saturating_sub(replay.bytes),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn unchanged_ticks_never_reparse_and_bytes_are_raw() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        fs::write(&path, "{\"type\":\"unknown\",\"payload\":\"🦀\"}\n").unwrap();
        let mut r = Reader::new(dir.path(), &path, 0);
        r.poll().unwrap();
        let len = fs::metadata(&path).unwrap().len();
        assert_eq!(r.offset, len);
        assert_eq!(r.decoder.sequence, 1);
        for _ in 0..10 {
            assert!(r.poll().unwrap().is_empty());
        }
        assert_eq!(r.decoder.sequence, 1);
        fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"{}\n")
            .unwrap();
        r.poll().unwrap();
        assert_eq!(r.offset, len + 3);
        assert_eq!(r.decoder.sequence, 2);
    }

    #[test]
    fn read_and_record_limits_recover_on_later_ticks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        let mut bytes = vec![b'x'; MAX_READ as usize + 2];
        bytes.extend_from_slice(
            b"\n{\"type\":\"session_meta\",\"payload\":{\"id\":\"recovered\"}}\n",
        );
        fs::write(&path, &bytes).unwrap();
        let mut r = Reader::new(dir.path(), &path, 0);
        assert_eq!(r.poll().unwrap().len(), 1);
        assert_eq!(r.offset, MAX_READ);
        assert!(r.buffer.len() <= MAX_RECORD);
        r.poll().unwrap();
        assert_eq!(r.offset, bytes.len() as u64);
        assert_eq!(r.decoder.id, "recovered");
        assert!(!r.skipping);
    }
}
