//! Desktop UI built on eframe/egui.
//!
//! `run_gui` is the app shell for `hermon gui`: it spawns its own [`Engine`]
//! exactly as [`crate::ui::run_tui`] does — the stores are read-only and
//! multi-consumer-safe, so the window is simply a second consumer, with no
//! bridge to the TUI and nothing serialized between them.
//!
//! Two things are specific to immediate mode. First, egui only repaints on
//! input or on request, so [`wake`] parks the engine's [`Event`] stream on
//! its own thread with a cloned [`egui::Context`] and nudges the window awake
//! per event; [`App::ui`] then drains what arrived. That keeps a session
//! change visible within one engine tick while an untouched window costs no
//! frames at all. Second, all row/sort/filter logic stays in
//! [`crate::view`] — this module reads [`RosterRow`]s through
//! [`view::apply`] and never orders them itself.

pub mod palette;
pub mod pane;
pub mod roster;

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::anyhow;
use eframe::egui;

use crate::config::EngineConfig;
use crate::engine::{Engine, Event, Lifecycle, UiCmd};
use crate::render::StyledLine;
use crate::roster::RosterRow;
use crate::source::Liveness;
use crate::ui::pane::SCROLLBACK;
use crate::view::{self, ViewState};

const WINDOW_SIZE: [f32; 2] = [900.0, 620.0];
const MIN_WINDOW_SIZE: [f32; 2] = [480.0, 320.0];
/// How much of the window the pane area starts with, and the least it can be
/// dragged down to.
const PANE_AREA_HEIGHT: f32 = 300.0;
const PANE_AREA_MIN: f32 = 80.0;

/// A pane whose session has produced no transcript yet.
static NO_LINES: VecDeque<StyledLine> = VecDeque::new();

/// Opens the window and blocks until it closes, then shuts the engine down
/// and joins it.
pub fn run_gui(config: EngineConfig) -> anyhow::Result<()> {
    // The store paths the engine is watching, for the empty state — taken
    // before `config` moves into the engine below.
    let paths = vec![
        config.claude_dir.clone(),
        config.hermes_db.clone(),
        config.opencode_db.clone(),
    ];
    let max_panes = config.max_panes;
    let (event_tx, event_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let engine = Engine::spawn(config, event_tx, cmd_rx);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(App::default_title())
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size(MIN_WINDOW_SIZE),
        ..eframe::NativeOptions::default()
    };
    let mut event_rx = Some(event_rx);
    let app_cmds = cmd_tx.clone();
    let result = eframe::run_native(
        "hermon",
        options,
        Box::new(move |cc| {
            palette::install(&cc.egui_ctx);
            let events = event_rx.take().expect("app creator runs once");
            Ok(Box::new(App::new(
                wake(events, cc.egui_ctx.clone()),
                app_cmds,
                paths,
                max_panes,
            )))
        }),
    );

    // The window is gone either way — a failed launch still leaves an engine
    // thread polling, so shut it down before reporting the error.
    let _ = cmd_tx.send(UiCmd::Shutdown);
    let _ = engine.join();
    result.map_err(|err| anyhow!("gui: {err}"))
}

/// Relays engine events onto a second channel, repainting for each one.
///
/// This is the repaint discipline in one function: egui redraws on input, so
/// a window nobody is touching would show a frozen roster without something
/// off-thread telling it otherwise. Requests coalesce per frame, so an event
/// burst still costs one repaint, and a quiet fleet costs none.
///
/// The thread ends on its own when either side of the relay hangs up — the
/// engine exiting, or the window dropping the app — so it is deliberately
/// not joined.
fn wake(events: Receiver<Event>, ctx: egui::Context) -> Receiver<Event> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for event in events {
            if tx.send(event).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
    rx
}

/// App state: the latest roster from the engine plus the buffers and
/// selection the later desktop tickets draw from. Pure data apart from the
/// channel — [`App::apply_event`] is the whole update path, so tests drive
/// it without a window.
pub struct App {
    pub roster: Vec<RosterRow>,
    pub ticker: Vec<StyledLine>,
    /// The selected session's [`RosterRow::id`], not its row number: the
    /// roster re-sorts by activity every tick. #75's table sets it; the pane
    /// under the table follows it.
    pub selected_id: Option<String>,
    /// Transcript lines per open pane, oldest first. Only open panes have an
    /// entry: closing one drops its buffer, so reopening shows the engine's
    /// fresh replay instead of it twice.
    pub panes: HashMap<String, VecDeque<StyledLine>>,
    /// The active sort, filter and attention-first flag (#40's pure core),
    /// which #75's header clicks and #77's controls drive.
    pub view: ViewState,
    /// The store paths the engine is watching, for the empty state —
    /// [`crate::ui::App::paths`]'s desktop twin.
    pub paths: Vec<String>,
    /// Whether the pane area tiles every session with a slot, rather than
    /// just the selected one.
    pub grid: bool,
    /// Whether the selected pane has the window to itself.
    pub zoomed: bool,
    /// The engine's `--max-panes` ceiling: the grid never asks it to tail
    /// more than it would keep.
    pub max_panes: usize,
    /// Follow-scroll and wrap state per pane, keyed like [`App::panes`].
    pane_views: HashMap<String, pane::PaneView>,
    /// The keys whose tailers the engine currently holds open — tracked
    /// separately from the selection so a roster reorder that changes nothing
    /// visible does not churn them.
    open_panes: Vec<String>,
    cmds: Sender<UiCmd>,
    events: Receiver<Event>,
    /// The title last pushed to the window, so an unchanged one costs no
    /// viewport command.
    title: String,
}

impl App {
    pub fn new(
        events: Receiver<Event>,
        cmds: Sender<UiCmd>,
        paths: Vec<String>,
        max_panes: usize,
    ) -> Self {
        App {
            roster: Vec::new(),
            ticker: Vec::new(),
            selected_id: None,
            panes: HashMap::new(),
            view: ViewState::default(),
            paths,
            grid: false,
            zoomed: false,
            max_panes,
            pane_views: HashMap::new(),
            open_panes: Vec::new(),
            cmds,
            events,
            title: Self::default_title(),
        }
    }

    /// The title before the first roster arrives, and the one the window
    /// opens with.
    pub fn default_title() -> String {
        title_for(0)
    }

    /// Applies everything that has arrived since the last frame. Returns the
    /// window title the new state calls for, or `None` if it is unchanged.
    pub fn drain(&mut self) -> Option<String> {
        while let Ok(event) = self.events.try_recv() {
            self.apply_event(event);
        }
        let title = title_for(self.live_count());
        (title != self.title).then(|| {
            self.title = title.clone();
            title
        })
    }

    pub fn apply_event(&mut self, event: Event) {
        match event {
            Event::Roster(rows) => self.roster = rows,
            Event::Ticker(lines) => self.ticker = lines,
            Event::PaneLines { key, lines } => self.buffer_pane(&key, lines),
            Event::Lifecycle { key, change } => self.apply_lifecycle(&key, change),
            // Delivery already happened in the engine by the time this
            // lands; the window has nothing further to do with it.
            Event::Alert(_) => {}
        }
    }

    /// Sessions the fleet counts as live, using the TUI's vocabulary: ⏸/⚠
    /// rows are attention, not live (`ui::list`'s fleet totals).
    pub fn live_count(&self) -> usize {
        self.roster
            .iter()
            .filter(|row| row.state == Liveness::Live)
            .count()
    }

    /// Drops a pane's buffer when the engine closes or reopens its tailer,
    /// as [`crate::ui::App`] does: the stale tail would otherwise look
    /// frozen forever, or replay its history twice.
    fn apply_lifecycle(&mut self, key: &str, change: Lifecycle) {
        match change {
            Lifecycle::Evicted | Lifecycle::Resumed => self.drop_pane(key),
            Lifecycle::Started
            | Lifecycle::Finished(_)
            | Lifecycle::Attention(_)
            | Lifecycle::Cleared => {}
        }
    }

    /// Appends a fast tick's lines to a pane's buffer, dropping the oldest
    /// past [`SCROLLBACK`]. Lines for any other key are stale — in flight
    /// when the pane closed — and discarded.
    fn buffer_pane(&mut self, key: &str, lines: Vec<StyledLine>) {
        if !self.open_panes.iter().any(|open| open == key) {
            return;
        }
        let arrived = lines.len();
        let buffer = self.panes.entry(key.to_string()).or_default();
        buffer.extend(lines);
        while buffer.len() > SCROLLBACK {
            buffer.pop_front();
        }
        // The pane's wrap is cached, and a paused pane counts what it is not
        // showing: both live on the view, which the writer keeps honest.
        let view = self.pane_views.entry(key.to_string()).or_default();
        view.invalidate();
        view.follow.appended(arrived);
    }

    fn drop_pane(&mut self, key: &str) {
        self.panes.remove(key);
        self.pane_views.remove(key);
    }

    /// The sessions the engine should be tailing: the selected one, or in
    /// grid mode every session with a slot, capped at the `--max-panes` the
    /// engine would keep open anyway.
    fn wanted_panes(&self) -> Vec<String> {
        if !self.grid {
            return self.selected_key().into_iter().collect();
        }
        view::apply(&self.roster, &self.view)
            .rows
            .iter()
            .take(self.max_panes)
            .map(|row| row.key.clone())
            .collect()
    }

    /// Brings the engine's open tailers in line with what the window shows,
    /// through the same [`UiCmd`]s the TUI sends. A pane that loses its slot
    /// is closed and its buffer dropped, since the engine replays history on
    /// reopen and the stale tail would show twice.
    fn sync_panes(&mut self) {
        let wanted = self.wanted_panes();
        if wanted == self.open_panes {
            return;
        }
        for old in &self.open_panes {
            if !wanted.contains(old) {
                self.panes.remove(old);
                self.pane_views.remove(old);
                let _ = self.cmds.send(UiCmd::ClosePane(old.clone()));
            }
        }
        for new in &wanted {
            if !self.open_panes.contains(new) {
                let _ = self.cmds.send(UiCmd::OpenPane(new.clone()));
            }
        }
        self.open_panes = wanted;
    }

    /// The selected session's row, or `None` while the roster is empty or
    /// the filter hides it.
    pub fn selected_row(&self) -> Option<&RosterRow> {
        let id = self.selected_id.as_deref()?;
        view::apply(&self.roster, &self.view)
            .rows
            .into_iter()
            .find(|row| row.id == id)
    }

    fn selected_key(&self) -> Option<String> {
        self.selected_row().map(|row| row.key.clone())
    }

    /// The keyboard half of the pane controls: zoom in and out, and park the
    /// selected pane at the top of its scrollback or back on its tail. #77
    /// owns the rest of the key map.
    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (escape, enter, top, tail) = ctx.input(|input| {
            (
                input.key_pressed(egui::Key::Escape),
                input.key_pressed(egui::Key::Enter),
                input.key_pressed(egui::Key::G) && !input.modifiers.shift,
                input.key_pressed(egui::Key::G) && input.modifiers.shift,
            )
        });
        if escape {
            self.zoomed = false;
        }
        if enter && self.selected_row().is_some() {
            self.zoomed = true;
        }
        if let Some(key) = self.selected_key().filter(|_| top || tail) {
            let view = self.pane_views.entry(key).or_default();
            if top {
                view.jump_top();
            } else {
                view.jump_latest();
            }
        }
    }

    /// The body: the fleet table with the pane area under it, or a single
    /// zoomed pane with the window to itself.
    fn draw(&mut self, ui: &mut egui::Ui) {
        self.handle_keys(ui.ctx());
        if self.zoomed && self.selected_row().is_some() {
            self.draw_panes(ui);
        } else {
            self.zoomed = false;
            egui::Panel::bottom("hermon-panes")
                .resizable(true)
                .default_size(PANE_AREA_HEIGHT)
                .min_size(PANE_AREA_MIN)
                .show(ui, |ui| self.draw_panes(ui));
            egui::CentralPanel::default().show(ui, |ui| roster::render(ui, self));
        }
        self.sync_panes();
    }

    /// The pane area: one pane for the selected session, or a tile per open
    /// session in grid mode. Zoom collapses either back to the selected one.
    fn draw_panes(&mut self, ui: &mut egui::Ui) {
        let keys: Vec<String> = if self.grid && !self.zoomed {
            self.open_panes.clone()
        } else {
            self.selected_key().into_iter().collect()
        };
        if keys.is_empty() {
            ui.label(egui::RichText::new("no session selected").color(palette::DIM));
            return;
        }

        let cells = tile(ui.available_rect_before_wrap(), keys.len());
        for (key, cell) in keys.iter().zip(cells) {
            let action = ui
                .scope_builder(egui::UiBuilder::new().max_rect(cell), |ui| {
                    self.draw_pane(ui, key)
                })
                .inner;
            if action.clicked || action.double_clicked {
                self.selected_id = self
                    .roster
                    .iter()
                    .find(|row| &row.key == key)
                    .map(|row| row.id.clone());
            }
            if action.double_clicked {
                self.zoomed = !self.zoomed;
            }
        }
    }

    /// One session's pane. Its row carries the chrome (model, state, pinned),
    /// its buffer the transcript, its view the scroll state — three maps that
    /// stay disjoint so the draw can borrow them all at once.
    fn draw_pane(&mut self, ui: &mut egui::Ui, key: &str) -> pane::PaneAction {
        let Some(row) = self.roster.iter().find(|row| row.key == key) else {
            return pane::PaneAction::default();
        };
        let widget = pane::Pane {
            key: &row.key,
            model: &row.model,
            state: row.state,
            selected: self.selected_id.as_deref() == Some(row.id.as_str()),
            pinned: self.view.is_pinned(&row.id),
            attn_elapsed: row.attn_elapsed,
            lines: self.panes.get(&row.key).unwrap_or(&NO_LINES),
        };
        let view = self.pane_views.entry(row.key.clone()).or_default();
        pane::render(ui, &widget, view)
    }
}

/// `n` equal cells filling `area`, in the tiling the artboards call for: one
/// fills it, two split it, four make a square, six go three across.
fn tile(area: egui::Rect, n: usize) -> Vec<egui::Rect> {
    let cols = (n as f32).sqrt().ceil().max(1.0);
    let rows = (n as f32 / cols).ceil().max(1.0);
    let size = egui::vec2(area.width() / cols, area.height() / rows);
    (0..n)
        .map(|i| {
            let x = (i % cols as usize) as f32;
            let y = (i / cols as usize) as f32;
            egui::Rect::from_min_size(area.min + egui::vec2(x * size.x, y * size.y), size)
        })
        .collect()
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(title) = self.drain() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(title));
        }
        // The root `Ui` comes with neither margin nor fill of its own.
        egui::Frame::central_panel(ui.style()).show(ui, |ui| self.draw(ui));
    }

    /// Behind the panels too, so a resize never flashes egui's default gray.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        palette::BG.to_normalized_gamma_f32()
    }
}

/// `hermon — 3 live`, the window title.
fn title_for(live: usize) -> String {
    format!("hermon \u{2014} {live} live")
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Sender;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::engine::Cause;
    use crate::source::Attn;
    use crate::view;

    fn row(key: &str, state: Liveness) -> RosterRow {
        RosterRow {
            id: format!("{key}-id"),
            key: key.to_string(),
            state,
            model: "sonnet-4.5".to_string(),
            last_tool: "Bash".to_string(),
            last_line: "running".to_string(),
            in_tok: 0,
            out_tok: 0,
            cost: Some(0.0),
            elapsed: Some(187.0),
            last_ts: 0.0,
            title: "t".to_string(),
            attn_elapsed: None,
        }
    }

    fn app() -> (Sender<Event>, App) {
        let (tx, rx) = mpsc::channel();
        let (cmds, _) = mpsc::channel();
        (tx, App::new(rx, cmds, Vec::new(), 6))
    }

    /// An app whose engine commands can be read back, for the pane tests.
    fn app_with_cmds() -> (Sender<Event>, Receiver<UiCmd>, App) {
        let (tx, rx) = mpsc::channel();
        let (cmds, cmd_rx) = mpsc::channel();
        (tx, cmd_rx, App::new(rx, cmds, Vec::new(), 6))
    }

    /// One egui pass with no window behind it. The output carries texture
    /// deltas a real backend would upload; headless they only have to be
    /// acknowledged before the output is dropped.
    fn pass(ctx: &egui::Context, ui: impl FnMut(&mut egui::Ui)) {
        let mut out = ctx.run_ui(egui::RawInput::default(), ui);
        out.textures_delta.clear();
    }

    #[test]
    fn drain_applies_events_and_retitles_once_per_change() {
        let (tx, mut app) = app();
        tx.send(Event::Roster(vec![
            row("C:aaa", Liveness::Live),
            row("H:bbb", Liveness::Live),
            row("O:ccc", Liveness::Done),
        ]))
        .unwrap();

        assert_eq!(app.drain().as_deref(), Some("hermon \u{2014} 2 live"));
        assert_eq!(app.roster.len(), 3);
        // Same fleet next tick: no viewport command.
        tx.send(Event::Roster(vec![
            row("C:aaa", Liveness::Live),
            row("H:bbb", Liveness::Live),
            row("O:ccc", Liveness::Done),
        ]))
        .unwrap();
        assert_eq!(app.drain(), None);
    }

    #[test]
    fn attention_rows_are_not_counted_live() {
        let (_tx, mut app) = app();
        app.apply_event(Event::Roster(vec![
            row("C:aaa", Liveness::Attention(Attn::PermWait)),
            row("H:bbb", Liveness::Attention(Attn::Stuck)),
        ]));
        assert_eq!(app.live_count(), 0);
    }

    /// An app with one selected session whose pane the engine has been asked
    /// to tail — the state every pane test starts from.
    fn app_with_pane(key: &str) -> App {
        let (_tx, mut app) = app();
        app.apply_event(Event::Roster(vec![row(key, Liveness::Live)]));
        app.selected_id = Some(format!("{key}-id"));
        app.sync_panes();
        app
    }

    #[test]
    fn pane_lines_buffer_and_lifecycle_drops_them() {
        let mut app = app_with_pane("C:aaa");
        app.apply_event(Event::PaneLines {
            key: "C:aaa".to_string(),
            lines: vec![StyledLine::default(), StyledLine::default()],
        });
        assert_eq!(app.panes["C:aaa"].len(), 2);

        // A finish leaves the pane alone; an eviction or a resumption drops
        // it, since the engine's own tailer closed or is about to replay.
        app.apply_event(Event::Lifecycle {
            key: "C:aaa".to_string(),
            change: Lifecycle::Finished(Cause::Clean),
        });
        assert_eq!(app.panes["C:aaa"].len(), 2);
        app.apply_event(Event::Lifecycle {
            key: "C:aaa".to_string(),
            change: Lifecycle::Evicted,
        });
        assert!(!app.panes.contains_key("C:aaa"));
    }

    /// Lines for a pane nobody opened were in flight when it closed: taking
    /// them would leave a buffer no one ever draws or drops.
    #[test]
    fn lines_for_a_closed_pane_are_discarded() {
        let (_tx, mut app) = app();
        app.apply_event(Event::PaneLines {
            key: "C:aaa".to_string(),
            lines: vec![StyledLine::default()],
        });
        assert!(!app.panes.contains_key("C:aaa"));
    }

    #[test]
    fn pane_buffer_is_capped_at_scrollback() {
        let mut app = app_with_pane("C:aaa");
        app.apply_event(Event::PaneLines {
            key: "C:aaa".to_string(),
            lines: vec![StyledLine::default(); SCROLLBACK + 10],
        });
        assert_eq!(app.panes["C:aaa"].len(), SCROLLBACK);
    }

    /// The cursor is what the engine tails: selecting a session opens its
    /// pane, moving off it closes the old one and opens the new, and the
    /// closed pane's buffer goes with it.
    #[test]
    fn the_selection_drives_the_engine_s_tailers() {
        let (tx, cmds, mut app) = app_with_cmds();
        tx.send(Event::Roster(vec![
            row("C:aaa", Liveness::Live),
            row("H:bbb", Liveness::Live),
        ]))
        .unwrap();
        app.drain();

        app.selected_id = Some("C:aaa-id".to_string());
        app.sync_panes();
        assert_eq!(
            cmds.try_iter().collect::<Vec<_>>(),
            vec![UiCmd::OpenPane("C:aaa".to_string())]
        );
        app.apply_event(Event::PaneLines {
            key: "C:aaa".to_string(),
            lines: vec![StyledLine::default()],
        });

        app.selected_id = Some("H:bbb-id".to_string());
        app.sync_panes();
        assert_eq!(
            cmds.try_iter().collect::<Vec<_>>(),
            vec![
                UiCmd::ClosePane("C:aaa".to_string()),
                UiCmd::OpenPane("H:bbb".to_string()),
            ]
        );
        assert!(
            !app.panes.contains_key("C:aaa"),
            "a closed pane's buffer goes with it, or the engine's replay shows twice"
        );
    }

    /// Grid mode tails every session with a slot, never more than the engine
    /// would keep open under `--max-panes`.
    #[test]
    fn grid_mode_opens_every_slot_up_to_max_panes() {
        let (_tx, cmds, mut app) = app_with_cmds();
        let keys = ["C:aaa", "H:bbb", "O:ccc", "C:ddd"];
        app.apply_event(Event::Roster(
            keys.iter().map(|k| row(k, Liveness::Live)).collect(),
        ));
        app.max_panes = 3;
        app.grid = true;
        app.sync_panes();

        let opened: Vec<UiCmd> = cmds.try_iter().collect();
        assert_eq!(opened.len(), 3, "{opened:?}");
        for key in &keys[..3] {
            assert!(opened.contains(&UiCmd::OpenPane(key.to_string())), "{key}");
        }
    }

    /// A paused pane counts what it is not showing; the buffer's writer is
    /// what keeps the badge honest.
    #[test]
    fn lines_arriving_behind_a_paused_pane_feed_the_badge() {
        let mut app = app_with_pane("C:aaa");
        app.pane_views
            .entry("C:aaa".to_string())
            .or_default()
            .follow
            .observe(false);
        app.apply_event(Event::PaneLines {
            key: "C:aaa".to_string(),
            lines: vec![StyledLine::default(); 4],
        });
        assert_eq!(app.pane_views["C:aaa"].follow.unseen, 4);
    }

    /// The repaint discipline, headless: a quiet fleet stops asking for
    /// frames, and one engine event both reaches the app and wakes the
    /// context back up.
    #[test]
    fn a_quiet_window_settles_and_an_engine_event_wakes_it() {
        let ctx = egui::Context::default();
        let (tx, engine_rx) = mpsc::channel();
        let (cmds, _cmd_rx) = mpsc::channel();
        let mut app = App::new(wake(engine_rx, ctx.clone()), cmds, Vec::new(), 6);
        app.apply_event(Event::Roster(vec![row("C:aaa", Liveness::Live)]));

        // A fresh context takes a pass or two to settle (fonts, first
        // layout); after that an untouched window must cost nothing.
        let mut settled = false;
        for _ in 0..8 {
            pass(&ctx, |ui| app.draw(ui));
            settled = !ctx.has_requested_repaint();
            if settled {
                break;
            }
        }
        assert!(settled, "a quiet fleet must not repaint continuously");

        tx.send(Event::Roster(vec![
            row("C:aaa", Liveness::Live),
            row("H:bbb", Liveness::Live),
        ]))
        .unwrap();
        // The relay repaints after handing the event over, so a woken
        // context means the event is already there to drain.
        let deadline = Instant::now() + Duration::from_secs(1);
        while !ctx.has_requested_repaint() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            ctx.has_requested_repaint(),
            "an engine event must wake an idle window"
        );
        assert_eq!(app.drain().as_deref(), Some("hermon \u{2014} 2 live"));
    }

    /// The draw path end to end, with no display: egui lays the frame out on
    /// the CPU, so CI exercises it exactly as the window does.
    #[test]
    fn a_fleet_and_an_empty_deck_both_draw() {
        let (_tx, mut app) = app();
        let ctx = egui::Context::default();
        pass(&ctx, |ui| app.draw(ui));

        app.apply_event(Event::Roster(vec![
            row("C:aaa", Liveness::Live),
            row("H:bbb", Liveness::Attention(Attn::Stuck)),
        ]));
        pass(&ctx, |ui| app.draw(ui));
    }

    /// The three body layouts — a pane under the table, a grid of tiles, and
    /// one zoomed pane with the window to itself — all lay out headless.
    #[test]
    fn the_pane_area_draws_selected_grid_and_zoomed() {
        let (_tx, mut app) = app();
        let ctx = egui::Context::default();
        app.apply_event(Event::Roster(vec![
            row("C:aaa", Liveness::Live),
            row("H:bbb", Liveness::Attention(Attn::Stuck)),
            row("O:ccc", Liveness::Done),
        ]));
        app.selected_id = Some("C:aaa-id".to_string());
        app.sync_panes();
        app.apply_event(Event::PaneLines {
            key: "C:aaa".to_string(),
            lines: vec![StyledLine::default(); 20],
        });
        pass(&ctx, |ui| app.draw(ui));

        app.grid = true;
        app.sync_panes();
        pass(&ctx, |ui| app.draw(ui));

        app.zoomed = true;
        pass(&ctx, |ui| app.draw(ui));
        assert!(app.zoomed, "a zoom with a selection survives the frame");
    }

    /// Zoom needs something to zoom: with no selection it collapses back
    /// rather than leaving the window blank.
    #[test]
    fn zoom_without_a_selection_falls_back_to_the_table() {
        let (_tx, mut app) = app();
        let ctx = egui::Context::default();
        app.zoomed = true;
        pass(&ctx, |ui| app.draw(ui));
        assert!(!app.zoomed);
    }

    /// `Enter` zooms the selected pane and `Esc` comes back, through the
    /// same `ctx.input` path #77 extends.
    #[test]
    fn enter_zooms_and_escape_returns() {
        let (_tx, mut app) = app();
        let ctx = egui::Context::default();
        app.apply_event(Event::Roster(vec![row("C:aaa", Liveness::Live)]));
        app.selected_id = Some("C:aaa-id".to_string());

        let key = |key: egui::Key| egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
            ..egui::RawInput::default()
        };
        let mut out = ctx.run_ui(key(egui::Key::Enter), |ui| app.draw(ui));
        out.textures_delta.clear();
        assert!(app.zoomed);

        let mut out = ctx.run_ui(key(egui::Key::Escape), |ui| app.draw(ui));
        out.textures_delta.clear();
        assert!(!app.zoomed);
    }

    #[test]
    fn rows_are_read_through_the_view() {
        let (_tx, mut app) = app();
        app.apply_event(Event::Roster(vec![
            row("C:aaa", Liveness::Live),
            row("H:bbb", Liveness::Live),
        ]));
        app.view.set_filter("key=H:*").unwrap();
        let rows = view::apply(&app.roster, &app.view).rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "H:bbb");
    }

    /// The table's selection is keyed by [`RosterRow::id`], the same
    /// invariant [`ViewState::pinned`] relies on: a tick that reorders the
    /// roster must not lose or shift the cursor.
    #[test]
    fn selection_survives_a_tick_reorder() {
        let (_tx, mut app) = app();
        app.apply_event(Event::Roster(vec![
            row("C:aaa", Liveness::Live),
            row("H:bbb", Liveness::Live),
        ]));
        app.selected_id = Some("H:bbb-id".to_string());

        // Newest activity first next tick: the same two sessions, reversed.
        app.apply_event(Event::Roster(vec![
            row("H:bbb", Liveness::Live),
            row("C:aaa", Liveness::Live),
        ]));

        let rows = view::apply(&app.roster, &app.view).rows;
        assert_eq!(app.selected_id.as_deref(), Some("H:bbb-id"));
        assert!(
            rows.iter()
                .any(|r| Some(r.id.as_str()) == app.selected_id.as_deref())
        );
    }
}
