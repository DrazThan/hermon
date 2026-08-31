//! Rendering a single session's transcript into displayable lines.
//!
//! Renderers emit semantic styles, never ANSI escapes: the same
//! [`StyledLine`] feeds the ratatui panes (M2 maps [`Sem`] to a `Style`) and
//! `hermon ls`, which prints [`StyledLine::to_plain`]. Line wrapping belongs
//! to the pane widget; renderers emit logical lines and use [`clip`] only for
//! content truncation.
//!
//! **Security: Terminal control sanitization.** The render boundary operates
//! under a defensive contract that extends beyond JSON shape to text content:
//! [`to_plain()`](StyledLine::to_plain) output contains no byte < 0x20 except
//! newline (0x0A). Control sequences (ANSI/OSC, C0/C1, ESC) are stripped at
//! the [`Seg::new`] choke point and replaced with a visible placeholder
//! (`U+FFFD`), making hostile or malformed bytes visible rather than silent.
//! This applies uniformly to all sources (local and future remote agents).

pub mod claude;
pub mod hermes;
pub mod opencode;

use anyhow::{Result, anyhow};
use chrono::DateTime;
use serde::{Deserialize, Serialize};

/// A 24-bit color, ready for `ratatui::style::Color::Rgb`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

/// Tokyo Night Storm palette. Chrome colors first, then one per [`Sem`].
pub const BG: Rgb = rgb(0x1a, 0x1b, 0x26);
pub const CHROME: Rgb = rgb(0x16, 0x16, 0x1e);
pub const BORDER: Rgb = rgb(0x3b, 0x42, 0x61);
pub const SELECTION: Rgb = rgb(0x28, 0x34, 0x57);
pub const FG: Rgb = rgb(0xc0, 0xca, 0xf5);
pub const DIM: Rgb = rgb(0x56, 0x5f, 0x89);
pub const CYAN: Rgb = rgb(0x7d, 0xcf, 0xff);
pub const BLUE: Rgb = rgb(0x7a, 0xa2, 0xf7);
pub const GREEN: Rgb = rgb(0x9e, 0xce, 0x6a);
pub const AMBER: Rgb = rgb(0xe0, 0xaf, 0x68);
pub const RED: Rgb = rgb(0xf7, 0x76, 0x8e);

/// Semantic role of a run of text, replacing `hermon.py`'s raw escape codes
/// (`hermon.py:55 BOLD/DIM/RED/...`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sem {
    /// Body text.
    Plain,
    /// Secondary detail: tool arguments, results, unknown events.
    Dim,
    /// Emphasis at body color: tool-call headers.
    Bold,
    /// Errors and failed tool results.
    Error,
    /// User prompts.
    User,
    /// Token/cost statistics.
    Stat,
    /// Completion markers.
    Ok,
    /// Tool names.
    Tool,
}

impl Sem {
    /// The palette entry this role paints with.
    pub fn color(self) -> Rgb {
        match self {
            Sem::Plain | Sem::Bold => FG,
            Sem::Dim => DIM,
            Sem::Error => RED,
            Sem::User => AMBER,
            Sem::Stat => CYAN,
            Sem::Ok => GREEN,
            Sem::Tool => BLUE,
        }
    }
}

/// Strips terminal control sequences from text, making them visible as placeholders.
/// Replaces C0 controls (0x00-0x1F except 0x0A for newline), C1 controls (0x80-0x9F),
/// with U+FFFD (replacement character). Newlines are preserved; all other control bytes
/// become visible, foiling ANSI/OSC injection attempts.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            let code = c as u32;
            // C0 controls except newline (0x0A): 0x00-0x09, 0x0B-0x1F, and C1: 0x80-0x9F
            if (code <= 0x09 || (0x0B..=0x1F).contains(&code)) || (0x80..=0x9F).contains(&code) {
                '\u{FFFD}' // U+FFFD REPLACEMENT CHARACTER
            } else {
                c
            }
        })
        .collect()
}

/// A run of text carrying one semantic style.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seg {
    pub sem: Sem,
    pub text: String,
}

impl Seg {
    pub fn new(sem: Sem, text: impl Into<String>) -> Self {
        Seg {
            sem,
            text: sanitize(&text.into()),
        }
    }
}

/// One logical output line, as a sequence of styled runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyledLine(pub Vec<Seg>);

impl StyledLine {
    /// The line's text with styling dropped — what `hermon ls` prints and what
    /// tests assert on.
    pub fn to_plain(&self) -> String {
        self.0.iter().map(|seg| seg.text.as_str()).collect()
    }

    /// The line as 24-bit ANSI, for `hermon ls` on a color terminal
    /// (`hermon.py:70 c()`). Callers decide whether color is wanted —
    /// `NO_COLOR` and a non-tty stdout both mean [`to_plain`](Self::to_plain).
    pub fn to_ansi(&self) -> String {
        let mut out = String::new();
        for seg in &self.0 {
            let Rgb { r, g, b } = seg.sem.color();
            if seg.sem == Sem::Bold {
                out.push_str("\x1b[1m");
            }
            out.push_str(&format!("\x1b[38;2;{r};{g};{b}m{}\x1b[0m", seg.text));
        }
        out
    }
}

/// Pane/roster label core: last 6 chars of a uuid stem or hermes id
/// (`hermon.py:77 short_id`).
pub fn short_id(ident: &str) -> String {
    let chars: Vec<char> = ident.chars().collect();
    if chars.is_empty() {
        return "??????".to_string();
    }
    chars[chars.len().saturating_sub(6)..].iter().collect()
}

/// Collapse whitespace and truncate to `n` chars, ellipsis included
/// (`hermon.py:82 clip`). Call sites use 120 for tool arguments and 200 for
/// tool results.
pub fn clip(s: &str, n: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= n {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Elapsed seconds as `12s` / `3m07s` / `2h05m`; `-` for missing or negative
/// (`hermon.py:104 fmt_elapsed`).
pub fn fmt_elapsed(sec: Option<f64>) -> String {
    let Some(sec) = sec else {
        return "-".to_string();
    };
    if sec < 0.0 {
        return "-".to_string();
    }
    let sec = sec as i64;
    if sec < 60 {
        format!("{sec}s")
    } else if sec < 3600 {
        format!("{}m{:02}s", sec / 60, sec % 60)
    } else {
        format!("{}h{:02}m", sec / 3600, (sec % 3600) / 60)
    }
}

/// ISO-8601 timestamp to epoch seconds (`hermon.py:94 parse_ts`).
///
/// Claude transcripts carry `2026-07-08T10:00:00Z`; the hermes and opencode
/// stores hold numeric epochs instead (seconds and milliseconds respectively,
/// see `hermon.py:653`), converted in the source layer without parsing.
pub fn parse_ts(val: &str) -> Result<f64> {
    let dt =
        DateTime::parse_from_rfc3339(val).map_err(|e| anyhow!("bad timestamp {val:?}: {e}"))?;
    Ok(dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_nanos()) / 1e9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_plain_round_trips_without_escape_codes() {
        let line = StyledLine(vec![
            Seg::new(Sem::Bold, "▶ Bash"),
            Seg::new(Sem::Plain, " "),
            Seg::new(Sem::Dim, "ls -la"),
        ]);
        let plain = line.to_plain();
        assert_eq!(plain, "▶ Bash ls -la");
        assert!(!plain.contains('\u{1b}'));
    }

    #[test]
    fn empty_line_is_empty_string() {
        assert_eq!(StyledLine::default().to_plain(), "");
    }

    #[test]
    fn every_sem_has_a_palette_color() {
        let sems = [
            Sem::Plain,
            Sem::Dim,
            Sem::Bold,
            Sem::Error,
            Sem::User,
            Sem::Stat,
            Sem::Ok,
            Sem::Tool,
        ];
        for sem in sems {
            let c = sem.color();
            assert!([FG, DIM, RED, AMBER, CYAN, GREEN, BLUE].contains(&c));
        }
        assert_eq!(Sem::Error.color(), rgb(0xf7, 0x76, 0x8e));
    }

    #[test]
    fn short_id_takes_last_six_chars() {
        assert_eq!(short_id("d4e5f6a7-1234-5678-9abc-def012345678"), "345678");
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id(""), "??????");
    }

    #[test]
    fn clip_collapses_whitespace() {
        assert_eq!(clip("  a\n b\tc  ", 40), "a b c");
    }

    #[test]
    fn clip_passes_short_text_through() {
        assert_eq!(clip("short", 120), "short");
    }

    #[test]
    fn clip_truncates_tool_input_at_120() {
        // Mirrors tests/test_render.py::test_tool_use_shows_name_and_clipped_input.
        let out = clip(&"x".repeat(500), 120);
        assert_eq!(out.chars().count(), 120);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn clip_truncates_tool_result_at_200() {
        // Mirrors tests/test_render.py::test_tool_result_truncated.
        let out = clip(&"y".repeat(500), 200);
        assert_eq!(out.chars().count(), 200);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn clip_counts_chars_not_bytes() {
        let out = clip(&"é".repeat(10), 4);
        assert_eq!(out, "ééé…");
    }

    #[test]
    fn fmt_elapsed_formats_each_band() {
        assert_eq!(fmt_elapsed(None), "-");
        assert_eq!(fmt_elapsed(Some(-1.0)), "-");
        assert_eq!(fmt_elapsed(Some(0.0)), "0s");
        assert_eq!(fmt_elapsed(Some(59.9)), "59s");
        assert_eq!(fmt_elapsed(Some(60.0)), "1m00s");
        assert_eq!(fmt_elapsed(Some(187.0)), "3m07s");
        assert_eq!(fmt_elapsed(Some(3599.0)), "59m59s");
        assert_eq!(fmt_elapsed(Some(3600.0)), "1h00m");
        assert_eq!(fmt_elapsed(Some(7500.0)), "2h05m");
    }

    #[test]
    fn parse_ts_reads_z_suffixed_timestamps() {
        assert_eq!(parse_ts("2026-07-08T10:00:00Z").unwrap(), 1783504800.0);
        assert_eq!(parse_ts("2026-07-08T10:00:00+00:00").unwrap(), 1783504800.0);
        assert_eq!(parse_ts("2026-07-08T12:00:00+02:00").unwrap(), 1783504800.0);
    }

    #[test]
    fn parse_ts_keeps_fractional_seconds() {
        let ts = parse_ts("2026-07-08T10:00:00.250Z").unwrap();
        assert!((ts - 1783504800.25).abs() < 1e-6, "got {ts}");
    }

    #[test]
    fn parse_ts_errors_on_malformed_input() {
        for bad in [
            "",
            "not a timestamp",
            "2026-07-08",
            "12345",
            "2026-13-40T99:00:00Z",
        ] {
            assert!(parse_ts(bad).is_err(), "expected Err for {bad:?}");
        }
    }

    // ---------------------------------------------------- sanitize tests

    #[test]
    fn sanitize_keeps_normal_text() {
        assert_eq!(sanitize("hello world"), "hello world");
        assert_eq!(sanitize("Claude 3.5"), "Claude 3.5");
        assert_eq!(sanitize("café"), "café");
    }

    #[test]
    fn sanitize_keeps_newlines() {
        assert_eq!(sanitize("line1\nline2"), "line1\nline2");
    }

    #[test]
    fn sanitize_removes_null_byte() {
        assert_eq!(sanitize("before\x00after"), "before\u{FFFD}after");
    }

    #[test]
    fn sanitize_removes_tab() {
        assert_eq!(sanitize("col1\tcol2"), "col1\u{FFFD}col2");
    }

    #[test]
    fn sanitize_removes_c0_controls() {
        // BEL (0x07), BS (0x08), VT (0x0B), FF (0x0C), CR (0x0D)
        assert_eq!(sanitize("x\x07y"), "x\u{FFFD}y"); // BEL
        assert_eq!(sanitize("x\x08y"), "x\u{FFFD}y"); // BS
        assert_eq!(sanitize("x\x0by"), "x\u{FFFD}y"); // VT
        assert_eq!(sanitize("x\x0cy"), "x\u{FFFD}y"); // FF
        assert_eq!(sanitize("x\x0dy"), "x\u{FFFD}y"); // CR
    }

    #[test]
    fn sanitize_removes_esc() {
        assert_eq!(sanitize("hello\x1bworld"), "hello\u{FFFD}world");
    }

    #[test]
    fn sanitize_removes_c1_controls() {
        // C1 range: 0x80-0x9F — use char::from_u32 to construct them
        if let Some(c80) = char::from_u32(0x80) {
            let s = format!("x{}y", c80);
            assert_eq!(sanitize(&s), "x\u{FFFD}y");
        }
        if let Some(c9f) = char::from_u32(0x9F) {
            let s = format!("x{}y", c9f);
            assert_eq!(sanitize(&s), "x\u{FFFD}y");
        }
    }

    #[test]
    fn sanitize_osc_52_clipboard_attack() {
        // OSC 52 sequence: ESC ] 52 ; c ; data BEL
        // Only ESC (0x1B) and BEL (0x07) are control bytes that get replaced
        let hostile = "text\x1b]52;c;Y2lhbmV0\x07more";
        assert_eq!(sanitize(hostile), "text\u{FFFD}]52;c;Y2lhbmV0\u{FFFD}more");
    }

    #[test]
    fn sanitize_osc_0_title_attack() {
        // OSC 0 sequence: ESC ] 0 ; title BEL
        // Only ESC (0x1B) and BEL (0x07) are control bytes
        let hostile = "prefix\x1b]0;pwned\x07suffix";
        assert_eq!(sanitize(hostile), "prefix\u{FFFD}]0;pwned\u{FFFD}suffix");
    }

    #[test]
    fn sanitize_csi_sequence() {
        // CSI sequence: ESC [ ... m (e.g., color code)
        // Only ESC (0x1B) is a control byte
        let hostile = "before\x1b[1;31mafter";
        assert_eq!(sanitize(hostile), "before\u{FFFD}[1;31mafter");
    }

    #[test]
    fn sanitize_to_plain_is_printable_only() {
        let line = StyledLine(vec![
            Seg::new(Sem::Bold, "prefix\x1b]0;pwned\x07middle"),
            Seg::new(Sem::Plain, "normal\x00text"),
            Seg::new(Sem::Error, "error\x1bcode"),
        ]);
        let plain = line.to_plain();
        // Check no byte < 0x20 except newline
        for b in plain.as_bytes() {
            assert!(
                *b >= 0x20 || *b == 0x0A,
                "found control byte 0x{:02x} in to_plain output",
                b
            );
        }
    }

    #[test]
    fn seg_constructor_sanitizes() {
        let seg = Seg::new(Sem::Plain, "text\x1bESC");
        assert_eq!(seg.text, "text\u{FFFD}ESC");
    }

    #[test]
    fn hostile_session_title_is_neutralized_in_roster() {
        // Session title with control sequences
        let hostile_title = "Session\x1b]0;hacked\x07Title";
        let seg = Seg::new(Sem::Plain, hostile_title);
        let plain = seg.text;
        for b in plain.as_bytes() {
            assert!(
                *b >= 0x20 || *b == 0x0A,
                "session title not neutralized: 0x{:02x}",
                b
            );
        }
    }
}
