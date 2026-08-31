//! The fleet table: sortable columns, attention colors, selection and
//! totals — the desktop twin of [`crate::ui::roster`]'s list mode.
//!
//! Every bit of row logic — attention grouping, sort order, the empty
//! states — is read through [`crate::view`] and [`crate::ui::roster`]; this
//! module only turns their output into egui widgets. Sort state and the
//! selected session id live on [`super::App`], not here, so #76 and #77 can
//! read and drive the same fields.

use eframe::egui::{self, Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder, TableRow};

use crate::render::{StyledLine, fmt_elapsed};
use crate::roster::{RosterRow, commas, fmt_cost, tool_annotation, totals_line};
use crate::ui::roster::{empty_state, no_matches_state, row_sems};
use crate::view::{self, SortKey, ViewState};

use super::App;
use super::palette;

const ROW_HEIGHT: f32 = 20.0;
const HEADER_HEIGHT: f32 = 22.0;

/// Draws the top bar, the fleet table (or the matching empty state), and
/// the totals/ticker status strip into `ui`.
pub fn render(ui: &mut Ui, app: &mut App) {
    render_top_bar(ui, app);
    ui.separator();

    if app.roster.is_empty() {
        render_lines(ui, &empty_state(&app.paths));
        return;
    }

    let output = view::apply(&app.roster, &app.view);
    if output.rows.is_empty() {
        render_lines(ui, &no_matches_state());
        ui.separator();
        render_status_strip(ui, app);
        return;
    }
    let rows = output.rows;

    let selected_id = app.selected_id.clone();
    let mut sort_click: Option<SortKey> = None;
    let mut select_click: Option<String> = None;

    egui::ScrollArea::vertical()
        .id_salt("roster-table")
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .sense(egui::Sense::click())
                .column(Column::exact(24.0))
                .column(Column::initial(90.0).at_least(70.0))
                .column(Column::initial(130.0).at_least(90.0))
                .column(Column::remainder().at_least(100.0))
                .column(Column::initial(120.0).at_least(90.0))
                .column(Column::initial(80.0).at_least(60.0))
                .column(Column::initial(80.0).at_least(60.0))
                .header(HEADER_HEIGHT, |mut header| {
                    header.col(|_| {});
                    header.col(|ui| {
                        ui.label(RichText::new("key").color(palette::DIM));
                    });
                    sort_header(&mut header, &app.view, SortKey::Model, &mut sort_click);
                    sort_header(&mut header, &app.view, SortKey::Tool, &mut sort_click);
                    sort_header(&mut header, &app.view, SortKey::InOut, &mut sort_click);
                    sort_header(&mut header, &app.view, SortKey::Cost, &mut sort_click);
                    sort_header(&mut header, &app.view, SortKey::Elapsed, &mut sort_click);
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                        let r = rows[row.index()];
                        row.set_selected(selected_id.as_deref() == Some(r.id.as_str()));
                        row_cells(&mut row, r);
                        if row.response().clicked() {
                            select_click = Some(r.id.clone());
                        }
                    });
                });
        });

    if let Some(key) = sort_click {
        app.view.toggle_sort(key);
    }
    if let Some(id) = select_click {
        app.selected_id = Some(id);
    }

    ui.separator();
    render_status_strip(ui, app);
}

/// The attention-first and grid toggles — the desktop-native stand-ins for
/// the TUI's `[a]` and `[l]` keys. #77 adds the filter box and pin controls
/// next to them.
fn render_top_bar(ui: &mut Ui, app: &mut App) {
    ui.horizontal(|ui| {
        ui.toggle_value(&mut app.view.attention_first, "⚠ attention first");
        ui.toggle_value(&mut app.grid, "▦ grid");
    });
}

/// One sortable header cell: the column label plus the active sort's arrow,
/// clickable to activate the key or flip its direction via
/// [`ViewState::toggle_sort`].
fn sort_header(
    header: &mut TableRow<'_, '_>,
    view: &ViewState,
    key: SortKey,
    clicked: &mut Option<SortKey>,
) {
    header.col(|ui| {
        let label = match view.sort_key {
            Some(active) if active == key => format!("{} {}", key.label(), view.sort_dir.arrow()),
            _ => key.label().to_string(),
        };
        if ui
            .add(egui::Button::new(RichText::new(label).color(palette::DIM)).frame(false))
            .clicked()
        {
            *clicked = Some(key);
        }
    });
}

/// One row's seven cells, colored by the row's attention state through the
/// same [`row_sems`] mapping the TUI paints with.
fn row_cells(row: &mut TableRow<'_, '_>, r: &RosterRow) {
    let sems = row_sems(r.state);
    let (glyph, glyph_color) = palette::glyph_for_liveness(r.state);
    row.col(|ui| {
        ui.label(mono(glyph, glyph_color));
    });
    row.col(|ui| {
        ui.label(mono(&r.key, palette::color(sems.text)));
    });
    row.col(|ui| {
        ui.label(mono(&r.model, palette::color(sems.text)));
    });
    row.col(|ui| {
        ui.label(mono(&tool_annotation(r), palette::color(sems.text)));
    });
    row.col(|ui| {
        ui.label(mono(&inout_text(r), palette::color(sems.meta)));
    });
    row.col(|ui| {
        ui.label(mono(&fmt_cost(r.cost), palette::color(sems.cost)));
    });
    row.col(|ui| {
        ui.label(mono(&fmt_elapsed(r.elapsed), palette::color(sems.meta)));
    });
}

/// `12,345/6,789`, the IN/OUT column's text — [`SortKey::InOut`] orders by
/// the same two fields summed.
fn inout_text(r: &RosterRow) -> String {
    format!("{}/{}", commas(r.in_tok), commas(r.out_tok))
}

fn mono(text: &str, color: Color32) -> RichText {
    RichText::new(text.to_string()).color(color).monospace()
}

/// Fleet totals ([`totals_line`]) plus the Hermes API-call ticker
/// [`App::apply_event`] already keeps current — the same content `hermon
/// ls` prints under its table.
fn render_status_strip(ui: &mut Ui, app: &App) {
    render_lines(ui, std::slice::from_ref(&totals_line(&app.roster)));
    if !app.ticker.is_empty() {
        render_lines(ui, &app.ticker);
    }
}

/// Renders [`StyledLine`]s with each [`Seg`](crate::render::Seg)'s semantic
/// color, the same text both `hermon ls` and the TUI show.
fn render_lines(ui: &mut Ui, lines: &[StyledLine]) {
    for line in lines {
        if line.0.is_empty() {
            ui.label("");
            continue;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for seg in &line.0 {
                ui.label(mono(&seg.text, palette::color(seg.sem)));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::engine::Event;
    use crate::source::{Attn, Liveness};

    fn row(key: &str, state: Liveness) -> RosterRow {
        RosterRow {
            id: format!("{key}-id"),
            key: key.to_string(),
            state,
            model: "sonnet-4.5".to_string(),
            last_tool: "Bash".to_string(),
            last_line: "running".to_string(),
            in_tok: 1_234,
            out_tok: 56,
            cost: Some(1.5),
            elapsed: Some(187.0),
            last_ts: 0.0,
            title: "t".to_string(),
            attn_elapsed: None,
        }
    }

    #[test]
    fn inout_text_combines_commas_in_and_out() {
        assert_eq!(inout_text(&row("C:aaa", Liveness::Live)), "1,234/56");
    }

    /// One egui pass with no window behind it, matching [`super::super`]'s
    /// own headless test helper.
    fn pass(ctx: &egui::Context, ui: impl FnMut(&mut Ui)) {
        let mut out = ctx.run_ui(egui::RawInput::default(), ui);
        out.textures_delta.clear();
    }

    /// Every branch [`render`] can take must lay out without panicking: a
    /// bare deck, a populated fleet with the selection on and off a row and
    /// the ticker showing, and a filter that hides every row.
    #[test]
    fn render_lays_out_every_branch() {
        let ctx = egui::Context::default();
        let (_tx, rx) = mpsc::channel();
        let (cmds, _cmd_rx) = mpsc::channel();
        let mut app = App::new(rx, cmds, vec!["/tmp/claude".to_string()], 6);
        pass(&ctx, |ui| render(ui, &mut app));

        app.apply_event(Event::Roster(vec![
            row("C:aaa", Liveness::Live),
            row("H:bbb", Liveness::Attention(Attn::Stuck)),
        ]));
        app.selected_id = Some("C:aaa-id".to_string());
        app.apply_event(Event::Ticker(vec![StyledLine::default()]));
        pass(&ctx, |ui| render(ui, &mut app));

        app.view
            .set_filter("model=nomatch*")
            .expect("filter parses");
        pass(&ctx, |ui| render(ui, &mut app));
    }
}
