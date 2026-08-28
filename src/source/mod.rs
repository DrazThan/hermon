//! Session source abstractions: reading live session state from Claude Code,
//! Hermes, and OpenCode's on-disk stores.

pub mod claude;
pub mod hermes;
pub mod opencode;

/// The last event observed in a session's transcript. Only the Claude
/// source can populate this; DB-backed sources (Hermes, OpenCode) leave
/// `SessionMeta::last_event` as `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LastEvent {
    /// A tool call with no result yet; carries the tool name.
    ToolUse(String),
    ToolResult,
    AssistantText,
    User,
}

/// Everything a source knows about one session, in the shared shape the
/// roster and UI consume.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionMeta {
    pub id: String,
    pub started_at: f64,
    pub ended: bool,
    pub model: String,
    pub title: String,
    pub in_tok: u64,
    pub out_tok: u64,
    pub cost: f64,
    pub last_ts: f64,
    pub turn_done: bool,
    pub tool_pending: bool,
    pub last_tool: String,
    pub last_line: String,
    /// Claude fills this; DB sources leave `None`.
    pub last_event: Option<LastEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Live,
    Attention(Attn),
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attn {
    /// A tool call has sat unanswered past the permission-prompt threshold —
    /// the session is probably waiting on a human to approve it.
    PermWait,
    /// The tool-pending ceiling blew: a tool has been "running" so long the
    /// session is presumed wedged, but it is still fresh enough to surface.
    Stuck,
}

/// A source of sessions from one tool's on-disk store (Hermes, OpenCode,
/// Claude Code). Kept minimal — just what the roster needs — since each
/// backing store implements it differently.
pub trait Source {
    /// Current sessions from this source; empty on any read error.
    fn sessions(&mut self) -> Vec<SessionMeta>;
    /// The most recently used tool name in a session, or `"-"` if none.
    fn last_tool(&mut self, session_id: &str) -> String;
}

/// A running tool call gets a much longer leash.
pub const TOOL_PENDING_CEILING_MULT: f64 = 5.0;

/// Silence after a tool call before we assume it is waiting on a permission
/// prompt rather than still running.
const PERM_WAIT_SILENCE: f64 = 30.0;

/// live/done for a turn-based session (Hermes, OpenCode), using the
/// tool's own turn-completion signal rather than guessing from silence.
///
/// An explicit "session closed" flag (Hermes's `ended_at`, OpenCode's
/// `time_archived`) is set rarely in practice — interactive sessions
/// routinely sit for hours between turns without it ever being set — so it
/// can't be the primary signal. The reliable one is the last message's
/// finish/stop reason: a clean stop with no pending tool call means the
/// assistant closed its turn and is idle waiting on the next user message —
/// that's "done" the instant it happens, no timeout needed.
///
/// Otherwise the tool is structurally mid-turn, but two very different
/// waits hide behind that: a pending tool call means a tool (shell command,
/// web fetch, sub-agent) is actually *running* and can legitimately take
/// minutes without a new row appearing, so it gets a much longer ceiling.
/// Anything else mid-turn (a fresh tool result awaiting the assistant's
/// next completion, a user message not yet answered) should resolve within
/// normal API latency, so it keeps the tighter idle_timeout ceiling — a
/// multi-minute gap there is genuinely suspicious, not just a slow tool.
///
/// Verbatim port of `hermon.py` `turn_liveness`; returns `true` for live.
fn turn_liveness_raw(s: &SessionMeta, now: f64, idle_timeout: f64) -> bool {
    if s.ended {
        return false;
    }
    if s.turn_done {
        return false;
    }
    let ceiling = if s.tool_pending {
        idle_timeout * TOOL_PENDING_CEILING_MULT
    } else {
        idle_timeout
    };
    now - s.last_ts <= ceiling
}

/// Classify a session for the UI: the verbatim Python live/done rule,
/// with two attention states layered on top of it rather than edited in.
///
/// - Raw-live with an unanswered tool call silent for more than
///   [`PERM_WAIT_SILENCE`] → [`Attn::PermWait`] (likely a permission
///   prompt); otherwise [`Liveness::Live`].
/// - Raw-done because the tool-pending ceiling blew (`tool_pending` set,
///   not `ended`/`turn_done`) → [`Attn::Stuck`] while `now - last_ts`
///   is within `fresh_window`, [`Liveness::Done`] after — so an abandoned
///   session eventually leaves the deck instead of sitting Stuck forever.
/// - Raw-done for any other reason → [`Liveness::Done`].
pub fn classify(s: &SessionMeta, now: f64, idle_timeout: f64, fresh_window: f64) -> Liveness {
    if turn_liveness_raw(s, now, idle_timeout) {
        let silent = now - s.last_ts > PERM_WAIT_SILENCE;
        if silent && matches!(s.last_event, Some(LastEvent::ToolUse(_))) {
            return Liveness::Attention(Attn::PermWait);
        }
        return Liveness::Live;
    }
    let ceiling_blew = s.tool_pending && !s.ended && !s.turn_done;
    if ceiling_blew && now - s.last_ts <= fresh_window {
        return Liveness::Attention(Attn::Stuck);
    }
    Liveness::Done
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 100_000.0;
    const IDLE: f64 = 180.0;
    const CEILING: f64 = IDLE * TOOL_PENDING_CEILING_MULT; // 900
    const FRESH: f64 = 3_600.0;

    struct M {
        ended: bool,
        turn_done: bool,
        tool_pending: bool,
        age: f64,
        last_event: Option<LastEvent>,
    }

    impl Default for M {
        fn default() -> Self {
            M {
                ended: false,
                turn_done: false,
                tool_pending: false,
                age: 0.0,
                last_event: None,
            }
        }
    }

    fn meta(m: M) -> SessionMeta {
        SessionMeta {
            id: "s1".into(),
            started_at: NOW - 7_200.0,
            ended: m.ended,
            model: "claude-sonnet-5".into(),
            title: "t".into(),
            in_tok: 0,
            out_tok: 0,
            cost: 0.0,
            last_ts: NOW - m.age,
            turn_done: m.turn_done,
            tool_pending: m.tool_pending,
            last_tool: "-".into(),
            last_line: String::new(),
            last_event: m.last_event,
        }
    }

    fn tool_use() -> Option<LastEvent> {
        Some(LastEvent::ToolUse("Bash".into()))
    }

    #[test]
    fn classify_table() {
        use Attn::*;
        use Liveness::*;
        let cases: Vec<(&str, M, Liveness)> = vec![
            // --- mirrors tests/test_hermes.py:250 TestHermesLiveness ---
            (
                "ended wins over everything",
                M { ended: true, age: 0.0, ..M::default() },
                Done,
            ),
            (
                "turn_done is immediate regardless of recency",
                M { turn_done: true, age: 0.0, ..M::default() },
                Done,
            ),
            (
                "mid-turn recent is live",
                M { age: 10.0, ..M::default() },
                Live,
            ),
            (
                "mid-turn beyond ceiling is done",
                M { age: 999.0, ..M::default() },
                Done,
            ),
            (
                "tool_pending gets a longer ceiling (236s, real flicker case)",
                M { tool_pending: true, age: 236.0, ..M::default() },
                Live,
            ),
            (
                "tool_pending past its own ceiling, still fresh -> Stuck",
                M { tool_pending: true, age: 901.0, ..M::default() },
                Attention(Stuck),
            ),
            // --- exact boundaries ---
            (
                "plain ceiling boundary: age == idle_timeout is live",
                M { age: IDLE, ..M::default() },
                Live,
            ),
            (
                "plain ceiling boundary: just past idle_timeout is done",
                M { age: IDLE + 0.001, ..M::default() },
                Done,
            ),
            (
                "tool-pending boundary: age == 5x ceiling is live",
                M { tool_pending: true, age: CEILING, ..M::default() },
                Live,
            ),
            (
                "fresh-window boundary: age == fresh_window is still Stuck",
                M { tool_pending: true, age: FRESH, ..M::default() },
                Attention(Stuck),
            ),
            (
                "past fresh_window the stuck session leaves as Done",
                M { tool_pending: true, age: FRESH + 0.001, ..M::default() },
                Done,
            ),
            // --- pending tool progression: Live -> Stuck -> Done ---
            (
                "pending tool stays live past idle_timeout",
                M { tool_pending: true, age: IDLE + 120.0, ..M::default() },
                Live,
            ),
            (
                "pending tool becomes Stuck past 5x idle_timeout",
                M { tool_pending: true, age: CEILING + 100.0, ..M::default() },
                Attention(Stuck),
            ),
            (
                "pending tool becomes Done past fresh_window",
                M { tool_pending: true, age: FRESH + 100.0, ..M::default() },
                Done,
            ),
            // --- Stuck is only for a blown tool-pending ceiling ---
            (
                "non-pending timeout within fresh_window is Done, not Stuck",
                M { age: IDLE + 1.0, ..M::default() },
                Done,
            ),
            (
                "ended + tool_pending is Done, never Stuck",
                M { ended: true, tool_pending: true, age: 1_000.0, ..M::default() },
                Done,
            ),
            (
                "turn_done + tool_pending, however stale, is Done, never Stuck",
                M { turn_done: true, tool_pending: true, age: 1_000.0, ..M::default() },
                Done,
            ),
            (
                "clean turn_done stays Done at any elapsed time",
                M { turn_done: true, age: 100_000.0, ..M::default() },
                Done,
            ),
            // --- PermWait ---
            (
                "ToolUse with 31s silence -> PermWait",
                M { age: 31.0, last_event: tool_use(), ..M::default() },
                Attention(PermWait),
            ),
            (
                "ToolUse with 29s silence -> Live",
                M { age: 29.0, last_event: tool_use(), ..M::default() },
                Live,
            ),
            (
                "ToolUse with exactly 30s silence -> Live (strictly greater)",
                M { age: 30.0, last_event: tool_use(), ..M::default() },
                Live,
            ),
            (
                "last_event None never yields PermWait",
                M { age: 31.0, ..M::default() },
                Live,
            ),
            (
                "ToolResult silence is Live, not PermWait",
                M { age: 31.0, last_event: Some(LastEvent::ToolResult), ..M::default() },
                Live,
            ),
            (
                "AssistantText silence is Live, not PermWait",
                M { age: 31.0, last_event: Some(LastEvent::AssistantText), ..M::default() },
                Live,
            ),
            (
                "User silence is Live, not PermWait",
                M { age: 31.0, last_event: Some(LastEvent::User), ..M::default() },
                Live,
            ),
            (
                "tool_pending + ToolUse silence while raw-live -> PermWait",
                M { tool_pending: true, age: 236.0, last_event: tool_use(), ..M::default() },
                Attention(PermWait),
            ),
            (
                "ended trumps PermWait even with ToolUse and silence",
                M { ended: true, age: 31.0, last_event: tool_use(), ..M::default() },
                Done,
            ),
            (
                "turn_done trumps PermWait even with ToolUse and silence",
                M { turn_done: true, age: 31.0, last_event: tool_use(), ..M::default() },
                Done,
            ),
            (
                "blown ceiling with ToolUse is Stuck, not PermWait",
                M { tool_pending: true, age: CEILING + 100.0, last_event: tool_use(), ..M::default() },
                Attention(Stuck),
            ),
        ];

        for (name, m, expected) in cases {
            let s = meta(m);
            let got = classify(&s, NOW, IDLE, FRESH);
            assert_eq!(got, expected, "case failed: {name}");
        }
    }

    #[test]
    fn raw_rule_matches_python_semantics() {
        // The private rule keeps pure live/done semantics: the same inputs
        // the Python matrix uses, checked against the bool directly.
        assert!(!turn_liveness_raw(&meta(M { ended: true, ..M::default() }), NOW, IDLE));
        assert!(!turn_liveness_raw(&meta(M { turn_done: true, ..M::default() }), NOW, IDLE));
        assert!(turn_liveness_raw(&meta(M { age: 10.0, ..M::default() }), NOW, IDLE));
        assert!(!turn_liveness_raw(&meta(M { age: 999.0, ..M::default() }), NOW, IDLE));
        assert!(turn_liveness_raw(
            &meta(M { tool_pending: true, age: 236.0, ..M::default() }),
            NOW,
            IDLE
        ));
        assert!(!turn_liveness_raw(
            &meta(M { tool_pending: true, age: 901.0, ..M::default() }),
            NOW,
            IDLE
        ));
    }
}
