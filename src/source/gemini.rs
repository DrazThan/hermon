//! Gemini CLI session source (`~/.gemini/tmp/<project>/chats/session-*.jsonl`).
//!
//! Exploratory v1: the on-disk store mixes a session header, `$set` state
//! patches, and direct message records. Treating every JSONL line as a new
//! chat message would duplicate content and misread activity. This module
//! reconstructs a normalized per-session message list and tails *that*,
//! not the raw patch stream.
//!
//! # Supported shapes (including Gemini CLI 0.46.0)
//!
//! - Header: `{sessionId, projectHash, startTime, lastUpdated, kind:"main"}`.
//! - Messages patch: `{"$set":{"messages":[{id,timestamp,type,content}],"lastUpdated"}}`.
//! - Metadata-only patch: `{"$set":{"lastUpdated"}}`.
//! - Direct records: `{id,timestamp,type,content}` with `user`, `info`, and
//!   `gemini` (assistant) types; `model` / `assistant` are also recognized.
//!   `content` is either a string or `[{"text":"…"}]`.
//! - Direct and `$set.messages` records may include `model` (e.g.
//!   `"gemini-3.5-flash"`) and `tokens: {input,output,cached,thoughts,tool,total}`.
//!   The latest non-empty model in normalized message order is reported;
//!   unsigned integer input/output counts are summed with saturation across
//!   current messages, so upserts and snapshots do not double-count. Other
//!   token fields are ignored. Missing/malformed metadata and log-only
//!   records retain `"unknown"` / zero fallbacks.
//! - Optional `<project>/logs.json`: a JSON array of
//!   `{sessionId,messageId,type:"user"|"model",message,timestamp}`. Project-wide;
//!   must be filtered by `sessionId`. Observed local logs included user
//!   records; the model role is from the captured format description.
//!
//! # Unsupported (follow-up, not blocking v1)
//!
//! No verified complete tool/multi-turn exchange was available. v1 does
//! **not** map `LastEvent::ToolUse` / `LastEvent::ToolResult`, infer calls
//! from prose, populate token/cost/model fields from config, or claim
//! permission/stuck detection. `last_tool` is `"-"`, `tool_pending` /
//! `turn_done` / `ended` / `force_live` are false, `cost` is `None`. A sanitized
//! real tool-call fixture plus tool, completion, and cost mapping is
//! follow-up work.
//!
//! # Replay
//!
//! Gemini JSONL is a patch stream. A byte seek from the end can omit the
//! header and the `$set.messages` snapshot later lines assume. [`Replay::bytes`]
//! is therefore a bound on *normalized message text bytes* after
//! reconstructing supported state from the beginning (capped by
//! [`MAX_JSONL_RECORDS`] / [`MAX_MESSAGES`]). This is the format-specific
//! clamp [`Replay`] permits. `bytes == 0` emits no existing messages but
//! still initializes state so later polls stream only new/changed messages.
//! Unchanged ticks reuse the reconstructed cache; they do not reread the
//! whole file.
//!
//! # Limits
//!
//! - [`MAX_JSONL_RECORDS`]: complete JSONL lines ingested per session.
//! - [`MAX_MESSAGES`]: normalized messages retained per session.
//! - [`MAX_LOGS_ENTRIES`]: `logs.json` array entries parsed.
//! - [`MAX_LINE_BYTES`]: a longer complete or partial line is dropped.
//!
//! Hitting a cap yields one dim notice on the tail and keeps the partial
//! state rather than spinning or panicking.
//!
//! # Security
//!
//! Read-only. Discovery stays inside configured `tmp/` and does not follow
//! directory (or file) symlinks that escape it. `.project_root` is display
//! text, never a path to traverse. Unknown session ids cannot open
//! arbitrary files.

use std::collections::HashMap;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::render::gemini::{
    GeminiRole, dim_status, limit_notice, render_gemini_message, revision_notice,
};
use crate::render::{clip, parse_ts};
use crate::source::{LastEvent, Replay, SessionMeta, Source, Tailer};

/// Roster / agent prefix reserved for this source. Integration (#107) owns
/// CLI flags and roster registration; the protocol already carries
/// source-qualified ids of this form without a schema change.
pub const PREFIX: &str = "Gm";

/// Complete JSONL lines ingested from one session file.
pub const MAX_JSONL_RECORDS: usize = 8_192;
/// Normalized messages retained per session (transcript + logs fallback).
pub const MAX_MESSAGES: usize = 2_048;
/// `logs.json` array entries parsed per project.
pub const MAX_LOGS_ENTRIES: usize = 4_096;
/// Longer complete/partial lines are dropped rather than buffered forever.
pub const MAX_LINE_BYTES: usize = 1_048_576;
/// First-N bytes fingerprinted to detect equal-size / larger replacement.
const PREFIX_FINGERPRINT: usize = 64;
/// Preview clip for titles and roster last-line.
const PREVIEW_CLIP: usize = 120;

/// Gemini home (`~/.gemini`). Discovers `tmp/<project>/chats/session-*.jsonl`.
pub struct GeminiSource {
    /// Jail for symlink confinement: `root/tmp`.
    tmp: PathBuf,
    stores: HashMap<PathBuf, SessionStore>,
    /// Public id and filename fallback id → transcript path. Never a raw
    /// user-supplied filesystem path.
    by_id: HashMap<String, PathBuf>,
    logs: HashMap<PathBuf, LogsCache>,
}

impl GeminiSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = expand_tilde(root.into());
        let tmp = root.join("tmp");
        GeminiSource {
            tmp,
            stores: HashMap::new(),
            by_id: HashMap::new(),
            logs: HashMap::new(),
        }
    }

    fn session_meta(&mut self, disc: Discovered) -> Option<SessionMeta> {
        let logs = self.logs_for(&disc.project_dir);
        let store = self
            .stores
            .entry(disc.path.clone())
            .or_insert_with(|| SessionStore::new(&disc));
        store.sync();
        store.apply_logs(&logs);
        let meta = store.meta();
        Some(meta)
    }

    fn logs_for(&mut self, project_dir: &Path) -> Vec<LogEntry> {
        let path = project_dir.join("logs.json");
        let cache = self
            .logs
            .entry(project_dir.to_path_buf())
            .or_insert_with(|| LogsCache::new(path, self.tmp.clone()));
        cache.refresh();
        cache.entries.clone()
    }

    fn rebuild_index(&mut self, live_paths: &[PathBuf]) {
        self.stores.retain(|p, _| live_paths.iter().any(|l| l == p));
        self.by_id.clear();
        for (path, store) in &self.stores {
            self.by_id.insert(store.filename_id.clone(), path.clone());
            if let Some(id) = &store.header_id {
                self.by_id.insert(id.clone(), path.clone());
            }
        }
    }

    #[cfg(test)]
    pub fn test_logs_reads(&self) -> u32 {
        self.logs.values().map(|c| c.reads).sum()
    }
}

impl Default for GeminiSource {
    fn default() -> Self {
        let root = dirs::home_dir().unwrap_or_default().join(".gemini");
        GeminiSource::new(root)
    }
}

impl Source for GeminiSource {
    fn sessions(&mut self) -> Vec<SessionMeta> {
        let discovered = discover_sessions(&self.tmp);
        let live_paths: Vec<PathBuf> = discovered.iter().map(|d| d.path.clone()).collect();
        let mut out = Vec::new();
        for disc in discovered {
            if let Some(meta) = self.session_meta(disc) {
                out.push(meta);
            }
        }
        self.rebuild_index(&live_paths);
        out
    }

    fn last_tool(&mut self, session_id: &str) -> String {
        // No verified tool shape in the v1 subset.
        let _ = session_id;
        "-".to_string()
    }

    fn open_tailer(&self, session_id: &str, replay: Replay) -> Option<Box<dyn Tailer>> {
        // Unknown ids must not be treated as paths.
        if looks_like_path(session_id) {
            return None;
        }
        let path = self.by_id.get(session_id)?.clone();
        if !path_within(&path, &self.tmp) {
            return None;
        }
        Some(Box::new(GeminiTailer::new(path, self.tmp.clone(), replay)))
    }
}

fn looks_like_path(id: &str) -> bool {
    id.contains('/') || id.contains('\\') || id.contains("..")
}

/// Live tail of one Gemini session. Reconstructs supported state from the
/// beginning (with caps) rather than seeking into the patch stream, then
/// emits only new/changed semantic messages. Replacement that edits or
/// removes prior messages yields a dim revision notice plus a replay-bounded
/// current context — the pane cannot erase old lines.
pub struct GeminiTailer {
    jail: PathBuf,
    replay_bytes: u64,
    store: SessionStore,
    logs: LogsCache,
    emitted: Vec<MsgFp>,
    first_poll: bool,
    warned_missing: bool,
    warned_limit: bool,
}

impl GeminiTailer {
    pub fn new(path: impl Into<PathBuf>, jail: impl Into<PathBuf>, replay: Replay) -> Self {
        let path = path.into();
        let jail = jail.into();
        let disc = Discovered::from_path(&path);
        let logs_path =
            project_logs_path(&path).unwrap_or_else(|| path.with_file_name("logs.json"));
        GeminiTailer {
            jail: jail.clone(),
            replay_bytes: replay.bytes,
            store: SessionStore::new(&disc),
            logs: LogsCache::new(logs_path, jail),
            emitted: Vec::new(),
            first_poll: true,
            warned_missing: false,
            warned_limit: false,
        }
    }

    fn poll_impl(&mut self) -> Vec<crate::render::StyledLine> {
        let mut out = Vec::new();
        if !path_within(&self.store.path, &self.jail) {
            if self.warned_missing {
                return Vec::new();
            }
            self.warned_missing = true;
            return vec![dim_status("· transcript not found — waiting")];
        }

        let event = self.store.sync();
        match event {
            SyncEvent::Missing => {
                if self.warned_missing {
                    return Vec::new();
                }
                self.warned_missing = true;
                let msg = if self.first_poll {
                    "· transcript not found — waiting"
                } else {
                    "· transcript removed — waiting for it to return"
                };
                return vec![dim_status(msg)];
            }
            SyncEvent::Truncated => {
                out.push(dim_status("· transcript truncated — reloading"));
            }
            SyncEvent::Replaced => {
                out.push(revision_notice());
            }
            SyncEvent::Unchanged | SyncEvent::Appended | SyncEvent::Loaded => {}
        }
        self.warned_missing = false;
        self.logs.refresh();
        self.store.apply_logs(&self.logs.entries);

        if self.store.hit_limit && !self.warned_limit {
            self.warned_limit = true;
            out.push(limit_notice());
        }

        let fps = self.store.fingerprint();
        if self.first_poll {
            self.first_poll = false;
            if self.replay_bytes > 0 {
                out.extend(self.store.render_suffix(self.replay_bytes));
            }
            self.emitted = fps;
            return out;
        }

        match diff_fps(&self.emitted, &fps) {
            FpDiff::Same => {}
            FpDiff::Append(from) => {
                out.extend(self.store.render_from(from));
            }
            FpDiff::Revision => {
                if !out.iter().any(|l| l.to_plain().contains("revised")) {
                    out.push(revision_notice());
                }
                out.extend(self.store.render_suffix(self.replay_bytes.max(1)));
            }
        }
        self.emitted = fps;
        out
    }
}

impl Tailer for GeminiTailer {
    fn poll(&mut self) -> Vec<crate::render::StyledLine> {
        self.poll_impl()
    }
}

#[derive(Debug, Clone)]
struct Discovered {
    path: PathBuf,
    project_dir: PathBuf,
    project_label: String,
    cwd: Option<String>,
    filename_id: String,
}

impl Discovered {
    fn from_path(path: &Path) -> Self {
        let filename_id = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(parse_session_filename)
            .unwrap_or_else(|| "unknown".to_string());
        let chats = path.parent();
        let project_dir = chats
            .and_then(|p| p.parent())
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        let project_label = project_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let cwd = read_project_root(&project_dir.join(".project_root"));
        Discovered {
            path: path.to_path_buf(),
            project_dir,
            project_label,
            cwd,
            filename_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncEvent {
    Missing,
    Unchanged,
    Appended,
    Truncated,
    Replaced,
    Loaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
    Info,
    System,
    Other,
}

impl Role {
    fn from_type(t: &str) -> Self {
        match t {
            "user" => Role::User,
            "gemini" | "model" | "assistant" => Role::Assistant,
            "info" => Role::Info,
            "system" => Role::System,
            _ => Role::Other,
        }
    }

    fn gemini_role(self) -> GeminiRole {
        match self {
            Role::User => GeminiRole::User,
            Role::Assistant => GeminiRole::Assistant,
            Role::Info => GeminiRole::Info,
            Role::System => GeminiRole::System,
            Role::Other => GeminiRole::Other,
        }
    }
}

#[derive(Debug, Clone)]
struct NormMsg {
    id: String,
    ts: f64,
    order: u64,
    role: Role,
    text: String,
    model: Option<String>,
    in_tok: u64,
    out_tok: u64,
    scaffold: bool,
    from_logs: bool,
}

impl NormMsg {
    fn conversational(&self) -> bool {
        !self.scaffold
            && !self.text.trim().is_empty()
            && matches!(self.role, Role::User | Role::Assistant)
    }

    fn visible(&self) -> bool {
        !self.scaffold && !self.text.trim().is_empty()
    }

    fn fp(&self) -> MsgFp {
        MsgFp {
            id: self.id.clone(),
            hash: content_hash(&self.text, self.role),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MsgFp {
    id: String,
    hash: u64,
}

#[derive(Debug)]
enum FpDiff {
    Same,
    Append(usize),
    Revision,
}

fn diff_fps(old: &[MsgFp], new: &[MsgFp]) -> FpDiff {
    if old == new {
        return FpDiff::Same;
    }
    if new.len() >= old.len() && new[..old.len()] == *old {
        return FpDiff::Append(old.len());
    }
    FpDiff::Revision
}

fn content_hash(text: &str, role: Role) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    std::mem::discriminant(&role).hash(&mut h);
    h.finish()
}

struct SessionStore {
    path: PathBuf,
    filename_id: String,
    project_label: String,
    cwd: Option<String>,
    offset: u64,
    size: u64,
    mtime: Option<SystemTime>,
    prefix: Vec<u8>,
    records_read: usize,
    next_order: u64,
    hit_limit: bool,
    header_id: Option<String>,
    started_at: Option<f64>,
    last_updated: Option<f64>,
    messages: Vec<NormMsg>,
    by_id: HashMap<String, usize>,
    /// Last activity from matching logs.json entries, never the file mtime.
    logs_last_ts: Option<f64>,
}

impl SessionStore {
    fn new(disc: &Discovered) -> Self {
        SessionStore {
            path: disc.path.clone(),
            filename_id: disc.filename_id.clone(),
            project_label: disc.project_label.clone(),
            cwd: disc.cwd.clone(),
            offset: 0,
            size: 0,
            mtime: None,
            prefix: Vec::new(),
            records_read: 0,
            next_order: 0,
            hit_limit: false,
            header_id: None,
            started_at: None,
            last_updated: None,
            messages: Vec::new(),
            by_id: HashMap::new(),
            logs_last_ts: None,
        }
    }

    fn reset_parse(&mut self) {
        self.offset = 0;
        self.records_read = 0;
        self.next_order = 0;
        self.hit_limit = false;
        self.header_id = None;
        self.started_at = None;
        self.last_updated = None;
        self.messages.clear();
        self.by_id.clear();
        self.logs_last_ts = None;
        self.prefix.clear();
    }

    fn public_id(&self) -> &str {
        self.header_id.as_deref().unwrap_or(&self.filename_id)
    }

    fn sync(&mut self) -> SyncEvent {
        let Ok(meta) = fs::metadata(&self.path) else {
            // Drop reconstructed state so a later recreate is read from
            // byte zero rather than appended onto a ghost offset.
            if self.offset > 0 || !self.messages.is_empty() {
                self.reset_parse();
                self.size = 0;
                self.mtime = None;
            }
            return SyncEvent::Missing;
        };
        let size = meta.len();
        let mtime = meta.modified().ok();
        let prefix = read_prefix(&self.path, size);

        if size < self.offset {
            self.reset_parse();
            self.size = size;
            self.mtime = mtime;
            self.prefix = prefix;
            self.read_from(0);
            return SyncEvent::Truncated;
        }

        let replaced = !self.prefix.is_empty() && !prefix.is_empty() && prefix != self.prefix
            || (size == self.size
                && size > 0
                && self.offset == size
                && mtime != self.mtime
                && mtime.is_some());

        if replaced && self.offset > 0 {
            self.reset_parse();
            self.size = size;
            self.mtime = mtime;
            self.prefix = prefix;
            self.read_from(0);
            return SyncEvent::Replaced;
        }

        if size == self.offset && mtime == self.mtime {
            return SyncEvent::Unchanged;
        }

        let first = self.offset == 0 && self.records_read == 0;
        let start = self.offset;
        self.size = size;
        self.mtime = mtime;
        if self.prefix.is_empty() {
            self.prefix = prefix;
        }
        self.read_from(start);
        if first {
            SyncEvent::Loaded
        } else {
            SyncEvent::Appended
        }
    }

    fn read_from(&mut self, start: u64) {
        let Ok(file) = File::open(&self.path) else {
            return;
        };
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(start)).is_err() {
            return;
        }
        self.offset = start;
        let mut raw = Vec::new();
        loop {
            if self.records_read >= MAX_JSONL_RECORDS {
                self.hit_limit = true;
                return;
            }
            raw.clear();
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    if !raw.ends_with(b"\n") {
                        return;
                    }
                    if raw.len() > MAX_LINE_BYTES {
                        self.offset += raw.len() as u64;
                        self.records_read += 1;
                        continue;
                    }
                    self.offset += raw.len() as u64;
                    self.records_read += 1;
                    self.ingest(&String::from_utf8_lossy(&raw));
                }
            }
        }
    }

    fn ingest(&mut self, raw: &str) {
        let raw = raw.trim();
        if raw.is_empty() {
            return;
        }
        let Ok(Value::Object(ev)) = serde_json::from_str::<Value>(raw) else {
            return;
        };
        if ev.get("sessionId").and_then(Value::as_str).is_some() {
            self.ingest_header(&ev);
            return;
        }
        if let Some(Value::Object(set)) = ev.get("$set") {
            self.ingest_set(set);
            return;
        }
        if ev.get("id").and_then(Value::as_str).is_some()
            && ev.get("type").is_some()
            && let Some(msg) = parse_message(&ev, self.next_order)
        {
            self.next_order += 1;
            self.upsert_message(msg);
        }
        // Unknown records (unsupported patch ops, junk) are ignored.
    }

    fn ingest_header(&mut self, ev: &serde_json::Map<String, Value>) {
        if let Some(id) = ev.get("sessionId").and_then(Value::as_str)
            && is_safe_id(id)
        {
            self.header_id = Some(id.to_string());
        }
        if let Some(ts) = ev.get("startTime").and_then(parse_timestamp) {
            self.started_at = Some(ts);
        }
        if let Some(ts) = ev.get("lastUpdated").and_then(parse_timestamp) {
            self.last_updated = Some(ts);
        }
    }

    fn ingest_set(&mut self, set: &serde_json::Map<String, Value>) {
        if let Some(ts) = set.get("lastUpdated").and_then(parse_timestamp) {
            self.last_updated = Some(ts);
        }
        if let Some(Value::Array(msgs)) = set.get("messages") {
            // Replacement of normalized message state, not append-all.
            self.messages.clear();
            self.by_id.clear();
            for (i, m) in msgs.iter().enumerate() {
                let Some(obj) = m.as_object() else {
                    continue;
                };
                let Some(msg) = parse_message(obj, self.next_order * 1000 + i as u64) else {
                    continue;
                };
                self.upsert_message(msg);
            }
            self.next_order += 1;
        }
        // Unknown $set keys are ignored; no JSON-patch interpreter.
    }

    fn upsert_message(&mut self, msg: NormMsg) {
        if let Some(&idx) = self.by_id.get(&msg.id) {
            self.messages[idx].ts = msg.ts;
            self.messages[idx].role = msg.role;
            self.messages[idx].text = msg.text;
            self.messages[idx].model = msg.model;
            self.messages[idx].in_tok = msg.in_tok;
            self.messages[idx].out_tok = msg.out_tok;
            self.messages[idx].scaffold = msg.scaffold;
            self.messages[idx].from_logs = msg.from_logs;
            // Keep original `order` so equal timestamps stay stable.
            self.reindex();
            return;
        }
        if self.messages.len() >= MAX_MESSAGES {
            self.hit_limit = true;
            return;
        }
        self.by_id.insert(msg.id.clone(), self.messages.len());
        self.messages.push(msg);
        self.reindex();
    }

    fn reindex(&mut self) {
        self.messages
            .sort_by(|a, b| a.ts.total_cmp(&b.ts).then(a.order.cmp(&b.order)));
        self.by_id.clear();
        for (i, m) in self.messages.iter().enumerate() {
            self.by_id.insert(m.id.clone(), i);
        }
    }

    fn apply_logs(&mut self, entries: &[LogEntry]) {
        self.messages.retain(|m| !m.from_logs);
        self.reindex();
        let sid = self.public_id().to_string();
        self.logs_last_ts = None;
        for e in entries {
            if e.session_id != sid {
                continue;
            }
            self.logs_last_ts = Some(self.logs_last_ts.unwrap_or(0.0).max(e.ts));
            if self.by_id.contains_key(&e.message_id) {
                // Transcript wins conflicts.
                continue;
            }
            if self.messages.len() >= MAX_MESSAGES {
                self.hit_limit = true;
                continue;
            }
            let msg = NormMsg {
                id: e.message_id.clone(),
                ts: e.ts,
                order: self.next_order,
                role: e.role,
                text: e.text.clone(),
                model: None,
                in_tok: 0,
                out_tok: 0,
                scaffold: is_session_context(&e.text),
                from_logs: true,
            };
            self.next_order += 1;
            self.upsert_message(msg);
        }
    }

    fn fingerprint(&self) -> Vec<MsgFp> {
        self.messages
            .iter()
            .filter(|m| m.visible())
            .map(NormMsg::fp)
            .collect()
    }

    fn render_from(&self, from: usize) -> Vec<crate::render::StyledLine> {
        let vis: Vec<&NormMsg> = self.messages.iter().filter(|m| m.visible()).collect();
        vis.into_iter()
            .skip(from)
            .flat_map(|m| render_gemini_message(m.role.gemini_role(), &m.text, m.scaffold))
            .collect()
    }

    fn render_suffix(&self, budget: u64) -> Vec<crate::render::StyledLine> {
        if budget == 0 {
            return Vec::new();
        }
        let vis: Vec<&NormMsg> = self.messages.iter().filter(|m| m.visible()).collect();
        if vis.is_empty() {
            return Vec::new();
        }
        let mut used = 0u64;
        let mut start = vis.len();
        while start > 0 {
            let idx = start - 1;
            let n = vis[idx].text.len() as u64;
            if used > 0 && used + n > budget {
                break;
            }
            used += n;
            start = idx;
            if used >= budget {
                break;
            }
        }
        vis[start..]
            .iter()
            .flat_map(|m| render_gemini_message(m.role.gemini_role(), &m.text, m.scaffold))
            .collect()
    }

    fn meta(&self) -> SessionMeta {
        let mtime = mtime_secs(&self.path);
        let mut last_ts = self.last_updated.or(self.started_at).unwrap_or(0.0);
        for m in &self.messages {
            if m.ts > last_ts {
                last_ts = m.ts;
            }
        }
        if let Some(ts) = self.logs_last_ts {
            last_ts = last_ts.max(ts);
        }
        if let Some(mt) = mtime {
            last_ts = last_ts.max(mt);
        }
        let started_at = self
            .started_at
            .or_else(|| {
                self.messages
                    .iter()
                    .map(|m| m.ts)
                    .find(|t| *t > 0.0)
                    .or(mtime)
            })
            .unwrap_or(last_ts);

        let title = self
            .messages
            .iter()
            .find(|m| m.conversational() && m.role == Role::User)
            .map(|m| clip(&m.text, PREVIEW_CLIP))
            .filter(|s| !s.is_empty())
            .or_else(|| self.cwd.clone())
            .unwrap_or_else(|| self.project_label.clone());

        let (last_line, last_event) = self
            .messages
            .iter()
            .rev()
            .find(|m| m.conversational())
            .map(|m| {
                let line = match m.role {
                    Role::User => format!("» {}", clip(&m.text, PREVIEW_CLIP)),
                    Role::Assistant => clip(&m.text, PREVIEW_CLIP),
                    _ => clip(&m.text, PREVIEW_CLIP),
                };
                let event = match m.role {
                    Role::User => Some(LastEvent::User),
                    Role::Assistant => Some(LastEvent::AssistantText),
                    _ => None,
                };
                (line, event)
            })
            .unwrap_or_default();

        let model = self
            .messages
            .iter()
            .rev()
            .find_map(|m| m.model.as_deref())
            .unwrap_or("unknown")
            .to_string();
        // Aggregate current normalized state, never the raw patch stream.
        let (in_tok, out_tok) = self
            .messages
            .iter()
            .fold((0u64, 0u64), |(input, output), m| {
                (
                    input.saturating_add(m.in_tok),
                    output.saturating_add(m.out_tok),
                )
            });

        SessionMeta {
            id: self.public_id().to_string(),
            started_at,
            ended: false,
            model,
            title,
            in_tok,
            out_tok,
            cost: None,
            last_ts,
            turn_done: false,
            tool_pending: false,
            force_live: false,
            last_tool: "-".to_string(),
            last_line,
            last_event,
        }
    }
}

#[derive(Debug, Clone)]
struct LogEntry {
    session_id: String,
    message_id: String,
    role: Role,
    text: String,
    ts: f64,
}

struct LogsCache {
    path: PathBuf,
    jail: PathBuf,
    size: u64,
    mtime: Option<SystemTime>,
    entries: Vec<LogEntry>,
    #[cfg_attr(not(test), allow(dead_code))]
    reads: u32,
}

impl LogsCache {
    fn new(path: PathBuf, jail: PathBuf) -> Self {
        LogsCache {
            path,
            jail,
            size: 0,
            mtime: None,
            entries: Vec::new(),
            reads: 0,
        }
    }

    fn refresh(&mut self) {
        if !path_within(&self.path, &self.jail) && fs::metadata(&self.path).is_ok() {
            // Present but outside the jail: ignore, keep last valid.
            return;
        }
        let Ok(meta) = fs::metadata(&self.path) else {
            return;
        };
        let size = meta.len();
        let mtime = meta.modified().ok();
        if size == self.size && mtime == self.mtime && self.reads > 0 {
            return;
        }
        self.reads = self.reads.saturating_add(1);
        match parse_logs_file(&self.path) {
            Some(entries) => {
                self.entries = entries;
                self.size = size;
                self.mtime = mtime;
            }
            None => {
                // Partial/atomic replacement: retain last valid until a
                // later poll retries. Do not update size/mtime so we retry.
            }
        }
    }
}

fn parse_logs_file(path: &Path) -> Option<Vec<LogEntry>> {
    let bytes = fs::read(path).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let arr = value.as_array()?;
    let mut out = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        if i >= MAX_LOGS_ENTRIES {
            break;
        }
        let Some(obj) = item.as_object() else {
            continue;
        };
        let Some(session_id) = obj.get("sessionId").and_then(Value::as_str) else {
            continue;
        };
        let message_id = json_id(obj.get("messageId")).unwrap_or_else(|| format!("log-{i}"));
        let role = obj
            .get("type")
            .and_then(Value::as_str)
            .map(Role::from_type)
            .unwrap_or(Role::Other);
        let text = obj
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let ts = obj
            .get("timestamp")
            .and_then(parse_timestamp)
            .unwrap_or(0.0);
        out.push(LogEntry {
            session_id: session_id.to_string(),
            message_id,
            role,
            text,
            ts,
        });
    }
    Some(out)
}

fn json_id(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn parse_message(obj: &serde_json::Map<String, Value>, order: u64) -> Option<NormMsg> {
    let id = json_id(obj.get("id"))?;
    if !is_safe_id(&id) {
        return None;
    }
    let role = obj
        .get("type")
        .and_then(Value::as_str)
        .map(Role::from_type)
        .unwrap_or(Role::Other);
    let text = content_text(obj.get("content"));
    let ts = obj
        .get("timestamp")
        .and_then(parse_timestamp)
        .unwrap_or(0.0);
    Some(NormMsg {
        id,
        ts,
        order,
        role,
        scaffold: is_session_context(&text),
        from_logs: false,
        text,
        model: obj
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string),
        in_tok: obj
            .get("tokens")
            .and_then(|t| t.get("input"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        out_tok: obj
            .get("tokens")
            .and_then(|t| t.get("output"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

pub(crate) fn is_session_context(text: &str) -> bool {
    text.contains("<session_context>")
}

fn parse_timestamp(v: &Value) -> Option<f64> {
    if let Some(s) = v.as_str() {
        parse_ts(s).ok()
    } else {
        v.as_f64().map(|n| if n > 1e12 { n / 1000.0 } else { n })
    }
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Filename `session-<timestamp>-<id>.jsonl` → validated identifier.
pub(crate) fn parse_session_filename(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".jsonl")?;
    let rest = stem.strip_prefix("session-")?;
    if rest.is_empty() || rest.contains('/') || rest.contains('\\') || rest.contains("..") {
        return None;
    }
    let id = rest.rsplit_once('-').map(|(_, id)| id).unwrap_or(rest);
    if is_safe_id(id) {
        Some(id.to_string())
    } else {
        None
    }
}

fn read_project_root(path: &Path) -> Option<String> {
    // Text only: never traverse, never interpret as an instruction.
    let text = fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn project_logs_path(session_path: &Path) -> Option<PathBuf> {
    let chats = session_path.parent()?;
    if chats.file_name()? != "chats" {
        return None;
    }
    Some(chats.parent()?.join("logs.json"))
}

fn discover_sessions(tmp: &Path) -> Vec<Discovered> {
    let Ok(tmp_canon) = fs::canonicalize(tmp) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let Ok(projects) = fs::read_dir(tmp) else {
        return Vec::new();
    };
    for entry in projects.flatten() {
        let project = entry.path();
        if !dir_within(&project, &tmp_canon) {
            continue;
        }
        let project_label = entry.file_name().to_string_lossy().into_owned();
        let cwd = read_project_root(&project.join(".project_root"));
        let chats = project.join("chats");
        if !dir_within(&chats, &tmp_canon) {
            continue;
        }
        let Ok(files) = fs::read_dir(&chats) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            let name = file.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(filename_id) = parse_session_filename(name) else {
                continue;
            };
            if !file_within(&path, &tmp_canon) {
                continue;
            }
            out.push(Discovered {
                path,
                project_dir: project.clone(),
                project_label: project_label.clone(),
                cwd: cwd.clone(),
                filename_id,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn dir_within(path: &Path, jail: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_dir() && !meta.file_type().is_symlink() {
        return false;
    }
    let Ok(canon) = fs::canonicalize(path) else {
        return false;
    };
    let Ok(dir_meta) = fs::metadata(&canon) else {
        return false;
    };
    dir_meta.is_dir() && canon.starts_with(jail)
}

fn file_within(path: &Path, jail: &Path) -> bool {
    let Ok(canon) = fs::canonicalize(path) else {
        return false;
    };
    let Ok(meta) = fs::metadata(&canon) else {
        return false;
    };
    meta.is_file() && canon.starts_with(jail)
}

fn path_within(path: &Path, jail: &Path) -> bool {
    let Ok(jail) = fs::canonicalize(jail) else {
        return false;
    };
    match fs::canonicalize(path) {
        Ok(canon) => canon.starts_with(&jail),
        Err(_) => {
            // Missing is fine to attempt; a parent that escapes is not.
            path.starts_with(&jail)
                || path
                    .parent()
                    .and_then(|p| fs::canonicalize(p).ok())
                    .is_some_and(|p| p.starts_with(&jail))
        }
    }
}

fn read_prefix(path: &Path, size: u64) -> Vec<u8> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let n = (size as usize).min(PREFIX_FINGERPRINT);
    let mut buf = vec![0u8; n];
    use std::io::Read;
    match file.read(&mut buf) {
        Ok(got) => {
            buf.truncate(got);
            buf
        }
        Err(_) => Vec::new(),
    }
}

fn mtime_secs(path: &Path) -> Option<f64> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
}

fn expand_tilde(path: PathBuf) -> PathBuf {
    match path.to_str() {
        Some(s) => match s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
            Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
            None => path,
        },
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_id_from_observed_pattern() {
        assert_eq!(
            parse_session_filename("session-2026-09-15T18-35-952aecd6.jsonl").as_deref(),
            Some("952aecd6")
        );
        assert_eq!(
            parse_session_filename("session-abc.jsonl").as_deref(),
            Some("abc")
        );
        assert!(parse_session_filename("not-a-session.jsonl").is_none());
        assert!(parse_session_filename("session-foo/../etc.jsonl").is_none());
        assert!(parse_session_filename("session-..-x.jsonl").is_none());
    }

    #[test]
    fn session_context_tag_is_detected() {
        assert!(is_session_context(
            "<session_context>\nThis is the Gemini CLI.\n</session_context>"
        ));
        assert!(!is_session_context("Reply with exactly: OK"));
    }

    #[test]
    fn prefix_is_multi_character() {
        assert_eq!(PREFIX, "Gm");
        assert!(PREFIX.len() > 1);
        let key = format!("{PREFIX}:952aecd6-8ec1-4ece-b920-e272d272bfac");
        let (p, id) = key.split_once(':').expect("colon");
        assert_eq!(p, "Gm");
        assert_eq!(id, "952aecd6-8ec1-4ece-b920-e272d272bfac");
    }

    #[test]
    fn logs_json_is_reread_only_on_change() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir.path();
        let chats = home.join("tmp/demo/chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(
            chats.join("session-2026-09-15T18-00-aaa11111.jsonl"),
            concat!(
                r#"{"sessionId":"aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa","projectHash":"x","startTime":"2026-09-15T18:00:00.000Z","lastUpdated":"2026-09-15T18:00:00.000Z","kind":"main"}"#,
                "\n",
            ),
        )
        .unwrap();
        fs::write(
            home.join("tmp/demo/logs.json"),
            r#"[{"sessionId":"aaaaaaaa-1111-4000-8000-aaaaaaaaaaaa","messageId":"log1","type":"user","message":"cached","timestamp":"2026-09-15T18:00:01.000Z"}]"#,
        )
        .unwrap();
        let mut src = GeminiSource::new(home);
        assert_eq!(src.sessions()[0].title, "cached");
        let reads = src.test_logs_reads();
        assert_eq!(src.sessions()[0].title, "cached");
        assert_eq!(src.test_logs_reads(), reads, "unchanged array is cached");
    }
}
