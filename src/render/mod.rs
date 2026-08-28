//! Rendering a single session's transcript into displayable lines.
//!
//! Renderers emit semantic styles, never ANSI escapes: the same
//! [`StyledLine`] feeds the ratatui panes (M2 maps [`Sem`] to a `Style`) and
//! `hermon ls`, which prints [`StyledLine::to_plain`]. Line wrapping belongs
//! to the pane widget; renderers emit logical lines and use [`clip`] only for
//! content truncation.

pub mod claude;
pub mod hermes;
pub mod opencode;

use anyhow::{Result, anyhow};
use chrono::DateTime;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// A run of text carrying one semantic style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seg {
    pub sem: Sem,
    pub text: String,
}

impl Seg {
    pub fn new(sem: Sem, text: impl Into<String>) -> Self {
        Seg {
            sem,
            text: text.into(),
        }
    }
}

/// One logical output line, as a sequence of styled runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyledLine(pub Vec<Seg>);

impl StyledLine {
    /// The line's text with styling dropped — what `hermon ls` prints and what
    /// tests assert on.
    pub fn to_plain(&self) -> String {
        self.0.iter().map(|seg| seg.text.as_str()).collect()
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
}
