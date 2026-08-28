//! Terminal UI widgets built on ratatui.
//!
//! `run_tui` is the app shell for `hermon watch`: raw mode + alternate
//! screen, a panic hook that restores the terminal before the panic prints,
//! the engine thread, and the key/redraw loop. The body drawn here is a
//! placeholder; the real list view is issue #21.

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
use crate::roster::RosterRow;

const FOOTER: &str = "[q]uit [j/k]select [?]help";
const HELP: &str = "q / Ctrl-C  quit\nj / \u{2193}       next\nk / \u{2191}       previous\n?           toggle help";
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const REDRAW_INTERVAL: Duration = Duration::from_millis(100);

/// UI state: the latest roster from the engine plus what the user has done
/// with it. Pure data — every transition is a plain method so tests can
/// drive it without a terminal.
#[derive(Debug, Default)]
pub struct App {
    pub roster: Vec<RosterRow>,
    pub selected: usize,
    pub show_help: bool,
    pub quit: bool,
}

impl App {
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
                self.roster = rows;
                self.selected = self.selected.min(self.roster.len().saturating_sub(1));
            }
            Event::Lifecycle { .. } | Event::Alert => {}
        }
    }

    fn select_next(&mut self) {
        self.selected = (self.selected + 1).min(self.roster.len().saturating_sub(1));
    }

    fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
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
    let engine = Engine::spawn(config, event_tx, cmd_rx);

    install_panic_hook(restore_terminal);
    let result = run_terminal(&event_rx);
    restore_terminal();

    let _ = cmd_tx.send(UiCmd::Shutdown);
    let _ = engine.join();
    result
}

fn run_terminal(rx: &Receiver<Event>) -> anyhow::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    // Hidden switch for manually verifying the panic path restores the
    // terminal: HERMON_PANIC_TEST=1 hermon watch.
    if std::env::var_os("HERMON_PANIC_TEST").is_some() {
        panic!("HERMON_PANIC_TEST: forced panic to exercise terminal restore");
    }

    let mut app = App::default();
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

/// Placeholder frame until the list view lands (#21): session count, footer,
/// and the help overlay.
pub fn draw(frame: &mut Frame, app: &App) {
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    frame.render_widget(
        Paragraph::new(format!("{} sessions", app.roster.len())),
        body,
    );
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
            key: key.to_string(),
            state: Liveness::Live,
            model: "m".to_string(),
            last_tool: "-".to_string(),
            in_tok: 0,
            out_tok: 0,
            cost: 0.0,
            elapsed: None,
            last_ts: 0.0,
            title: String::new(),
        }
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

        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.selected, 2);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, 2);

        app.handle_key(key(KeyCode::Char('k')));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.selected, 0);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn selection_stays_zero_on_empty_roster() {
        let mut app = App::default();
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, 0);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn roster_shrink_clamps_selection() {
        let mut app = App::default();
        app.apply_event(Event::Roster(vec![row("a"), row("b"), row("c")]));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, 2);

        app.apply_event(Event::Roster(vec![row("a")]));
        assert_eq!(app.selected, 0);
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
    fn draws_empty_state_before_first_roster() {
        let mut terminal = Terminal::new(TestBackend::new(30, 4)).unwrap();
        let app = App::default();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        terminal.backend().assert_buffer_lines([
            "0 sessions                    ",
            "                              ",
            "                              ",
            "[q]uit [j/k]select [?]help    ",
        ]);
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
