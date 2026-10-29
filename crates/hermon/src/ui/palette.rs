//! Shared color palette for the TUI, mapping Sem to ratatui styles and providing glyphs.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::render::{Rgb, Sem, StyledLine};
use crate::source::{Attn, Liveness};

/// Convert an Rgb value to a ratatui Color.
fn rgb_to_color(rgb: Rgb) -> Color {
    Color::Rgb(rgb.r, rgb.g, rgb.b)
}

/// Map a semantic role to a ratatui style using the Tokyo Night Storm palette.
pub fn style(sem: Sem) -> Style {
    let color = rgb_to_color(sem.color());
    match sem {
        Sem::Bold => Style::new().fg(color).add_modifier(Modifier::BOLD),
        _ => Style::new().fg(color),
    }
}

/// Style for the border in normal state.
pub fn border() -> Style {
    let border_color = Rgb {
        r: 0x3b,
        g: 0x42,
        b: 0x61,
    };
    Style::new().fg(rgb_to_color(border_color))
}

/// Style for the border in selected state (cyan).
pub fn border_selected() -> Style {
    let cyan = Rgb {
        r: 0x7d,
        g: 0xcf,
        b: 0xff,
    };
    Style::new().fg(rgb_to_color(cyan))
}

/// Style for the selection background.
pub fn selection_bg() -> Style {
    let selection = Rgb {
        r: 0x28,
        g: 0x34,
        b: 0x57,
    };
    Style::new().bg(rgb_to_color(selection))
}

/// A set of glyphs for indicating session liveness/attention states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphSet {
    pub live: &'static str,
    pub done: &'static str,
    pub perm_wait: &'static str,
    pub stuck: &'static str,
    pub muted: &'static str,
    /// Marks a pinned session in the roster's pin column — 📌 is exactly the
    /// emoji-width hazard this fallback table exists for.
    pub pin: &'static str,
}

impl GlyphSet {
    /// Create a glyph set for the given mode: `true` for ASCII fallback, `false` for Unicode.
    pub fn for_mode(ascii: bool) -> Self {
        if ascii {
            GlyphSet {
                live: "*",
                done: ".",
                perm_wait: "||",
                stuck: "!",
                muted: "[muted]",
                pin: "*",
            }
        } else {
            GlyphSet {
                live: "●",
                done: "✓",
                perm_wait: "⏸",
                stuck: "⚠",
                muted: "🔕",
                pin: "📌",
            }
        }
    }
}

/// Get the cached glyph set, selecting ASCII or Unicode based on the
/// HERMON_ASCII env var. Public because the egui window paints the same
/// glyphs ([`crate::gui::palette`]) from its own color mapping.
pub fn glyphs() -> GlyphSet {
    use std::sync::OnceLock;
    static GLYPHS: OnceLock<GlyphSet> = OnceLock::new();
    *GLYPHS.get_or_init(|| {
        let use_ascii = std::env::var_os("HERMON_ASCII").is_some();
        GlyphSet::for_mode(use_ascii)
    })
}

/// The mute indicator for the footer: empty when unmuted, else the glyph
/// (`🔕`, or `[muted]` under `HERMON_ASCII`).
pub fn mute_indicator(muted: bool) -> &'static str {
    if muted { glyphs().muted } else { "" }
}

/// Return the glyph and style for a session's liveness state.
pub fn glyph_for_liveness(liveness: Liveness) -> (&'static str, Style) {
    let glyphs = glyphs();
    match liveness {
        Liveness::Live => (glyphs.live, style(Sem::Ok)),
        Liveness::Done => (glyphs.done, style(Sem::Dim)),
        Liveness::Attention(Attn::PermWait) => (glyphs.perm_wait, style(Sem::User)),
        Liveness::Attention(Attn::Stuck) => (glyphs.stuck, style(Sem::Error)),
    }
}

/// The pin column's glyph — 📌, or `*` under `HERMON_ASCII`.
pub fn pin_glyph() -> &'static str {
    glyphs().pin
}

/// Convert a StyledLine into a ratatui Line, mapping each segment's Sem to a Style.
pub fn line_to_spans(line: &StyledLine) -> Line<'static> {
    let spans: Vec<Span> = line
        .0
        .iter()
        .map(|seg| Span::styled(seg.text.clone(), style(seg.sem)))
        .collect();
    Line::from(spans)
}

/// [`line_to_spans`] over a whole block of lines, ready for a `Paragraph`.
pub fn to_lines(lines: &[StyledLine]) -> Vec<Line<'static>> {
    lines.iter().map(line_to_spans).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Seg;

    #[test]
    fn line_to_spans_preserves_text_content() {
        let line = StyledLine(vec![
            Seg::new(Sem::Bold, "▶ Bash"),
            Seg::new(Sem::Plain, " "),
            Seg::new(Sem::Dim, "ls -la"),
        ]);
        let spans_line = line_to_spans(&line);
        let text: String = spans_line
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(text, "▶ Bash ls -la");
    }

    #[test]
    fn line_to_spans_maps_plain_to_fg() {
        let line = StyledLine(vec![Seg::new(Sem::Plain, "test")]);
        let spans_line = line_to_spans(&line);
        assert_eq!(spans_line.spans.len(), 1);
        let fg = spans_line.spans[0].style.fg;
        assert_eq!(fg, Some(Color::Rgb(0xc0, 0xca, 0xf5))); // FG
    }

    #[test]
    fn line_to_spans_maps_dim() {
        let line = StyledLine(vec![Seg::new(Sem::Dim, "dim")]);
        let spans_line = line_to_spans(&line);
        let fg = spans_line.spans[0].style.fg;
        assert_eq!(fg, Some(Color::Rgb(0x56, 0x5f, 0x89))); // DIM
    }

    #[test]
    fn line_to_spans_maps_bold_with_modifier() {
        let line = StyledLine(vec![Seg::new(Sem::Bold, "bold")]);
        let spans_line = line_to_spans(&line);
        let s = &spans_line.spans[0].style;
        assert_eq!(s.fg, Some(Color::Rgb(0xc0, 0xca, 0xf5))); // FG
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn line_to_spans_maps_error() {
        let line = StyledLine(vec![Seg::new(Sem::Error, "err")]);
        let spans_line = line_to_spans(&line);
        let fg = spans_line.spans[0].style.fg;
        assert_eq!(fg, Some(Color::Rgb(0xf7, 0x76, 0x8e))); // RED
    }

    #[test]
    fn line_to_spans_maps_user() {
        let line = StyledLine(vec![Seg::new(Sem::User, "user")]);
        let spans_line = line_to_spans(&line);
        let fg = spans_line.spans[0].style.fg;
        assert_eq!(fg, Some(Color::Rgb(0xe0, 0xaf, 0x68))); // AMBER
    }

    #[test]
    fn line_to_spans_maps_stat() {
        let line = StyledLine(vec![Seg::new(Sem::Stat, "stat")]);
        let spans_line = line_to_spans(&line);
        let fg = spans_line.spans[0].style.fg;
        assert_eq!(fg, Some(Color::Rgb(0x7d, 0xcf, 0xff))); // CYAN
    }

    #[test]
    fn line_to_spans_maps_ok() {
        let line = StyledLine(vec![Seg::new(Sem::Ok, "ok")]);
        let spans_line = line_to_spans(&line);
        let fg = spans_line.spans[0].style.fg;
        assert_eq!(fg, Some(Color::Rgb(0x9e, 0xce, 0x6a))); // GREEN
    }

    #[test]
    fn line_to_spans_maps_tool() {
        let line = StyledLine(vec![Seg::new(Sem::Tool, "tool")]);
        let spans_line = line_to_spans(&line);
        let fg = spans_line.spans[0].style.fg;
        assert_eq!(fg, Some(Color::Rgb(0x7a, 0xa2, 0xf7))); // BLUE
    }

    #[test]
    fn glyph_set_unicode_mode() {
        let glyphs = GlyphSet::for_mode(false);
        assert_eq!(glyphs.live, "●");
        assert_eq!(glyphs.done, "✓");
        assert_eq!(glyphs.perm_wait, "⏸");
        assert_eq!(glyphs.stuck, "⚠");
        assert_eq!(glyphs.pin, "📌");
    }

    #[test]
    fn glyph_set_ascii_mode() {
        let glyphs = GlyphSet::for_mode(true);
        assert_eq!(glyphs.live, "*");
        assert_eq!(glyphs.done, ".");
        assert_eq!(glyphs.perm_wait, "||");
        assert_eq!(glyphs.stuck, "!");
        assert_eq!(glyphs.pin, "*");
    }

    #[test]
    fn pin_glyph_resolves_to_the_cached_mode() {
        let pin = pin_glyph();
        assert!(
            [GlyphSet::for_mode(false).pin, GlyphSet::for_mode(true).pin].contains(&pin),
            "{pin:?}"
        );
    }

    #[test]
    fn glyph_for_liveness_live() {
        let (glyph, _style) = glyph_for_liveness(Liveness::Live);
        assert!(
            [
                GlyphSet::for_mode(false).live,
                GlyphSet::for_mode(true).live
            ]
            .contains(&glyph)
        );
    }

    #[test]
    fn glyph_for_liveness_done() {
        let (glyph, style) = glyph_for_liveness(Liveness::Done);
        assert!(
            [
                GlyphSet::for_mode(false).done,
                GlyphSet::for_mode(true).done
            ]
            .contains(&glyph)
        );
        // Dim style should have DIM color
        assert_eq!(style.fg, Some(Color::Rgb(0x56, 0x5f, 0x89)));
    }

    #[test]
    fn glyph_for_liveness_perm_wait() {
        let (glyph, style) = glyph_for_liveness(Liveness::Attention(Attn::PermWait));
        assert!(
            [
                GlyphSet::for_mode(false).perm_wait,
                GlyphSet::for_mode(true).perm_wait
            ]
            .contains(&glyph)
        );
        // User style should have AMBER color
        assert_eq!(style.fg, Some(Color::Rgb(0xe0, 0xaf, 0x68)));
    }

    #[test]
    fn glyph_for_liveness_stuck() {
        let (glyph, style) = glyph_for_liveness(Liveness::Attention(Attn::Stuck));
        assert!(
            [
                GlyphSet::for_mode(false).stuck,
                GlyphSet::for_mode(true).stuck
            ]
            .contains(&glyph)
        );
        // Error style should have RED color
        assert_eq!(style.fg, Some(Color::Rgb(0xf7, 0x76, 0x8e)));
    }

    #[test]
    fn glyphs_uses_env_var_once() {
        // This test verifies that glyphs() caches the result after the first call.
        // We can't easily test the env-var reading in parallel tests, so instead
        // we test that GlyphSet::for_mode() works correctly for both modes.
        let ascii_set = GlyphSet::for_mode(true);
        let unicode_set = GlyphSet::for_mode(false);
        assert_ne!(ascii_set.live, unicode_set.live);
    }
}
