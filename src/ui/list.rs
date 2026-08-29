//! List mode: the fleet as one dense row per session (the shared
//! [`roster`](crate::ui::roster) table), the totals and API ticker under it,
//! and a preview of the selected session at the bottom.
//!
//! [`render_preview`] takes already-rendered [`StyledLine`]s rather than a
//! [`RosterRow`], so [`preview_lines`] can hand it either the pane's live
//! transcript tail or the metadata fallback without touching the layout.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Paragraph};

use crate::render::{Seg, Sem, StyledLine, clip, fmt_elapsed};
use crate::roster::{RosterRow, commas, totals_line};
use crate::source::{Attn, Liveness};
use crate::ui::palette::{self, to_lines};
use crate::ui::roster::row_sems;
use crate::ui::{App, PREVIEW_HEIGHT, roster};

/// How much of `last_line` and the title the preview keeps — the same
/// ceiling the renderers use for tool results.
const PREVIEW_CLIP: usize = 200;

/// Transcript lines the preview box can show: its height less its border.
/// Anything older stays in the pane buffer for grid mode's scrollback.
const PREVIEW_BODY: usize = PREVIEW_HEIGHT as usize - 2;

/// Draw the whole list mode into `area`: rows, stats, preview.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let stats = stats_lines(app);
    let [rows_area, stats_area, preview_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(stats.len() as u16),
        Constraint::Length(PREVIEW_HEIGHT),
    ])
    .areas(area);

    roster::render(frame, rows_area, app);
    frame.render_widget(Paragraph::new(to_lines(&stats)), stats_area);

    let selected = app.selected_row();
    let title = selected.map_or("—", |r| r.key.as_str());
    let body = selected
        .map(|row| preview_lines(app, row))
        .unwrap_or_default();
    render_preview(frame, preview_area, title, &body);
}

/// Fleet totals, plus the newest API call when the ticker has one.
fn stats_lines(app: &App) -> Vec<StyledLine> {
    let mut lines = vec![totals_line(&app.roster)];
    if let Some(last) = app.ticker.last() {
        lines.push(StyledLine(vec![Seg::new(
            Sem::Dim,
            format!("api: {}", last.to_plain().trim()),
        )]));
    }
    lines
}

/// The preview body: the live transcript tail as soon as the session's
/// pane has produced lines, and roster metadata until then — a pane that
/// was just opened, or a source whose tailer cannot read its store.
pub fn preview_lines(app: &App, row: &RosterRow) -> Vec<StyledLine> {
    match app.panes.get(&row.key) {
        Some(buffer) if !buffer.is_empty() => buffer
            .iter()
            .skip(buffer.len().saturating_sub(PREVIEW_BODY))
            .cloned()
            .collect(),
        _ => meta_lines(row),
    }
}

/// What a session looks like before its tail arrives, from roster data
/// alone.
fn meta_lines(row: &RosterRow) -> Vec<StyledLine> {
    let sems = row_sems(row.state);
    vec![
        StyledLine(vec![
            Seg::new(sems.text, state_name(row.state)),
            Seg::new(Sem::Dim, " · tool "),
            Seg::new(Sem::Tool, row.last_tool.clone()),
            Seg::new(Sem::Dim, " · "),
            Seg::new(Sem::Dim, fmt_elapsed(row.elapsed)),
        ]),
        StyledLine(vec![Seg::new(Sem::Dim, clip(&row.title, PREVIEW_CLIP))]),
        StyledLine(vec![Seg::new(
            Sem::Plain,
            clip(&row.last_line, PREVIEW_CLIP),
        )]),
        StyledLine(vec![Seg::new(
            Sem::Stat,
            format!(
                "Σ {} in / {} out / ${:.4} [{}]",
                commas(row.in_tok),
                commas(row.out_tok),
                row.cost,
                row.model
            ),
        )]),
    ]
}

/// The bordered `preview — <key>` box. Takes rendered lines, not a row, so
/// the streaming tail can feed it unchanged.
pub fn render_preview(frame: &mut Frame, area: Rect, key: &str, lines: &[StyledLine]) {
    let block = Block::bordered()
        .border_style(palette::border())
        .title(format!("preview — {key}"));
    frame.render_widget(Paragraph::new(to_lines(lines)).block(block), area);
}

fn state_name(state: Liveness) -> &'static str {
    match state {
        Liveness::Live => "live",
        Liveness::Attention(Attn::PermWait) => "waiting on you",
        Liveness::Attention(Attn::Stuck) => "stuck",
        Liveness::Done => "done",
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    use super::*;
    use crate::render::DIM;

    /// Five sessions covering every liveness state, newest first, as the
    /// engine would deliver them.
    fn fixture() -> Vec<RosterRow> {
        vec![
            row("C:aaaaaa", Liveness::Live, 1.5),
            row("H:bbbbbb", Liveness::Attention(Attn::PermWait), 0.25),
            row("O:cccccc", Liveness::Attention(Attn::Stuck), 0.0),
            row("C:dddddd", Liveness::Done, 12.0),
            row("H:eeeeee", Liveness::Live, 0.125),
        ]
    }

    fn row(key: &str, state: Liveness, cost: f64) -> RosterRow {
        RosterRow {
            id: format!("id-{key}"),
            key: key.to_string(),
            state,
            model: "claude-sonnet-5".to_string(),
            last_tool: "Bash".to_string(),
            last_line: format!("▶ Bash working on {key} for a good long while now"),
            in_tok: 1_234_567,
            out_tok: 890,
            cost,
            elapsed: Some(187.0),
            last_ts: 0.0,
            title: "a title".to_string(),
            attn_elapsed: None,
        }
    }

    fn app(rows: Vec<RosterRow>) -> App {
        App {
            selected_id: rows.first().map(|r| r.id.clone()),
            roster: rows,
            ..App::default()
        }
    }

    fn draw(app: &App, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), app))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// The `y`th row of the buffer as plain text.
    fn text_at(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// The whole frame as text, for the assertions that only care that
    /// something reached the screen.
    fn all_text(buf: &Buffer) -> String {
        (0..buf.area.height)
            .map(|y| text_at(buf, y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_session_gets_a_row_with_its_glyph_key_and_cost() {
        let buf = draw(&app(fixture()), 100, 20);
        let (live, _) = palette::glyph_for_liveness(Liveness::Live);
        let (wait, _) = palette::glyph_for_liveness(Liveness::Attention(Attn::PermWait));
        let (stuck, _) = palette::glyph_for_liveness(Liveness::Attention(Attn::Stuck));
        let (done, _) = palette::glyph_for_liveness(Liveness::Done);

        let rows: Vec<String> = (0..5).map(|y| text_at(&buf, y)).collect();
        for (i, glyph) in [live, wait, stuck, done, live].iter().enumerate() {
            assert!(rows[i].starts_with(glyph), "row {i}: {:?}", rows[i]);
        }
        assert!(rows[0].contains("C:aaaaaa"), "{:?}", rows[0]);
        assert!(rows[0].contains("claude-sonnet-5 · 3m07s"), "{:?}", rows[0]);
        assert!(rows[0].contains("working on C:aaaaaa"), "{:?}", rows[0]);
        assert!(rows[0].ends_with("$1.5000"), "{:?}", rows[0]);
        assert!(rows[3].ends_with("$12.0000"), "{:?}", rows[3]);
    }

    #[test]
    fn a_done_row_is_dim_from_end_to_end() {
        let buf = draw(&app(fixture()), 100, 20);
        let dim = Color::Rgb(DIM.r, DIM.g, DIM.b);
        // Row 3 is the finished session; every painted cell is dim.
        for x in 0..buf.area.width {
            assert_eq!(buf[(x, 3)].fg, dim, "cell {x} of the done row is not dim");
        }
        // Its live neighbour is not.
        assert!((0..buf.area.width).any(|x| buf[(x, 0)].fg != dim));
    }

    #[test]
    fn attention_rows_take_their_state_color() {
        let buf = draw(&app(fixture()), 100, 20);
        let amber = palette::style(Sem::User).fg.unwrap();
        let red = palette::style(Sem::Error).fg.unwrap();
        // The key column of the perm-wait row is amber, the stuck one red.
        let key_col = roster::W_GLYPH as u16;
        assert_eq!(buf[(key_col, 1)].fg, amber);
        assert_eq!(buf[(key_col, 2)].fg, red);
    }

    #[test]
    fn the_selected_row_is_the_only_one_with_the_selection_background() {
        let mut state = app(fixture());
        state.selected_id = Some("id-O:cccccc".to_string());
        let buf = draw(&state, 100, 20);
        let bg = palette::selection_bg().bg.unwrap();

        for x in 0..buf.area.width {
            assert_eq!(buf[(x, 2)].bg, bg, "cell {x} of the selected row lacks bg");
        }
        for y in [0, 1, 3, 4] {
            assert!(
                (0..buf.area.width).all(|x| buf[(x, y)].bg != bg),
                "row {y} should not be highlighted"
            );
        }
    }

    #[test]
    fn totals_and_ticker_sit_between_the_list_and_the_preview() {
        let mut state = app(fixture());
        state.ticker = vec![StyledLine(vec![Seg::new(
            Sem::Dim,
            "  14:23:03 sess03 #  3 claude-sonnet-5@anthropic in=3,000 out=3 1.25s",
        )])];
        let buf = draw(&state, 100, 20);
        let rendered: Vec<String> = (0..20).map(|y| text_at(&buf, y)).collect();

        let totals = rendered
            .iter()
            .position(|l| l.contains("live ·"))
            .expect("totals line");
        assert_eq!(
            rendered[totals],
            "1 ⏸ · 1 ⚠ · 2 live · 1 done · Σ $13.88 · 6,172,835 in"
        );
        assert!(rendered[totals + 1].starts_with("api: 14:23:03 sess03"));
        assert!(
            rendered[totals + 2].contains("preview — C:aaaaaa"),
            "{:?}",
            rendered[totals + 2]
        );
    }

    #[test]
    fn the_preview_shows_the_selected_session_not_the_first() {
        let mut state = app(fixture());
        state.selected_id = Some("id-C:dddddd".to_string());
        let rendered = all_text(&draw(&state, 100, 20));

        assert!(rendered.contains("preview — C:dddddd"), "{rendered}");
        assert!(rendered.contains("done · tool Bash · 3m07s"), "{rendered}");
        assert!(rendered.contains("working on C:dddddd"), "{rendered}");
        assert!(
            rendered.contains("Σ 1,234,567 in / 890 out / $12.0000 [claude-sonnet-5]"),
            "{rendered}"
        );
    }

    /// Once the pane is streaming, the preview is the transcript tail — the
    /// metadata summary it showed while waiting is gone.
    #[test]
    fn the_preview_shows_the_buffered_tail_once_the_pane_streams() {
        let mut state = App::default();
        state.apply_event(crate::engine::Event::Roster(fixture()));
        state.apply_event(crate::engine::Event::PaneLines {
            key: "C:aaaaaa".to_string(),
            lines: (1..=6)
                .map(|i| StyledLine(vec![Seg::new(Sem::Plain, format!("transcript line {i}"))]))
                .collect(),
        });
        let rendered = all_text(&draw(&state, 100, 20));

        assert!(rendered.contains("preview — C:aaaaaa"), "{rendered}");
        // Only the last four fit; the box shows the newest end of the tail.
        assert!(!rendered.contains("transcript line 2"), "{rendered}");
        for i in 3..=6 {
            assert!(
                rendered.contains(&format!("transcript line {i}")),
                "{rendered}"
            );
        }
        assert!(
            !rendered.contains("Σ 1,234,567 in"),
            "metadata should have given way to the tail: {rendered}"
        );
    }

    /// A session whose pane has produced nothing yet — just opened, or a
    /// source with no tailer — still gets the metadata preview.
    #[test]
    fn the_preview_falls_back_to_metadata_with_no_buffered_lines() {
        let mut state = App::default();
        state.apply_event(crate::engine::Event::Roster(fixture()));
        let rendered = all_text(&draw(&state, 100, 20));
        assert!(rendered.contains("live · tool Bash · 3m07s"), "{rendered}");
    }

    #[test]
    fn a_narrow_frame_truncates_the_summary_instead_of_panicking() {
        let buf = draw(&app(fixture()), 60, 16);
        let first = text_at(&buf, 0);
        assert!(first.contains('…'), "summary not truncated: {first:?}");
        assert!(first.ends_with("$1.5000"), "{first:?}");
        assert_eq!(first.chars().count(), 60, "the row should fill the width");
    }

    #[test]
    fn very_narrow_and_very_short_frames_still_render() {
        for (w, h) in [(20u16, 3u16), (12, 1), (1, 1), (40, 2)] {
            draw(&app(fixture()), w, h);
        }
    }

    #[test]
    fn the_empty_deck_names_the_paths_it_is_watching() {
        let state = App {
            paths: vec!["/home/me/.claude/projects".to_string()],
            ..App::default()
        };
        let rendered = all_text(&draw(&state, 60, 12));

        assert!(rendered.contains("no agent sessions found"), "{rendered}");
        assert!(
            rendered.contains("watching /home/me/.claude/projects"),
            "{rendered}"
        );
        assert!(
            rendered.contains("0 live · 0 done · Σ $0.00 · 0 in"),
            "{rendered}"
        );
        assert!(rendered.contains("preview — —"), "{rendered}");
    }

    #[test]
    fn the_cursor_scrolls_into_view_on_a_short_pane() {
        let rows: Vec<RosterRow> = (0..12)
            .map(|i| row(&format!("C:row{i:03}"), Liveness::Live, 0.0))
            .collect();
        let mut state = app(rows);
        state.selected_id = Some("id-C:row011".to_string());
        let rendered = all_text(&draw(&state, 80, 10));

        assert!(
            rendered.contains("C:row011"),
            "last row scrolled out: {rendered}"
        );
        assert!(
            !rendered.contains("C:row000"),
            "first row should be scrolled off: {rendered}"
        );
    }
}
