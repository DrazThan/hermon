//! Sort/filter palette overlay (artboard 3b): `[s]`/`[f]` open it centered
//! over whatever mode is on screen.
//!
//! The overlay edits [`ViewState`] live so the dimmed screen behind it and
//! the header chips ([`super::header`]) stay in sync as you pick a sort or
//! type a filter. [`PaletteFocus`] decides what typed characters mean:
//! `Sort` routes digits `1`-`5` to the sort chips, `Filter` routes every
//! printable character into the filter draft — the two never overlap, so
//! typing `cost>1` while filtering never flips a sort chip.
//! [`Palette::open`] snapshots the view; `[Esc]` restores it, `[Enter]`
//! commits the typed filter and closes, `[c]` clears both (Sort focus only,
//! so it can never eat a `c` typed into the filter text).

use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::render::Sem;
use crate::ui::palette;
use crate::view::{Filter, SortKey, ViewState};

/// Which section of the overlay typed characters go to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteFocus {
    Sort,
    Filter,
}

/// The overlay's own state, layered on top of the [`ViewState`] it edits
/// live. `view` itself is mutated as the user picks chips or applies a
/// filter; `prior` is the snapshot `[Esc]` restores.
#[derive(Debug, Clone)]
pub struct Palette {
    pub focus: PaletteFocus,
    /// The filter text as typed; parsed into `ViewState::filter` on
    /// `[Enter]`. Prefilled from the already-active filter on open, so
    /// reopening the palette continues editing rather than starting blank.
    pub input: String,
    /// The last parse error, shown in place of the match count until the
    /// input changes again.
    pub error: Option<String>,
    prior: ViewState,
}

impl Palette {
    pub fn open(view: &ViewState, focus: PaletteFocus) -> Self {
        Palette {
            focus,
            input: view.filter.as_input(),
            error: None,
            prior: view.clone(),
        }
    }

    /// Handles one key, mutating `view` in place. Returns `true` once the
    /// overlay should close (`[Esc]` or a successful `[Enter]`).
    pub fn handle_key(&mut self, code: KeyCode, view: &mut ViewState) -> bool {
        match code {
            KeyCode::Esc => {
                *view = self.prior.clone();
                true
            }
            KeyCode::Enter => match view.set_filter(self.input.trim()) {
                Ok(()) => true,
                Err(message) => {
                    self.error = Some(message);
                    false
                }
            },
            KeyCode::Char(c) if self.focus == PaletteFocus::Sort && c.is_ascii_digit() => {
                if let Some(index) = c.to_digit(10).map(|d| d as usize).filter(|n| *n >= 1)
                    && let Some(key) = SortKey::ALL.get(index - 1)
                {
                    view.toggle_sort(*key);
                }
                false
            }
            KeyCode::Char('c') if self.focus == PaletteFocus::Sort => {
                view.clear();
                self.input.clear();
                self.error = None;
                false
            }
            KeyCode::Char(c) if self.focus == PaletteFocus::Filter => {
                self.input.push(c);
                self.error = None;
                false
            }
            KeyCode::Backspace if self.focus == PaletteFocus::Filter => {
                self.input.pop();
                self.error = None;
                false
            }
            _ => false,
        }
    }
}

/// A centered box the height a sort row, a filter row, a status row and a
/// help row need, plus the border.
const WIDTH: u16 = 62;
const HEIGHT: u16 = 7;

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Draws the overlay over whatever is already on screen: the body first
/// gets dimmed (the artboard's 35% opacity, approximated with a flat dim
/// style since ratatui has no alpha blending), then `Clear` blanks the box
/// so it can't show through the gaps between spans.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    view: &ViewState,
    palette: &Palette,
    matched: usize,
    total: usize,
) {
    let rect = centered(area, WIDTH, HEIGHT);
    dim_screen(frame, area, rect);
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .title("sort / filter")
        .border_style(palette::border_selected());
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let [sort_area, filter_area, status_area, help_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    frame.render_widget(Paragraph::new(sort_line(view)), sort_area);
    frame.render_widget(Paragraph::new(filter_line(palette)), filter_area);
    frame.render_widget(
        Paragraph::new(status_line(palette, matched, total)),
        status_area,
    );
    frame.render_widget(Paragraph::new(help_line()), help_area);
}

/// Flattens every cell outside `except` to the dim foreground, standing in
/// for the artboard's 35%-opacity scrim over the screen behind the overlay.
fn dim_screen(frame: &mut Frame, area: Rect, except: Rect) {
    let dim = palette::style(Sem::Dim)
        .fg
        .expect("Sem::Dim always sets fg");
    let buf = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if except.contains((x, y).into()) {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.fg = dim;
            }
        }
    }
}

fn sort_line(view: &ViewState) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, key) in SortKey::ALL.iter().enumerate() {
        let active = view.sort_key == Some(*key);
        let n = i + 1;
        if active {
            spans.push(Span::styled("[", palette::border_selected()));
            spans.push(Span::styled(n.to_string(), palette::border_selected()));
            spans.push(Span::styled("] ", palette::border_selected()));
            spans.push(Span::styled(
                format!("{} {}", key.label(), view.sort_dir.arrow()),
                palette::style(Sem::User),
            ));
        } else {
            spans.push(Span::styled(
                format!("[{n}] {}", key.label()),
                palette::style(Sem::Dim),
            ));
        }
        spans.push(Span::raw("  "));
    }
    Line::from(spans)
}

fn filter_line(palette: &Palette) -> Line<'static> {
    Line::from(vec![
        Span::styled("filter: ", palette::style(Sem::Dim)),
        Span::styled(palette.input.clone(), palette::style(Sem::Plain)),
        Span::styled("\u{2588}", palette::style(Sem::Dim)),
    ])
}

fn status_line(palette: &Palette, matched: usize, total: usize) -> Line<'static> {
    match &palette.error {
        Some(message) => Line::from(Span::styled(message.clone(), palette::style(Sem::Error))),
        None => Line::from(Span::styled(
            format!("matches: {matched} of {total}"),
            palette::style(Sem::Stat),
        )),
    }
}

fn help_line() -> Line<'static> {
    Line::from(Span::styled(
        "[\u{21b5}] apply   [c] clear all   [Esc] cancel",
        palette::style(Sem::Dim),
    ))
}

/// A draft's match count, computed against the roster without touching
/// `ViewState::filter` — the palette's "N of M" updates as you type without
/// committing an unparseable or half-typed filter.
pub fn draft_matches(
    rows: &[crate::roster::RosterRow],
    view: &ViewState,
    input: &str,
) -> (usize, usize) {
    match Filter::parse(input) {
        Ok(filter) => {
            let mut probe = view.clone();
            probe.filter = filter;
            let out = crate::view::apply(rows, &probe);
            (out.matched, out.total)
        }
        Err(_) => (0, rows.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Liveness;
    use crossterm::event::KeyCode;

    fn key(c: char) -> KeyCode {
        KeyCode::Char(c)
    }

    fn row(key: &str, model: &str) -> crate::roster::RosterRow {
        crate::roster::RosterRow {
            id: format!("id-{key}"),
            key: key.to_string(),
            state: Liveness::Live,
            model: model.to_string(),
            last_tool: "-".to_string(),
            last_line: String::new(),
            in_tok: 0,
            out_tok: 0,
            cost: 0.0,
            elapsed: None,
            last_ts: 0.0,
            title: String::new(),
        }
    }

    #[test]
    fn sort_focus_digit_then_same_digit_flips_direction() {
        let mut view = ViewState::default();
        let mut palette = Palette::open(&view, PaletteFocus::Sort);

        assert!(!palette.handle_key(key('1'), &mut view));
        assert_eq!(view.sort_key, Some(SortKey::Model));
        assert_eq!(view.sort_dir, crate::view::SortDir::Asc);

        assert!(!palette.handle_key(key('1'), &mut view));
        assert_eq!(view.sort_dir, crate::view::SortDir::Desc);
    }

    #[test]
    fn sort_focus_ignores_letters_typed_for_filtering() {
        let mut view = ViewState::default();
        let mut palette = Palette::open(&view, PaletteFocus::Sort);
        palette.handle_key(key('m'), &mut view);
        assert!(palette.input.is_empty(), "sort focus must not eat text");
    }

    #[test]
    fn filter_focus_types_into_the_draft_including_digits() {
        let mut view = ViewState::default();
        let mut palette = Palette::open(&view, PaletteFocus::Filter);
        for c in "cost>1.5".chars() {
            palette.handle_key(key(c), &mut view);
        }
        assert_eq!(palette.input, "cost>1.5");
        assert_eq!(view.sort_key, None, "filter focus must not touch sort");
    }

    #[test]
    fn backspace_edits_the_filter_draft() {
        let mut view = ViewState::default();
        let mut palette = Palette::open(&view, PaletteFocus::Filter);
        for c in "abc".chars() {
            palette.handle_key(key(c), &mut view);
        }
        palette.handle_key(KeyCode::Backspace, &mut view);
        assert_eq!(palette.input, "ab");
    }

    #[test]
    fn enter_commits_a_valid_filter_and_closes() {
        let mut view = ViewState::default();
        let mut palette = Palette::open(&view, PaletteFocus::Filter);
        for c in "model=claude*".chars() {
            palette.handle_key(key(c), &mut view);
        }
        assert!(palette.handle_key(KeyCode::Enter, &mut view));
        assert_eq!(view.filter.chips(), ["model=claude*"]);
    }

    #[test]
    fn enter_with_a_bad_filter_shows_the_error_and_stays_open() {
        let mut view = ViewState::default();
        let mut palette = Palette::open(&view, PaletteFocus::Filter);
        for c in "cost>abc".chars() {
            palette.handle_key(key(c), &mut view);
        }
        assert!(!palette.handle_key(KeyCode::Enter, &mut view));
        assert!(palette.error.is_some());
        assert!(view.filter.is_empty(), "the bad draft was never committed");
    }

    #[test]
    fn esc_restores_the_view_exactly_as_it_was_on_open() {
        let mut view = ViewState::default();
        view.set_filter("cost>1").expect("valid filter");
        let before = view.clone();

        let mut palette = Palette::open(&view, PaletteFocus::Sort);
        palette.handle_key(key('3'), &mut view);
        assert_ne!(view.sort_key, before.sort_key, "sanity: the chip did apply");

        assert!(palette.handle_key(KeyCode::Esc, &mut view));
        assert_eq!(view, before);
    }

    #[test]
    fn clear_only_fires_in_sort_focus_so_filter_text_can_contain_c() {
        let mut view = ViewState::default();
        view.toggle_sort(SortKey::Cost);
        view.set_filter("cost>1").expect("valid filter");

        let mut palette = Palette::open(&view, PaletteFocus::Filter);
        palette.handle_key(key('c'), &mut view);
        assert_eq!(palette.input, "cost>1c", "typed as text, not a clear");

        let mut palette = Palette::open(&view, PaletteFocus::Sort);
        palette.handle_key(key('c'), &mut view);
        assert_eq!(view.sort_key, None);
        assert!(view.filter.is_empty());
    }

    #[test]
    fn draft_matches_updates_without_committing_the_filter() {
        let rows = vec![row("r1", "claude-sonnet-5"), row("r2", "gpt-6")];
        let view = ViewState::default();
        let (matched, total) = draft_matches(&rows, &view, "model=claude*");
        assert_eq!((matched, total), (1, 2));
        assert!(view.filter.is_empty(), "draft_matches must not mutate view");
    }

    #[test]
    fn draft_matches_on_an_unparseable_draft_reports_zero_of_total() {
        let rows = vec![row("r1", "claude-sonnet-5")];
        let view = ViewState::default();
        assert_eq!(draft_matches(&rows, &view, "cost>abc"), (0, 1));
    }
}
