//! Single session pane widget: a bordered, scrollable tile of transcript.
//!
//! Wrapping happens here, at draw time. Renderers emit logical lines with no
//! idea how wide the pane will be, and the same buffer has to lay out again
//! every time the grid re-tiles or the window resizes, so [`wrap`] turns the
//! logical lines into display lines for the pane's current width.
//!
//! The scroll offset counts display lines below the viewport: zero follows
//! the tail, anything else pins the view and shows a dim `▼ N more`. The
//! offset is clamped at draw time — only the pane knows how tall it is — so
//! callers can park [`usize::MAX`] there to mean "as far back as it goes".

use std::collections::VecDeque;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::render::{Seg, Sem, StyledLine, fmt_elapsed};
use crate::source::{Attn, Liveness};
use crate::ui::palette;

/// Transcript lines kept for an open pane. Far more than any viewport shows —
/// the surplus is what grid mode's scrollback (and the desktop pane's) reads.
/// One constant for both front ends: a chatty agent must not outgrow memory
/// in either.
pub const SCROLLBACK: usize = 5_000;

/// A session's pane: what to draw and how far back it is scrolled.
pub struct Pane<'a> {
    pub key: &'a str,
    pub state: Liveness,
    pub selected: bool,
    /// The session's transcript buffer, oldest first.
    pub lines: &'a VecDeque<StyledLine>,
    /// Display lines hidden below the viewport; 0 follows the tail.
    pub offset: usize,
    /// Seconds since the session entered its current attention state, from
    /// [`crate::roster::RosterRow::attn_elapsed`]; `None` outside attention
    /// or before the engine has had a tick to measure it.
    pub attn_elapsed: Option<f64>,
    /// Whether the session is pinned — a finished, pinned pane keeps its
    /// slot (never evicted) and its border says so with the amber accent
    /// instead of going dim.
    pub pinned: bool,
}

/// Draws the pane into `area`, border included.
pub fn render(frame: &mut Frame, area: Rect, pane: &Pane) {
    let block = Block::bordered()
        .border_style(border_style(pane.state, pane.selected, pane.pinned))
        .title(title(pane.key, pane.state));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let mut wrapped = wrap(pane.lines, inner.width as usize);
    if let Some(status) = attention_status(pane.state, pane.attn_elapsed) {
        let mut tail = VecDeque::new();
        tail.push_back(status);
        wrapped.extend(wrap(&tail, inner.width as usize));
    }
    let height = inner.height as usize;
    let below = pane.offset.min(wrapped.len().saturating_sub(height));
    let end = wrapped.len() - below;
    let start = end.saturating_sub(height);

    frame.render_widget(Paragraph::new(wrapped[start..end].to_vec()), inner);
    if below > 0 {
        render_more(frame, inner, below);
    }
}

/// The line appended below an attention pane's transcript, narrating why it
/// needs eyes on it and for how long — `None` for `Live`/`Done`, which get
/// no extra line. Shared with [`crate::gui::pane`], so both front ends say
/// the same thing about a session that needs eyes on it.
pub(crate) fn attention_status(state: Liveness, elapsed: Option<f64>) -> Option<StyledLine> {
    let elapsed = fmt_elapsed(elapsed);
    match state {
        Liveness::Attention(Attn::PermWait) => Some(StyledLine(vec![Seg::new(
            Sem::User,
            format!(
                "{} waiting on permission prompt \u{b7} {elapsed}",
                palette::glyph_for_liveness(state).0,
            ),
        )])),
        Liveness::Attention(Attn::Stuck) => Some(StyledLine(vec![Seg::new(
            Sem::Error,
            format!(
                "{} tool pending {elapsed} \u{2014} no output",
                palette::glyph_for_liveness(state).0,
            ),
        )])),
        Liveness::Live | Liveness::Done => None,
    }
}

/// A finished session's tile is marked done the same way its roster row is:
/// the state glyph in front of the key, so `[x]`-dismissing then `[o]`-
/// reopening — or a resurrection reopening it automatically — reads the
/// same way a fresh pane does, glyph and all.
fn title(key: &str, state: Liveness) -> String {
    match state {
        Liveness::Done => format!("{} {key}", palette::glyph_for_liveness(state).0),
        _ => key.to_string(),
    }
}

/// Cyan for the selected pane; otherwise the session's own state — amber
/// waiting on you, red stuck, dim finished, plain chrome while it works. A
/// pinned pane that finished stays amber instead of dim: pinning is what
/// held its slot against eviction, and the border says so.
pub fn border_style(state: Liveness, selected: bool, pinned: bool) -> Style {
    if selected {
        return palette::border_selected();
    }
    match state {
        Liveness::Live => palette::border(),
        Liveness::Attention(Attn::PermWait) => palette::style(Sem::User),
        Liveness::Attention(Attn::Stuck) => palette::style(Sem::Error),
        Liveness::Done if pinned => palette::style(Sem::User),
        Liveness::Done => palette::style(Sem::Dim),
    }
}

/// How much transcript is still below the viewport, in the bottom-right
/// corner. It paints over the last row rather than stealing one, so the
/// pane shows the same number of lines scrolled or not.
fn render_more(frame: &mut Frame, inner: Rect, below: usize) {
    let label = format!("\u{25bc} {below} more");
    let width = (label.chars().count() as u16).min(inner.width);
    let area = Rect {
        x: inner.right() - width,
        y: inner.bottom() - 1,
        width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Line::styled(label, palette::style(Sem::Dim))),
        area,
    );
}

/// Logical lines laid out for a pane `width` columns wide, as ratatui lines.
pub fn wrap(lines: &VecDeque<StyledLine>, width: usize) -> Vec<Line<'static>> {
    wrap_styled(lines, width).iter().map(to_line).collect()
}

/// Logical lines broken into display lines `width` columns wide. Breaks at
/// the last space that fits and hard-splits words longer than the pane; an
/// empty logical line stays one empty display line.
///
/// Styling survives untouched, so the desktop pane wraps through this too
/// ([`crate::gui::pane`]) and both front ends break the same transcript in
/// the same places.
pub fn wrap_styled(lines: &VecDeque<StyledLine>, width: usize) -> Vec<StyledLine> {
    if width == 0 {
        return Vec::new();
    }
    lines
        .iter()
        .flat_map(|line| wrap_one(line, width))
        .collect()
}

fn wrap_one(line: &StyledLine, width: usize) -> Vec<StyledLine> {
    let chars: Vec<(Sem, char)> = line
        .0
        .iter()
        .flat_map(|seg| seg.text.chars().map(move |c| (seg.sem, c)))
        .collect();
    if chars.is_empty() {
        return vec![StyledLine::default()];
    }

    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        if chars.len() - start <= width {
            out.push(to_styled(&chars[start..]));
            break;
        }
        // One past the last character that fits, so a line ending exactly on
        // a space still breaks there. A space at the very start is no break
        // at all — hard-split instead, which also keeps the loop moving.
        let hard = start + width;
        let (end, next) = match chars[start..=hard].iter().rposition(|(_, c)| *c == ' ') {
            Some(i) if i > 0 => (start + i, start + i + 1),
            _ => (hard, hard),
        };
        out.push(to_styled(&chars[start..end]));
        start = next;
    }
    out
}

/// Runs of same-styled characters back into segments.
fn to_styled(chars: &[(Sem, char)]) -> StyledLine {
    let mut segs: Vec<Seg> = Vec::new();
    for &(sem, c) in chars {
        match segs.last_mut() {
            Some(seg) if seg.sem == sem => seg.text.push(c),
            _ => segs.push(Seg::new(sem, c.to_string())),
        }
    }
    StyledLine(segs)
}

/// A display line as a ratatui line, one span per segment.
fn to_line(line: &StyledLine) -> Line<'static> {
    Line::from(
        line.0
            .iter()
            .map(|seg| Span::styled(seg.text.clone(), palette::style(seg.sem)))
            .collect::<Vec<Span>>(),
    )
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    use super::*;
    use crate::render::Seg;

    fn buffer(texts: &[&str]) -> VecDeque<StyledLine> {
        texts
            .iter()
            .map(|t| StyledLine(vec![Seg::new(Sem::Plain, *t)]))
            .collect()
    }

    fn draw(pane: &Pane, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), pane))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn text(buf: &Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn pane<'a>(lines: &'a VecDeque<StyledLine>, offset: usize) -> Pane<'a> {
        Pane {
            key: "C:aaaaaa",
            state: Liveness::Live,
            selected: false,
            lines,
            offset,
            attn_elapsed: None,
            pinned: false,
        }
    }

    #[test]
    fn the_pane_is_titled_with_its_session_key_and_follows_the_tail() {
        let lines = buffer(&["one", "two", "three", "four", "five"]);
        let rendered = text(&draw(&pane(&lines, 0), 20, 5));

        assert!(rendered.contains("C:aaaaaa"), "{rendered}");
        // Three body rows: the newest three lines, oldest scrolled off.
        assert!(!rendered.contains("one"), "{rendered}");
        assert!(!rendered.contains("two"), "{rendered}");
        for line in ["three", "four", "five"] {
            assert!(rendered.contains(line), "{rendered}");
        }
        assert!(!rendered.contains("more"), "{rendered}");
    }

    /// A finished session's pane carries the same `✓` the roster row does,
    /// and its border goes dim — the lifecycle ticket's acceptance snapshot.
    #[test]
    fn a_done_pane_is_titled_with_a_checkmark_and_a_dim_border() {
        let lines = buffer(&["last line before it finished"]);
        let done = Pane {
            state: Liveness::Done,
            ..pane(&lines, 0)
        };
        let buf = draw(&done, 20, 5);
        let rendered = text(&buf);

        assert!(rendered.contains("✓ C:aaaaaa"), "{rendered}");
        let dim = palette::style(Sem::Dim).fg.unwrap();
        assert_eq!(buf[(0, 0)].fg, dim, "border should be dim: {rendered}");
    }

    #[test]
    fn a_scrolled_pane_stops_following_and_counts_what_is_below() {
        let lines = buffer(&["one", "two", "three", "four", "five"]);
        let rendered = text(&draw(&pane(&lines, 2), 20, 5));

        assert!(rendered.contains("one"), "{rendered}");
        assert!(!rendered.contains("five"), "{rendered}");
        assert!(rendered.contains("\u{25bc} 2 more"), "{rendered}");
    }

    /// Offsets are clamped to what the pane can actually scroll, so
    /// `usize::MAX` parks at the top rather than off the end of the buffer.
    #[test]
    fn an_over_large_offset_clamps_to_the_top_of_the_buffer() {
        let lines = buffer(&["one", "two", "three", "four", "five"]);
        let rendered = text(&draw(&pane(&lines, usize::MAX), 20, 5));

        assert!(rendered.contains("one"), "{rendered}");
        assert!(rendered.contains("three"), "{rendered}");
        assert!(rendered.contains("\u{25bc} 2 more"), "{rendered}");
    }

    #[test]
    fn a_narrow_pane_wraps_long_lines_instead_of_truncating_them() {
        let lines = buffer(&["the quick brown fox jumps over the lazy dog and keeps going"]);
        let rendered = text(&draw(&pane(&lines, 0), 20, 8));

        // 18 columns of body: the sentence lands on several rows, unbroken
        // mid-word, and every word survives.
        for word in ["quick", "brown", "jumps", "lazy", "keeps"] {
            assert!(rendered.contains(word), "{word} lost: {rendered}");
        }
    }

    #[test]
    fn wrapping_breaks_on_spaces_and_hard_splits_long_words() {
        let lines = buffer(&["alpha beta gamma", "supercalifragilistic"]);
        let plain: Vec<String> = wrap(&lines, 10)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert_eq!(plain, ["alpha beta", "gamma", "supercalif", "ragilistic"]);
    }

    #[test]
    fn wrapping_keeps_a_blank_line_as_a_blank_row() {
        let lines = buffer(&["a", "", "b"]);
        assert_eq!(wrap(&lines, 10).len(), 3);
    }

    #[test]
    fn wrapping_preserves_the_style_of_each_segment() {
        let lines: VecDeque<StyledLine> = VecDeque::from(vec![StyledLine(vec![
            Seg::new(Sem::Tool, "Bash"),
            Seg::new(Sem::Dim, " ls -la"),
        ])]);
        let wrapped = wrap(&lines, 40);
        let spans = &wrapped[0].spans;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].style, palette::style(Sem::Tool));
        assert_eq!(spans[1].style, palette::style(Sem::Dim));
    }

    #[test]
    fn the_border_takes_the_session_state_unless_the_pane_is_selected() {
        assert_eq!(
            border_style(Liveness::Done, true, false),
            palette::border_selected()
        );
        assert_eq!(
            border_style(Liveness::Live, false, false),
            palette::border()
        );
        assert_eq!(
            border_style(Liveness::Attention(Attn::PermWait), false, false),
            palette::style(Sem::User)
        );
        assert_eq!(
            border_style(Liveness::Attention(Attn::Stuck), false, false),
            palette::style(Sem::Error)
        );
        assert_eq!(
            border_style(Liveness::Done, false, false),
            palette::style(Sem::Dim)
        );
    }

    /// A finished pane that is pinned keeps the amber accent instead of
    /// going dim — the visual cue that it kept its slot on purpose.
    #[test]
    fn a_pinned_done_pane_stays_amber_instead_of_dim() {
        assert_eq!(
            border_style(Liveness::Done, false, true),
            palette::style(Sem::User)
        );
        let lines = buffer(&["last line"]);
        let done_pinned = Pane {
            state: Liveness::Done,
            pinned: true,
            ..pane(&lines, 0)
        };
        let buf = draw(&done_pinned, 20, 5);
        let amber = palette::style(Sem::User).fg.unwrap();
        assert_eq!(buf[(0, 0)].fg, amber);
    }

    #[test]
    fn the_border_is_painted_in_the_state_color() {
        let lines = buffer(&["x"]);
        let stuck = Pane {
            state: Liveness::Attention(Attn::Stuck),
            ..pane(&lines, 0)
        };
        let buf = draw(&stuck, 20, 4);
        let red = palette::style(Sem::Error).fg.unwrap();
        assert_eq!(buf[(0, 0)].fg, red);
        assert_ne!(buf[(0, 0)].fg, Color::Reset);
    }

    #[test]
    fn a_perm_wait_pane_appends_a_status_line_with_elapsed() {
        let lines = buffer(&["x"]);
        let waiting = Pane {
            state: Liveness::Attention(Attn::PermWait),
            attn_elapsed: Some(45.0),
            ..pane(&lines, 0)
        };
        let rendered = text(&draw(&waiting, 40, 6));
        assert!(
            rendered.contains("waiting on permission prompt \u{b7} 45s"),
            "{rendered}"
        );
    }

    #[test]
    fn a_stuck_pane_appends_a_status_line_with_elapsed() {
        let lines = buffer(&["x"]);
        let stuck = Pane {
            state: Liveness::Attention(Attn::Stuck),
            attn_elapsed: Some(905.0),
            ..pane(&lines, 0)
        };
        let rendered = text(&draw(&stuck, 40, 6));
        assert!(
            rendered.contains("tool pending 15m05s \u{2014} no output"),
            "{rendered}"
        );
    }

    #[test]
    fn a_live_pane_gets_no_status_line() {
        let lines = buffer(&["x"]);
        let rendered = text(&draw(&pane(&lines, 0), 40, 6));
        assert!(!rendered.contains("waiting on"), "{rendered}");
        assert!(!rendered.contains("tool pending"), "{rendered}");
    }

    #[test]
    fn tiny_and_empty_panes_render_without_panicking() {
        let lines = buffer(&["some content that is quite long indeed", "", "more"]);
        for (w, h) in [(60u16, 4u16), (20, 2), (3, 3), (1, 1), (0, 0), (2, 8)] {
            draw(&pane(&lines, 0), w.max(1), h.max(1));
        }
        draw(&pane(&VecDeque::new(), 0), 30, 6);
    }
}
