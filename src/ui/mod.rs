//! Terminal UI widgets built on ratatui.
//!
//! `run_tui` is the app shell for `hermon watch`: raw mode + alternate
//! screen, a panic hook that restores the terminal before the panic prints,
//! the engine thread, and the key/redraw loop. The body is [`list`], the
//! only mode there is until grid mode lands.

pub mod list;
pub mod palette;
pub mod pane;
pub mod roster;

use std::io;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::{Frame, Terminal};

use crate::config::EngineConfig;
use crate::engine::{Engine, Event, UiCmd};
use crate::render::StyledLine;
use crate::roster::RosterRow;

const FOOTER: &str = "[q]uit [j/k]select [?]help";
const HELP: &str = "q / Ctrl-C  quit\nj / \u{2193}       next\nk / \u{2191}       previous\n?           toggle help";
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const REDRAW_INTERVAL: Duration = Duration::from_millis(100);
/// Preview box: four lines of session detail plus its border.
const PREVIEW_HEIGHT: u16 = 6;

/// UI state: the latest roster from the engine plus what the user has done
/// with it. Pure data — every transition is a plain method so tests can
/// drive it without a terminal.
#[derive(Debug, Default)]
pub struct App {
    pub roster: Vec<RosterRow>,
    pub ticker: Vec<StyledLine>,
    /// The selected session's [`RosterRow::id`], not its row number: the
    /// roster is re-sorted by activity every tick, so an index would slide
    /// the cursor onto whichever session happened to overtake it.
    pub selected_id: Option<String>,
    pub show_help: bool,
    pub quit: bool,
    /// The store paths the engine is watching, for the empty state.
    pub paths: Vec<String>,
}

impl App {
    fn new(config: &EngineConfig) -> Self {
        App {
            paths: vec![
                config.claude_dir.clone(),
                config.hermes_db.clone(),
                config.opencode_db.clone(),
            ],
            ..App::default()
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            KeyCode::Char('?') => self.show_help = !self.show_help,
            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_prev(),
            _ => {}
        }
    }

    pub fn apply_event(&mut self, event: Event) {
        match event {
            Event::Roster(rows) => {
                // Where the cursor sat, in case its session is gone: falling
                // back to the same position beats jumping to the top.
                let position = self.selected_index();
                self.roster = rows;
                if self.selected_row().is_none() {
                    self.select_at(position);
                }
            }
            Event::Ticker(lines) => self.ticker = lines,
            Event::Lifecycle { .. } | Event::Alert => {}
        }
    }

    /// The selected session, or `None` while the roster is empty.
    pub fn selected_row(&self) -> Option<&RosterRow> {
        let id = self.selected_id.as_ref()?;
        self.roster.iter().find(|r| &r.id == id)
    }

    /// Where the selected session currently sits; 0 when it is gone, which
    /// is also where an untouched cursor starts.
    pub fn selected_index(&self) -> usize {
        self.selected_id
            .as_ref()
            .and_then(|id| self.roster.iter().position(|r| &r.id == id))
            .unwrap_or(0)
    }

    fn select_at(&mut self, position: usize) {
        let position = position.min(self.roster.len().saturating_sub(1));
        self.selected_id = self.roster.get(position).map(|r| r.id.clone());
    }

    fn select_next(&mut self) {
        self.select_at(self.selected_index() + 1);
    }

    fn select_prev(&mut self) {
        self.select_at(self.selected_index().saturating_sub(1));
    }
}

/// Puts the terminal back into cooked mode on the main screen. Errors are
/// ignored so it is safe from any state — the panic hook, every exit path,
/// even before setup ran.
pub fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
}

/// Chains `restore` in front of the current panic hook, so the terminal is
/// usable again before the panic message prints.
fn install_panic_hook(restore: fn()) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        prev(info);
    }));
}

pub fn run_tui(config: EngineConfig) -> anyhow::Result<()> {
    let (event_tx, event_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let app = App::new(&config);
    let engine = Engine::spawn(config, event_tx, cmd_rx);

    install_panic_hook(restore_terminal);
    let result = run_terminal(app, &event_rx);
    restore_terminal();

    let _ = cmd_tx.send(UiCmd::Shutdown);
    let _ = engine.join();
    result
}

fn run_terminal(mut app: App, rx: &Receiver<Event>) -> anyhow::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    // Hidden switch for manually verifying the panic path restores the
    // terminal: HERMON_PANIC_TEST=1 hermon watch.
    if std::env::var_os("HERMON_PANIC_TEST").is_some() {
        panic!("HERMON_PANIC_TEST: forced panic to exercise terminal restore");
    }

    let mut last_draw = Instant::now();
    terminal.draw(|frame| draw(frame, &app))?;

    while !app.quit {
        let mut dirty = false;
        if event::poll(POLL_INTERVAL)?
            && let CtEvent::Key(key) = event::read()?
        {
            app.handle_key(key);
            dirty = true;
        }
        for event in rx.try_iter() {
            app.apply_event(event);
            dirty = true;
        }
        if dirty || last_draw.elapsed() >= REDRAW_INTERVAL {
            terminal.draw(|frame| draw(frame, &app))?;
            last_draw = Instant::now();
        }
    }
    Ok(())
}

/// A frame: the list view over a one-line footer, with the help overlay on
/// top when it is open.
pub fn draw(frame: &mut Frame, app: &App) {
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    list::render(frame, body, app);
    frame.render_widget(Paragraph::new(FOOTER), footer);

    if app.show_help {
        let area = centered(frame.area(), 28, 6);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(HELP).block(Block::bordered().title("help")),
            area,
        );
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ratatui::backend::TestBackend;

    use super::*;
    use crate::source::Liveness;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn row(key: &str) -> RosterRow {
        RosterRow {
            id: format!("id-{key}"),
            key: key.to_string(),
            state: Liveness::Live,
            model: "m".to_string(),
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

    /// The selected session's key, which is what the user actually tracks.
    fn selected_key(app: &App) -> Option<&str> {
        app.selected_row().map(|r| r.key.as_str())
    }

    #[test]
    fn q_quits() {
        let mut app = App::default();
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.quit);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = App::default();
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }

    #[test]
    fn plain_c_does_not_quit() {
        let mut app = App::default();
        app.handle_key(key(KeyCode::Char('c')));
        assert!(!app.quit);
    }

    #[test]
    fn question_mark_toggles_help() {
        let mut app = App::default();
        app.handle_key(key(KeyCode::Char('?')));
        assert!(app.show_help);
        app.handle_key(key(KeyCode::Char('?')));
        assert!(!app.show_help);
    }

    #[test]
    fn selection_is_bounded_by_roster_length() {
        let mut app = App::default();
        app.apply_event(Event::Roster(vec![row("a"), row("b"), row("c")]));
        assert_eq!(selected_key(&app), Some("a"), "first row starts selected");

        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(selected_key(&app), Some("c"));
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(selected_key(&app), Some("c"));

        app.handle_key(key(KeyCode::Char('k')));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(selected_key(&app), Some("a"));
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(selected_key(&app), Some("a"));
    }

    #[test]
    fn selection_stays_empty_on_empty_roster() {
        let mut app = App::default();
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(selected_key(&app), None);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(selected_key(&app), None);
        assert_eq!(app.selected_index(), 0);
    }

    /// The roster is re-sorted by activity every tick, so the cursor has to
    /// follow its session rather than its row number.
    #[test]
    fn selection_follows_its_session_across_a_reorder() {
        let mut app = App::default();
        app.apply_event(Event::Roster(vec![row("a"), row("b"), row("c")]));
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(selected_key(&app), Some("b"));
        assert_eq!(app.selected_index(), 1);

        app.apply_event(Event::Roster(vec![row("c"), row("a"), row("b")]));
        assert_eq!(selected_key(&app), Some("b"));
        assert_eq!(app.selected_index(), 2);
    }

    #[test]
    fn a_vanished_session_leaves_the_cursor_at_its_old_position() {
        let mut app = App::default();
        app.apply_event(Event::Roster(vec![row("a"), row("b"), row("c")]));
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(selected_key(&app), Some("b"));

        app.apply_event(Event::Roster(vec![row("a"), row("c")]));
        assert_eq!(selected_key(&app), Some("c"));
    }

    #[test]
    fn roster_shrink_clamps_selection() {
        let mut app = App::default();
        app.apply_event(Event::Roster(vec![row("a"), row("b"), row("c")]));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(selected_key(&app), Some("c"));

        app.apply_event(Event::Roster(vec![row("a")]));
        assert_eq!(selected_key(&app), Some("a"));
    }

    #[test]
    fn the_ticker_event_is_kept_for_the_stats_line() {
        let mut app = App::default();
        let tick = StyledLine(vec![crate::render::Seg::new(crate::render::Sem::Dim, "t")]);
        app.apply_event(Event::Ticker(vec![tick.clone()]));
        assert_eq!(app.ticker, vec![tick]);
    }

    static HOOK_CALLS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

    fn record_restore() {
        HOOK_CALLS.lock().unwrap().push("restore");
    }

    #[test]
    fn panic_hook_runs_restore_before_the_previous_hook() {
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| HOOK_CALLS.lock().unwrap().push("prev")));
        install_panic_hook(record_restore);

        let result = std::panic::catch_unwind(|| panic!("boom"));

        std::panic::set_hook(original);
        assert!(result.is_err());
        assert_eq!(*HOOK_CALLS.lock().unwrap(), ["restore", "prev"]);
    }

    #[test]
    fn draws_the_empty_state_and_footer_before_the_first_roster() {
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let app = App {
            paths: vec!["/tmp/claude".to_string()],
            ..App::default()
        };
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("no agent sessions found"), "{rendered}");
        assert!(rendered.contains("watching /tmp/claude"), "{rendered}");
        assert!(
            rendered.contains("[q]uit [j/k]select [?]help"),
            "{rendered}"
        );
    }

    #[test]
    fn help_overlay_renders_over_the_body() {
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let app = App {
            show_help: true,
            ..App::default()
        };
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("help"));
        assert!(rendered.contains("Ctrl-C"));
    }
}
