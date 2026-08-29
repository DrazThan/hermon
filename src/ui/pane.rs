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

use crate::render::{Sem, StyledLine};
use crate::source::{Attn, Liveness};
use crate::ui::palette;

/// A session's pane: what to draw and how far back it is scrolled.
pub struct Pane<'a> {
    pub key: &'a str,
    pub state: Liveness,
    pub selected: bool,
    /// The session's transcript buffer, oldest first.
    pub lines: &'a VecDeque<StyledLine>,
    /// Display lines hidden below the viewport; 0 follows the tail.
    pub offset: usize,
}

/// Draws the pane into `area`, border included.
pub fn render(frame: &mut Frame, area: Rect, pane: &Pane) {
    let block = Block::bordered()
        .border_style(border_style(pane.state, pane.selected))
        .title(pane.key.to_string());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let wrapped = wrap(pane.lines, inner.width as usize);
    let height = inner.height as usize;
    let below = pane.offset.min(wrapped.len().saturating_sub(height));
    let end = wrapped.len() - below;
    let start = end.saturating_sub(height);

    frame.render_widget(Paragraph::new(wrapped[start..end].to_vec()), inner);
    if below > 0 {
        render_more(frame, inner, below);
    }
}

/// Cyan for the selected pane; otherwise the session's own state — amber
/// waiting on you, red stuck, dim finished, plain chrome while it works.
pub fn border_style(state: Liveness, selected: bool) -> Style {
    if selected {
        return palette::border_selected();
    }
    match state {
        Liveness::Live => palette::border(),
        Liveness::Attention(Attn::PermWait) => palette::style(Sem::User),
        Liveness::Attention(Attn::Stuck) => palette::style(Sem::Error),
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

/// Logical lines laid out for a pane `width` columns wide. Breaks at the
/// last space that fits and hard-splits words longer than the pane; an empty
/// logical line stays one empty display line.
pub fn wrap(lines: &VecDeque<StyledLine>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    lines
        .iter()
        .flat_map(|line| wrap_one(line, width))
        .collect()
}

fn wrap_one(line: &StyledLine, width: usize) -> Vec<Line<'static>> {
    let chars: Vec<(Sem, char)> = line
        .0
        .iter()
        .flat_map(|seg| seg.text.chars().map(move |c| (seg.sem, c)))
        .collect();
    if chars.is_empty() {
        return vec![Line::default()];
    }

    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        if chars.len() - start <= width {
            out.push(to_line(&chars[start..]));
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
        out.push(to_line(&chars[start..end]));
        start = next;
    }
    out
}

/// Runs of same-styled characters back into spans.
fn to_line(chars: &[(Sem, char)]) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    let mut current: Option<(Sem, String)> = None;
    for &(sem, c) in chars {
        match &mut current {
            Some((run, text)) if *run == sem => text.push(c),
            _ => {
                if let Some((run, text)) = current.replace((sem, c.to_string())) {
                    spans.push(Span::styled(text, palette::style(run)));
                }
            }
        }
    }
    if let Some((run, text)) = current {
        spans.push(Span::styled(text, palette::style(run)));
    }
    Line::from(spans)
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
            border_style(Liveness::Done, true),
            palette::border_selected()
        );
        assert_eq!(border_style(Liveness::Live, false), palette::border());
        assert_eq!(
            border_style(Liveness::Attention(Attn::PermWait), false),
            palette::style(Sem::User)
        );
        assert_eq!(
            border_style(Liveness::Attention(Attn::Stuck), false),
            palette::style(Sem::Error)
        );
        assert_eq!(
            border_style(Liveness::Done, false),
            palette::style(Sem::Dim)
        );
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
    fn tiny_and_empty_panes_render_without_panicking() {
        let lines = buffer(&["some content that is quite long indeed", "", "more"]);
        for (w, h) in [(60u16, 4u16), (20, 2), (3, 3), (1, 1), (0, 0), (2, 8)] {
            draw(&pane(&lines, 0), w.max(1), h.max(1));
        }
        draw(&pane(&VecDeque::new(), 0), 30, 6);
    }
}
