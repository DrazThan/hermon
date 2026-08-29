//! Roster table widget: every recent session with state, model, tokens, cost.
//!
//! List mode gives it the whole body; grid mode gets the same rows squeezed
//! into the few lines above the tiles, so the table lives here rather than in
//! either mode.
//!
//! A row is `glyph · key · model/elapsed · what it is doing · cost`, with the
//! summary column absorbing whatever width is left over. Attention states
//! tint the row, finished sessions go entirely dim.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::render::{Seg, Sem, StyledLine, clip, fmt_elapsed};
use crate::roster::RosterRow;
use crate::source::{Attn, Liveness};
use crate::ui::App;
use crate::ui::palette;

/// Row column widths. The glyph column is two wide because the ASCII
/// fallback for `⏸` is `||`; the summary column takes the remainder.
pub const W_GLYPH: usize = 2;
/// The pin column: 📌 (or its `*` fallback) plus a pad space.
pub const W_PIN: usize = 2;
const W_KEY: usize = 10;
const W_META: usize = 26;
const W_COST: usize = 9;

/// Draws the roster into `area`, or the empty state when the deck is bare
/// or the active filter hides every row. Taller decks scroll just enough to
/// keep the cursor on screen.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    if app.roster.is_empty() {
        frame.render_widget(
            Paragraph::new(palette::to_lines(&empty_state(&app.paths))),
            area,
        );
        return;
    }

    let rows = app.visible_rows();
    if rows.is_empty() {
        frame.render_widget(Paragraph::new(palette::to_lines(&no_matches_state())), area);
        return;
    }

    let selected = app.selected_index();
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let pinned = app.view.is_pinned(&row.id);
            let line = row_line(row, pinned, area.width as usize);
            if i == selected {
                line.patch_style(palette::selection_bg())
            } else {
                line
            }
        })
        .collect();

    // Scrollback is M3; this only keeps the cursor on screen when the fleet
    // is taller than the pane.
    let offset = (selected + 1).saturating_sub(area.height as usize);
    frame.render_widget(Paragraph::new(lines).scroll((offset as u16, 0)), area);
}

/// One session as a padded, full-width row. Padding matters: the selected
/// row's background is only visible where the line has cells.
fn row_line(row: &RosterRow, pinned: bool, width: usize) -> Line<'static> {
    let (glyph, glyph_style) = palette::glyph_for_liveness(row.state);
    let sems = row_sems(row.state);
    let meta = format!("{} · {}", row.model, fmt_elapsed(row.elapsed));
    let cost = format!("${:.4}", row.cost);
    let w_summary = width.saturating_sub(W_GLYPH + W_PIN + W_KEY + W_META + W_COST);

    // An unpinned row leaves the column blank in the row's own text color, so
    // a finished row still goes dim end to end; a pinned one always pops
    // amber, done or not — the accent the pane border echoes.
    let (pin_glyph, pin_style) = if pinned {
        (palette::pin_glyph(), palette::style(Sem::User))
    } else {
        ("", palette::style(sems.text))
    };

    let mut spans = vec![
        Span::styled(format!("{glyph:<W_GLYPH$}"), glyph_style),
        Span::styled(format!("{pin_glyph:<W_PIN$}"), pin_style),
        Span::styled(
            format!("{:<W_KEY$}", clip(&row.key, W_KEY - 1)),
            palette::style(sems.text),
        ),
        Span::styled(
            format!("{:<W_META$}", clip(&meta, W_META - 1)),
            palette::style(sems.meta),
        ),
    ];
    if w_summary > 0 {
        spans.push(Span::styled(
            format!("{:<w_summary$}", clip(&row.last_line, w_summary - 1)),
            palette::style(sems.text),
        ));
    }
    spans.push(Span::styled(
        format!("{cost:>W_COST$}"),
        palette::style(sems.cost),
    ));
    Line::from(spans)
}

/// The colors a row paints with: a session needing attention tints its text
/// and cost amber or red, a finished one goes dim throughout.
pub struct RowSems {
    pub text: Sem,
    pub meta: Sem,
    pub cost: Sem,
}

pub fn row_sems(state: Liveness) -> RowSems {
    match state {
        Liveness::Live => RowSems {
            text: Sem::Plain,
            meta: Sem::Dim,
            cost: Sem::Stat,
        },
        Liveness::Attention(Attn::PermWait) => RowSems {
            text: Sem::User,
            meta: Sem::Dim,
            cost: Sem::User,
        },
        Liveness::Attention(Attn::Stuck) => RowSems {
            text: Sem::Error,
            meta: Sem::Dim,
            cost: Sem::Error,
        },
        Liveness::Done => RowSems {
            text: Sem::Dim,
            meta: Sem::Dim,
            cost: Sem::Dim,
        },
    }
}

/// What the roster says when a filter is active but matches nothing.
fn no_matches_state() -> Vec<StyledLine> {
    vec![StyledLine(vec![Seg::new(
        Sem::Dim,
        "no sessions match the active filter",
    )])]
}

/// What a fresh, sessionless deck says: nothing found, and where it looked.
fn empty_state(paths: &[String]) -> Vec<StyledLine> {
    let mut lines = vec![
        StyledLine(vec![Seg::new(Sem::Dim, "no agent sessions found")]),
        StyledLine::default(),
    ];
    lines.extend(
        paths
            .iter()
            .map(|p| StyledLine(vec![Seg::new(Sem::Dim, format!("  watching {p}"))])),
    );
    lines
}
