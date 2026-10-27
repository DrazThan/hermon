//! Ownership and routing sidecars. Display labels never authorize I/O.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::render::{StyledLine, clip, sanitize};
use crate::roster::RosterRow;
use crate::source::Liveness;

#[derive(Debug, Clone, Default)]
pub struct MonitorOptions {
    pub hermes_home: Option<PathBuf>,
    pub hermes_db: Option<PathBuf>,
    pub hermes_log: Option<PathBuf>,
    pub bot: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotOwner {
    pub store: String,
    pub slug: String,
    pub label: String,
    #[serde(default)]
    pub origin: String,
}

impl BotOwner {
    pub fn display(&self) -> String {
        let label = display(&self.label);
        if self.origin.is_empty() {
            label
        } else {
            format!("{}/{}", display(&self.origin), label)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BotInfo {
    #[serde(flatten)]
    pub owner: BotOwner,
    pub available: bool,
    pub last_activity: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TickerEntry {
    pub owner: Option<BotOwner>,
    pub line: StyledLine,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RosterContext {
    pub owners: HashMap<String, BotOwner>,
    pub bots: Vec<BotInfo>,
    pub warnings: Vec<String>,
    pub ticker: Vec<TickerEntry>,
}

impl RosterContext {
    pub fn retain_open_owners(&mut self, previous: &Self, open: &[String], rows: &[RosterRow]) {
        for key in open
            .iter()
            .take(128)
            .filter(|key| !rows.iter().any(|r| &r.id == *key))
        {
            if !self.owners.contains_key(key)
                && let Some(owner) = previous.owners.get(key)
            {
                self.owners.insert(key.clone(), owner.clone());
            }
        }
    }

    pub fn owner_label(&self, id: &str) -> String {
        self.owners
            .get(id)
            .map_or_else(|| "owner unknown".into(), BotOwner::display)
    }
    pub fn label(&self, row: &RosterRow) -> String {
        format!("{} · {}", row.key, self.owner_label(&row.id))
    }
    pub fn overview(&self, rows: &[RosterRow]) -> Vec<String> {
        let mut lines = vec!["All discovered bots; counts in current session window".into()];
        for bot in &self.bots {
            let mut active = 0;
            let mut attention = 0;
            let mut done = 0;
            for row in rows.iter().filter(|r| {
                self.owners
                    .get(&r.id)
                    .is_some_and(|o| o.store == bot.owner.store)
            }) {
                match row.state {
                    Liveness::Live => active += 1,
                    Liveness::Attention(_) => attention += 1,
                    Liveness::Done => done += 1,
                }
            }
            let activity = bot
                .last_activity
                .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
                .map_or_else(|| "—".into(), |t| t.to_rfc3339());
            let health = if bot.available {
                ""
            } else {
                "unavailable; stale "
            };
            lines.push(format!("{}: {health}{active} active · {attention} attention (inferred) · {done} recent-finished · last activity {activity}", bot.owner.display()));
        }
        lines
    }
}

pub fn display(s: &str) -> String {
    clip(&sanitize(s).replace('\n', "�"), 64)
}

/// Only this exact envelope is a qualified attempt, including malformed ones.
pub fn routing_key(row: &RosterRow) -> &str {
    if row.id.starts_with("hr1:") {
        &row.id
    } else {
        &row.key
    }
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 15) as usize] as char);
    }
    s
}
fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    fn digit(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        }
    }
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| Some(digit(p[0])? * 16 + digit(p[1])?))
        .collect()
}
pub fn source_token(s: &str) -> bool {
    matches!(s, "C" | "H" | "O" | "X" | "G" | "Gm")
}

#[derive(Debug, Clone)]
pub struct Route {
    pub origin: Vec<u8>,
    pub source: String,
    pub store: Vec<u8>,
    pub vendor: Vec<u8>,
}
impl Route {
    pub fn encode(&self) -> String {
        format!(
            "hr1:{}:{}:{}:{}",
            hex(&self.origin),
            self.source,
            hex(&self.store),
            hex(&self.vendor)
        )
    }
    pub fn decode(s: &str) -> Option<Self> {
        if s.len() > 16 * 1024 {
            return None;
        }
        let p: Vec<_> = s.split(':').collect();
        if p.len() != 5 || p[0] != "hr1" || !source_token(p[2]) {
            return None;
        }
        let route = Self {
            origin: unhex(p[1])?,
            source: p[2].into(),
            store: unhex(p[3])?,
            vendor: unhex(p[4])?,
        };
        if route.store.is_empty() || route.vendor.is_empty() {
            return None;
        }
        Some(route)
    }
    pub fn wire(s: &str) -> Option<Self> {
        let (prefix, id) = s.split_once(':')?;
        let route = Self::decode(id)?;
        (prefix == route.source && route.origin.is_empty()).then_some(route)
    }
}

pub fn path_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_encoded_bytes().to_vec()
}
pub fn store_id(origin: &str, store: &[u8]) -> String {
    format!("bs1:{}:{}", hex(origin.as_bytes()), hex(store))
}
pub fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| path.into());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir().unwrap_or_default().join(rest);
    }
    path.into()
}
pub fn default_home() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".hermes")
}
