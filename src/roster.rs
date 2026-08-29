//! The session roster: every source's sessions unioned into one table of
//! display rows, plus the Hermes API-call ticker that sits under it
//! (`hermon.py:1000 API_CALL_RE`, `hermon.py:1025 RosterRow`,
//! `hermon.py:1069 build_roster`, `hermon.py:1039 roster_lines`).
//!
//! Rows carry data, not formatting: [`roster_lines`] turns them into
//! [`StyledLine`]s that `hermon ls` prints and (from M2) the list widget
//! paints.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::LazyLock;

use anyhow::anyhow;
use chrono::{DateTime, Local};
use regex::Regex;

use crate::render::{Seg, Sem, StyledLine, clip, fmt_elapsed, short_id};
use crate::source::claude::ClaudeSource;
use crate::source::hermes::HermesSource;
use crate::source::opencode::OpenCodeSource;
use crate::source::{Attn, Liveness, Replay, SessionMeta, Source, Tailer, classify};

/// One session as the roster displays it (`hermon.py:1025 RosterRow`).
#[derive(Debug, Clone, PartialEq)]
pub struct RosterRow {
    /// The source's own session id, unchanged. The UI keys its selection on
    /// this so the cursor stays on a session as rows reorder between ticks.
    pub id: String,
    /// Pane/roster label: `C:0f865f` (Claude), `H:b356d8` (Hermes),
    /// `O:fiiDPP` (OpenCode).
    pub key: String,
    pub state: Liveness,
    pub model: String,
    pub last_tool: String,
    /// One-line summary of the newest event, as the source rendered it
    /// ([`SessionMeta::last_line`]) — what the session is doing right now.
    pub last_line: String,
    pub in_tok: u64,
    pub out_tok: u64,
    /// Reported spend. Python distinguishes "no cost data" (`-`) from a
    /// genuine `$0.0000`; [`SessionMeta::cost`] has already collapsed the
    /// two to `0.0`, so the roster prints `0.0000` where Python prints `-`.
    pub cost: f64,
    pub elapsed: Option<f64>,
    pub last_ts: f64,
    pub title: String,
}

/// The three on-disk stores hermon reads, held together so the roster can
/// union them in one pass (`hermon.py:1462 build_sources`).
pub struct Sources {
    pub claude: ClaudeSource,
    pub hermes: HermesSource,
    pub opencode: OpenCodeSource,
}

impl Sources {
    pub fn new(claude_dir: &str, hermes_db: &str, opencode_db: &str) -> Self {
        Sources {
            claude: ClaudeSource::new(claude_dir),
            hermes: HermesSource::new(hermes_db),
            opencode: OpenCodeSource::new(opencode_db),
        }
    }

    /// Opens a tailer for one roster row, picking the source from the key's
    /// prefix (`C:`/`H:`/`O:`). Both halves of the row are needed: the key
    /// says which store to ask, and only [`RosterRow::id`] carries the full
    /// session id — the key's is shortened for display.
    ///
    /// `None` when the prefix is unknown or that source has no tailer for
    /// the session; callers show session metadata instead.
    pub fn open_tailer(
        &self,
        key: &str,
        session_id: &str,
        replay: Replay,
    ) -> Option<Box<dyn Tailer>> {
        match key.split_once(':')?.0 {
            "C" => self.claude.open_tailer(session_id, replay),
            "H" => self.hermes.open_tailer(session_id, replay),
            "O" => self.opencode.open_tailer(session_id, replay),
            _ => None,
        }
    }
}

/// Finds the row a `hermon render KEY` argument names. The error lists the
/// keys that *are* on the deck, since they change from run to run and a
/// mistyped one is otherwise a guessing game.
pub fn resolve_key<'a>(rows: &'a [RosterRow], key: &str) -> anyhow::Result<&'a RosterRow> {
    if let Some(row) = rows.iter().find(|r| r.key == key) {
        return Ok(row);
    }
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    Err(if keys.is_empty() {
        anyhow!("no session {key}: no sessions on the roster right now")
    } else {
        anyhow!("no session {key}: valid keys are {}", keys.join(" "))
    })
}

/// Every session from every source, newest activity first
/// (`hermon.py:1069 build_roster`; Python sorts in `roster_lines`, we sort
/// here so every consumer sees one order).
///
/// A session is dropped only when it is both finished and older than
/// `fresh_window` — a live session is never hidden by age.
///
/// One deliberate departure from Python: liveness comes from [`classify`]
/// for *all* sources, so Claude sessions can surface the attention states
/// too (Python has only live/done). The `lsof` write-handle escalation
/// Python uses to keep a quiet-but-open transcript live (`hermon.py:447`)
/// *is* ported, via [`crate::source::claude::ClaudeSource`] setting
/// [`crate::source::SessionMeta::force_live`] before `classify` ever sees
/// the session.
pub fn build_roster(
    sources: &mut Sources,
    now: f64,
    fresh_window: f64,
    idle_timeout: f64,
) -> Vec<RosterRow> {
    let mut rows = Vec::new();

    for s in sources.claude.sessions(now, idle_timeout) {
        let tool = s.last_tool.clone();
        rows.extend(roster_row("C", &s, tool, now, fresh_window, idle_timeout));
    }
    for s in sources.hermes.sessions() {
        let tool = sources.hermes.last_tool(&s.id);
        rows.extend(roster_row("H", &s, tool, now, fresh_window, idle_timeout));
    }
    for s in sources.opencode.sessions(now - fresh_window) {
        let tool = sources.opencode.last_tool(&s.id);
        rows.extend(roster_row("O", &s, tool, now, fresh_window, idle_timeout));
    }

    rows.sort_by(|a, b| b.last_ts.total_cmp(&a.last_ts));
    rows
}

fn roster_row(
    label_prefix: &str,
    s: &SessionMeta,
    last_tool: String,
    now: f64,
    fresh_window: f64,
    idle_timeout: f64,
) -> Option<RosterRow> {
    let state = classify(s, now, idle_timeout, fresh_window);
    if state == Liveness::Done && now - s.last_ts > fresh_window {
        return None;
    }
    Some(RosterRow {
        id: s.id.clone(),
        key: format!("{label_prefix}:{}", short_id(&s.id)),
        state,
        model: s.model.clone(),
        last_tool,
        last_line: s.last_line.clone(),
        in_tok: s.in_tok,
        out_tok: s.out_tok,
        cost: s.cost,
        // Python's `if s["started_at"] else None`: an absent start time
        // means the elapsed column has nothing to say.
        elapsed: (s.started_at > 0.0).then_some(s.last_ts - s.started_at),
        last_ts: s.last_ts,
        title: s.title.clone(),
    })
}

// ------------------------------------------------------------------ ticker

/// One `agent.conversation_loop` API-call log line (`hermon.py:1000`).
static API_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(\d\d:\d\d:\d\d),\d+ \S+ \[\S*?(\w{6})\] agent\.conversation_loop: ",
        r"API call #(\d+): model=(\S+) provider=(\S+) in=(\d+) out=(\d+)",
        r".*?latency=([\d.]+s)",
    ))
    .expect("API_CALL_RE is a valid regex")
});

/// How much of the log tail is scanned, and how many calls are shown
/// (`hermon.py:1007`).
const TICKER_TAIL_BYTES: u64 = 65536;
pub const TICKER_LIMIT: usize = 4;

/// The last few Hermes API calls from `agent.log`, covering the small and
/// auxiliary traffic no session row shows (`hermon.py:1007 api_call_ticker`).
///
/// Only the final [`TICKER_TAIL_BYTES`] are read: the log grows without
/// bound and only its tail is ever displayed. A missing or unreadable log
/// yields no lines, as in Python.
pub fn api_call_ticker(log_path: &Path, limit: usize) -> Vec<StyledLine> {
    let Some(tail) = read_tail(log_path, TICKER_TAIL_BYTES) else {
        return Vec::new();
    };
    let hits: Vec<_> = API_CALL_RE.captures_iter(&tail).collect();
    hits[hits.len().saturating_sub(limit)..]
        .iter()
        .map(|cap| {
            let f = |i: usize| cap.get(i).map_or("", |m| m.as_str());
            let n = |i: usize| commas(f(i).parse().unwrap_or(0));
            StyledLine(vec![Seg::new(
                Sem::Dim,
                format!(
                    "  {} {} #{:>3} {}@{} in={} out={} {}",
                    f(1),
                    f(2),
                    f(3),
                    f(4),
                    f(5),
                    n(6),
                    n(7),
                    f(8)
                ),
            )])
        })
        .collect()
}

/// The last `n` bytes of a file, lossily decoded; `None` on any I/O error.
fn read_tail(path: &Path, n: u64) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let size = f.seek(SeekFrom::End(0)).ok()?;
    f.seek(SeekFrom::Start(size.saturating_sub(n))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

// ----------------------------------------------------------------- display

/// Column widths, verbatim from `hermon.py:1039 roster_lines`.
const W_KEY: usize = 10;
const W_MODEL: usize = 24;
const W_TOOL: usize = 16;
const W_IN: usize = 12;
const W_OUT: usize = 9;
const W_COST: usize = 9;
const W_ELAPSED: usize = 9;
const W_TITLE: usize = 40;

/// The whole roster as printable lines: header, column labels, one row per
/// session, the fleet totals, then the API ticker.
pub fn roster_lines(rows: &[RosterRow], ticker: &[StyledLine], now: f64) -> Vec<StyledLine> {
    let mut lines = vec![
        StyledLine(vec![Seg::new(
            Sem::Bold,
            format!("hermon · {} session(s) · {}", rows.len(), clock(now)),
        )]),
        StyledLine(vec![Seg::new(
            Sem::Dim,
            format!(
                "{:2}{:<W_KEY$}{:<W_MODEL$}{:<W_TOOL$}{:>W_IN$}{:>W_OUT$}{:>W_COST$}{:>W_ELAPSED$}  title",
                "", "id", "model", "last tool", "in", "out", "cost", "elapsed",
            ),
        )]),
    ];

    lines.extend(rows.iter().map(row_line));
    if rows.is_empty() {
        lines.push(StyledLine(vec![Seg::new(
            Sem::Dim,
            "  (no sessions in window — waiting)",
        )]));
    }
    lines.push(totals_line(rows));

    if !ticker.is_empty() {
        lines.push(StyledLine::default());
        lines.push(StyledLine(vec![Seg::new(
            Sem::Dim,
            "  recent hermes API calls:",
        )]));
        lines.extend(ticker.iter().cloned());
    }
    lines
}

fn row_line(r: &RosterRow) -> StyledLine {
    StyledLine(vec![
        glyph(r.state),
        Seg::new(
            Sem::Plain,
            format!(
                " {:<W_KEY$}{:<W_MODEL$}{:<W_TOOL$}{:>W_IN$}{:>W_OUT$}{:>W_COST$}{:>W_ELAPSED$}  ",
                r.key,
                clip(&r.model, W_MODEL - 1),
                clip(&r.last_tool, W_TOOL - 1),
                commas(r.in_tok),
                commas(r.out_tok),
                format!("{:.4}", r.cost),
                fmt_elapsed(r.elapsed),
            ),
        ),
        Seg::new(Sem::Dim, clip(&r.title, W_TITLE)),
    ])
}

/// The fleet at a glance: `N live · N done · Σ $X.XX · Y in`. Sessions
/// needing attention are counted as live — they are unfinished work.
pub(crate) fn totals_line(rows: &[RosterRow]) -> StyledLine {
    let done = rows.iter().filter(|r| r.state == Liveness::Done).count();
    // fold, not sum(): f64's Sum identity is -0.0, which prints as "$-0.00".
    let cost = rows.iter().fold(0.0, |acc, r| acc + r.cost);
    let in_tok: u64 = rows.iter().map(|r| r.in_tok).sum();
    StyledLine(vec![Seg::new(
        Sem::Stat,
        format!(
            "{} live · {done} done · Σ ${cost:.2} · {} in",
            rows.len() - done,
            commas(in_tok),
        ),
    )])
}

/// Status glyph and its color, per the design's four session states.
fn glyph(state: Liveness) -> Seg {
    match state {
        Liveness::Live => Seg::new(Sem::Ok, "●"),
        Liveness::Attention(Attn::PermWait) => Seg::new(Sem::User, "⏸"),
        Liveness::Attention(Attn::Stuck) => Seg::new(Sem::Error, "⚠"),
        Liveness::Done => Seg::new(Sem::Dim, "✓"),
    }
}

/// Local wall-clock `HH:MM:SS` for the header (Python's `datetime.now()`).
fn clock(now: f64) -> String {
    DateTime::from_timestamp(now as i64, 0)
        .map(|t| t.with_timezone(&Local).format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Thousands separators, matching Python's `{:,}` token counts.
pub(crate) fn commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.char_indices() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::TempDir;

    use super::*;

    const NOW: f64 = 1_800_000_000.0;

    fn row(key: &str, state: Liveness) -> RosterRow {
        RosterRow {
            id: format!("id-{key}"),
            key: key.to_string(),
            state,
            model: "claude-sonnet-5".to_string(),
            last_tool: "Bash".to_string(),
            last_line: "▶ Bash ls -la".to_string(),
            in_tok: 1_234_567,
            out_tok: 890,
            cost: 1.5,
            elapsed: Some(187.0),
            last_ts: NOW,
            title: "a title".to_string(),
        }
    }

    #[test]
    fn resolve_key_finds_the_row_it_names() {
        let rows = vec![
            row("C:aaaaaa", Liveness::Live),
            row("H:bbbbbb", Liveness::Done),
        ];
        let found = resolve_key(&rows, "H:bbbbbb").expect("key is on the deck");
        assert_eq!(found.id, "id-H:bbbbbb");
    }

    /// A mistyped key is the common case, and the valid ones change every
    /// run, so the error has to name them.
    #[test]
    fn resolve_key_lists_the_valid_keys_when_it_fails() {
        let rows = vec![
            row("C:aaaaaa", Liveness::Live),
            row("H:bbbbbb", Liveness::Done),
        ];
        let err = resolve_key(&rows, "bogus")
            .expect_err("bogus key")
            .to_string();
        assert!(err.contains("no session bogus"), "{err}");
        assert!(err.contains("C:aaaaaa"), "{err}");
        assert!(err.contains("H:bbbbbb"), "{err}");
    }

    #[test]
    fn resolve_key_says_so_when_the_roster_is_empty() {
        let err = resolve_key(&[], "C:aaaaaa")
            .expect_err("empty roster")
            .to_string();
        assert!(err.contains("no sessions on the roster"), "{err}");
    }

    /// The prefix picks the store; an unknown one is not a panic. Hermes
    /// and OpenCode tailers open lazily against a possibly-missing db
    /// (self-healing per `HermesTailer::poll` / the opencode wait notice),
    /// so `H:` and `O:` are the prefixes that always yield a tailer even
    /// when the backing store doesn't exist yet.
    #[test]
    fn open_tailer_dispatches_on_the_key_prefix() {
        let sources = Sources::new(
            "/nonexistent/claude",
            "/nonexistent/h.db",
            "/nonexistent/o.db",
        );
        for key in ["C:aaaaaa", "Z:dddddd", "nocolon"] {
            assert!(sources.open_tailer(key, "id", Replay::DEFAULT).is_none());
        }
        for key in ["H:bbbbbb", "O:cccccc"] {
            assert!(sources.open_tailer(key, "id", Replay::DEFAULT).is_some());
        }
    }

    #[test]
    fn commas_groups_by_three() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_000), "1,000");
        assert_eq!(commas(1_234_567), "1,234,567");
    }

    #[test]
    fn glyphs_cover_every_state() {
        assert_eq!(glyph(Liveness::Live), Seg::new(Sem::Ok, "●"));
        assert_eq!(glyph(Liveness::Done), Seg::new(Sem::Dim, "✓"));
        assert_eq!(
            glyph(Liveness::Attention(Attn::PermWait)),
            Seg::new(Sem::User, "⏸")
        );
        assert_eq!(
            glyph(Liveness::Attention(Attn::Stuck)),
            Seg::new(Sem::Error, "⚠")
        );
    }

    #[test]
    fn row_line_lays_out_the_columns() {
        let line = row_line(&row("H:abc123", Liveness::Live)).to_plain();
        let cols: Vec<char> = line.chars().collect();
        // Every column starts where roster_lines' header says it does.
        assert_eq!(cols[0], '●');
        assert_eq!(text(&cols, 2, W_KEY), "H:abc123");
        assert_eq!(text(&cols, 12, W_MODEL), "claude-sonnet-5");
        assert_eq!(text(&cols, 36, W_TOOL), "Bash");
        assert_eq!(text(&cols, 52, W_IN), "1,234,567");
        assert_eq!(text(&cols, 64, W_OUT), "890");
        assert_eq!(text(&cols, 73, W_COST), "1.5000");
        assert_eq!(text(&cols, 82, W_ELAPSED), "3m07s");
        assert!(line.ends_with("  a title"), "{line}");
    }

    /// The `n`-wide column at `start`, trimmed of its padding.
    fn text(cols: &[char], start: usize, n: usize) -> String {
        cols[start..start + n]
            .iter()
            .collect::<String>()
            .trim()
            .to_string()
    }

    #[test]
    fn long_fields_are_clipped_to_their_columns() {
        let mut r = row("O:xyz789", Liveness::Done);
        r.model = "a-very-long-model-identifier-indeed".to_string();
        r.last_tool = "a_tool_with_a_very_long_name".to_string();
        r.title = "t".repeat(80);
        let line = row_line(&r).to_plain();
        let cols: Vec<char> = line.chars().collect();
        assert_eq!(text(&cols, 12, W_MODEL), "a-very-long-model-iden…");
        assert_eq!(text(&cols, 36, W_TOOL), "a_tool_with_a_…");
        assert_eq!(
            line.chars().skip(93).collect::<String>(),
            format!("{}…", "t".repeat(39))
        );
    }

    #[test]
    fn totals_count_attention_rows_as_live() {
        let rows = [
            row("C:aaaaaa", Liveness::Live),
            row("H:bbbbbb", Liveness::Attention(Attn::PermWait)),
            row("O:cccccc", Liveness::Attention(Attn::Stuck)),
            row("C:dddddd", Liveness::Done),
        ];
        assert_eq!(
            totals_line(&rows).to_plain(),
            "3 live · 1 done · Σ $6.00 · 4,938,268 in"
        );
    }

    #[test]
    fn empty_roster_still_renders_a_header_and_totals() {
        let lines: Vec<String> = roster_lines(&[], &[], NOW)
            .iter()
            .map(StyledLine::to_plain)
            .collect();
        assert!(lines[0].starts_with("hermon · 0 session(s) · "));
        assert!(lines.iter().any(|l| l.contains("(no sessions in window")));
        assert_eq!(lines.last().unwrap(), "0 live · 0 done · Σ $0.00 · 0 in");
    }

    #[test]
    fn ticker_lines_are_appended_under_a_heading() {
        let ticker = vec![StyledLine(vec![Seg::new(Sem::Dim, "  tick")])];
        let lines: Vec<String> = roster_lines(&[row("C:aaaaaa", Liveness::Live)], &ticker, NOW)
            .iter()
            .map(StyledLine::to_plain)
            .collect();
        assert_eq!(lines[lines.len() - 3], "");
        assert_eq!(lines[lines.len() - 2], "  recent hermes API calls:");
        assert_eq!(lines[lines.len() - 1], "  tick");
    }

    // ------------------------------------------------------------- ticker

    fn log_line(hh: &str, sid: &str, n: u32, in_tok: u32, out_tok: u32) -> String {
        format!(
            "{hh},123 INFO [hermes.{sid}] agent.conversation_loop: API call #{n}: \
             model=claude-sonnet-5 provider=anthropic in={in_tok} out={out_tok} \
             cached=0 latency=1.25s\n"
        )
    }

    fn temp_log(body: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("agent.log");
        File::create(&path)
            .expect("create log")
            .write_all(body.as_bytes())
            .expect("write log");
        (dir, path)
    }

    #[test]
    fn ticker_extracts_the_last_calls() {
        let body: String = (1..=6)
            .map(|i| {
                log_line(
                    &format!("14:23:{i:02}"),
                    &format!("sess{i:02}"),
                    i,
                    1000 * i,
                    i,
                )
            })
            .collect();
        let (_dir, path) = temp_log(&body);

        let lines: Vec<String> = api_call_ticker(&path, TICKER_LIMIT)
            .iter()
            .map(StyledLine::to_plain)
            .collect();
        assert_eq!(lines.len(), 4, "limit of 4 newest calls");
        assert_eq!(
            lines[0],
            "  14:23:03 sess03 #  3 claude-sonnet-5@anthropic in=3,000 out=3 1.25s"
        );
        assert!(lines[3].contains("sess06"));
    }

    #[test]
    fn ticker_reads_only_the_tail_of_a_huge_log() {
        let filler = "x".repeat(TICKER_TAIL_BYTES as usize * 2);
        let body = format!(
            "{}{filler}\n{}",
            log_line("01:00:00", "oldold", 1, 10, 10),
            log_line("02:00:00", "newnew", 2, 20, 20),
        );
        let (_dir, path) = temp_log(&body);

        let lines: Vec<String> = api_call_ticker(&path, TICKER_LIMIT)
            .iter()
            .map(StyledLine::to_plain)
            .collect();
        assert_eq!(lines.len(), 1, "the old call is outside the 64 KB tail");
        assert!(lines[0].contains("newnew"), "{:?}", lines[0]);
    }

    #[test]
    fn ticker_ignores_unrelated_lines_and_missing_logs() {
        let (_dir, path) = temp_log("11:11:11,000 INFO [x] something.else: hello\n");
        assert!(api_call_ticker(&path, TICKER_LIMIT).is_empty());
        assert!(api_call_ticker(Path::new("/nonexistent/agent.log"), TICKER_LIMIT).is_empty());
    }
}
