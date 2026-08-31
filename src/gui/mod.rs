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
pub mod roster;

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use anyhow::anyhow;
use eframe::egui;

use crate::config::EngineConfig;
use crate::engine::{Engine, Event, Lifecycle, UiCmd};
use crate::render::StyledLine;
use crate::roster::RosterRow;
use crate::source::Liveness;
use crate::view::ViewState;

/// Transcript lines kept per open pane, as in [`crate::ui`]: far more than
/// any viewport shows, since the surplus is what #76's scrollback reads.
const PANE_SCROLLBACK: usize = 5_000;

const WINDOW_SIZE: [f32; 2] = [900.0, 620.0];
const MIN_WINDOW_SIZE: [f32; 2] = [480.0, 320.0];

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
    let result = eframe::run_native(
        "hermon",
        options,
        Box::new(move |cc| {
            palette::install(&cc.egui_ctx);
            let events = event_rx.take().expect("app creator runs once");
            Ok(Box::new(App::new(wake(events, cc.egui_ctx.clone()), paths)))
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
    /// roster re-sorts by activity every tick. Nothing sets it yet — #75
    /// owns the cursor.
    pub selected_id: Option<String>,
    /// Transcript lines per open pane, oldest first. Filled once #76 opens
    /// panes; until then the engine tails nothing and this stays empty.
    pub panes: HashMap<String, VecDeque<StyledLine>>,
    /// The active sort, filter and attention-first flag (#40's pure core),
    /// which #75's header clicks and #77's controls drive.
    pub view: ViewState,
    /// The store paths the engine is watching, for the empty state —
    /// [`crate::ui::App::paths`]'s desktop twin.
    pub paths: Vec<String>,
    events: Receiver<Event>,
    /// The title last pushed to the window, so an unchanged one costs no
    /// viewport command.
    title: String,
}

impl App {
    pub fn new(events: Receiver<Event>, paths: Vec<String>) -> Self {
        App {
            roster: Vec::new(),
            ticker: Vec::new(),
            selected_id: None,
            panes: HashMap::new(),
            view: ViewState::default(),
            paths,
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
            Lifecycle::Evicted | Lifecycle::Resumed => {
                self.panes.remove(key);
            }
            Lifecycle::Started
            | Lifecycle::Finished(_)
            | Lifecycle::Attention(_)
            | Lifecycle::Cleared => {}
        }
    }

    /// Appends a fast tick's lines to a pane's buffer, dropping the oldest
    /// past [`PANE_SCROLLBACK`].
    fn buffer_pane(&mut self, key: &str, lines: Vec<StyledLine>) {
        let buffer = self.panes.entry(key.to_string()).or_default();
        buffer.extend(lines);
        while buffer.len() > PANE_SCROLLBACK {
            buffer.pop_front();
        }
    }

    /// The body: the fleet table, real widgets and all — [`roster::render`]
    /// owns the layout, this stays a thin call so the update path is still
    /// one function ([`App::apply_event`]) tests can drive without a window.
    fn draw(&mut self, ui: &mut egui::Ui) {
        roster::render(ui, self);
    }
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
        (tx, App::new(rx, Vec::new()))
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

    #[test]
    fn pane_lines_buffer_and_lifecycle_drops_them() {
        let (_tx, mut app) = app();
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

    #[test]
    fn pane_buffer_is_capped_at_scrollback() {
        let (_tx, mut app) = app();
        app.apply_event(Event::PaneLines {
            key: "C:aaa".to_string(),
            lines: vec![StyledLine::default(); PANE_SCROLLBACK + 10],
        });
        assert_eq!(app.panes["C:aaa"].len(), PANE_SCROLLBACK);
    }

    /// The repaint discipline, headless: a quiet fleet stops asking for
    /// frames, and one engine event both reaches the app and wakes the
    /// context back up.
    #[test]
    fn a_quiet_window_settles_and_an_engine_event_wakes_it() {
        let ctx = egui::Context::default();
        let (tx, engine_rx) = mpsc::channel();
        let mut app = App::new(wake(engine_rx, ctx.clone()), Vec::new());
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
