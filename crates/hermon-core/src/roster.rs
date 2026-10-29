//! Shared session display data, independent of the application and remote transports.

use crate::source::Liveness;

/// One session as the roster displays it (`hermon.py:1025 RosterRow`).
#[derive(Debug, Clone, PartialEq)]
pub struct RosterRow {
    /// Qualified aggregate identity; direct source metadata keeps the raw vendor ID.
    pub id: String,
    /// Pane/roster label: `C:0f865f` (Claude), `H:b356d8` (Hermes),
    /// `O:fiiDPP` (OpenCode), `X:123456` (Codex), `G:123456` (Grok),
    /// `Gm:123456` (Gemini).
    pub key: String,
    pub state: Liveness,
    pub model: String,
    pub last_tool: String,
    /// One-line summary of the newest event, as the source rendered it
    /// ([`crate::source::SessionMeta::last_line`]) — what the session is doing right now.
    pub last_line: String,
    pub in_tok: u64,
    pub out_tok: u64,
    /// Reported spend. `None` means no cost data available; `Some(v)` is the
    /// actual spend, which may be zero.
    pub cost: Option<f64>,
    pub elapsed: Option<f64>,
    pub last_ts: f64,
    pub title: String,
    /// Seconds since the session entered its current [`Liveness::Attention`]
    /// state; `None` outside attention, and also outside the live engine
    /// loop, which is the only caller that can track history across ticks
    /// (`hermon ls`'s one-shot `build_roster` call always leaves this
    /// unset). Populated by `engine::scan` after lifecycle events are
    /// resolved, from a `key -> since` map the roster itself has no memory
    /// of.
    pub attn_elapsed: Option<f64>,
}
