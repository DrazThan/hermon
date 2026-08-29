//! Terminal UI widgets built on ratatui.
//!
//! `run_tui` is the app shell for `hermon watch`: raw mode + alternate
//! screen, a panic hook that restores the terminal before the panic prints,
//! the engine thread, and the key/redraw loop. The body is one of two modes:
//! [`list`], the dense default, and grid mode — the compact [`roster`] table
//! over a wall of tiled [`pane`]s, laid out here.

pub mod list;
pub mod overlay;
pub mod palette;
pub mod pane;
pub mod roster;

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::{Frame, Terminal};

use crate::config::EngineConfig;
use crate::engine::{Engine, Event, Lifecycle, UiCmd};
use crate::render::{Sem, StyledLine};
use crate::roster::RosterRow;
use crate::ui::overlay::{Palette, PaletteFocus};
use crate::view::{self, ViewState};

const FOOTER_LIST: &str = "[q]uit [j/k]select [l]grid [s]ort [f]ilter [a]ttn [?]help";
const FOOTER_GRID: &str =
    "[q]uit [j/k]select [l]list [Tab]page [z]oom [x/o]close/open [s]ort [f]ilter [a]ttn [?]help";
const FOOTER_ZOOM: &str = "[q]uit [Esc]back [PgUp/PgDn]scroll [g/G]top/tail [?]help";
const HELP: &str = "q / Ctrl-C  quit\nj / \u{2193}       next\nk / \u{2191}       previous\nl           list / grid\nTab         next page\n\u{21b5} / z       zoom\nEsc         leave zoom\nPgUp/PgDn   scroll pane\ng / G       top / follow tail\nx / o       close / reopen\n?           toggle help";
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const REDRAW_INTERVAL: Duration = Duration::from_millis(100);
/// Preview box: four lines of session detail plus its border.
const PREVIEW_HEIGHT: u16 = 6;
/// Transcript lines kept for an open pane. Far more than the preview box
/// shows — the surplus is what grid mode's scrollback reads.
const PANE_SCROLLBACK: usize = 5_000;

/// Most panes the grid tiles at once, per the artboards: past this the last
/// tile becomes the `+ N more sessions` placeholder and `[Tab]` pages.
const GRID_SLOTS: usize = 6;
/// Roster rows above the tiles, and the three they collapse to under zoom.
const GRID_ROSTER_ROWS: u16 = 6;
const ZOOM_ROSTER_ROWS: u16 = 3;
/// Display lines one `PgUp`/`PgDn` moves a pane.
const SCROLL_STEP: usize = 10;
/// What `g` parks in the scroll offset. The pane clamps to whatever it can
/// actually scroll, so this just means "as far back as the buffer goes"; `G`
/// is the way home, since `PgDn` has to climb down from the sentinel.
const SCROLL_TOP: usize = usize::MAX;

/// A pane whose session has produced no transcript yet.
static NO_LINES: VecDeque<StyledLine> = VecDeque::new();

/// Which body the deck is showing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ViewMode {
    /// One dense row per session, with a preview of the selected one.
    #[default]
    List,
    /// The compact roster over a wall of tiled live panes.
    Grid,
}

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
    pub mode: ViewMode,
    /// Whether the selected pane has the body to itself.
    pub zoom: bool,
    /// The grid page on screen, when the fleet outgrows [`GRID_SLOTS`].
    pub page: usize,
    /// The store paths the engine is watching, for the empty state.
    pub paths: Vec<String>,
    /// Transcript lines per open pane, oldest first. Only open panes have an
    /// entry: closing one drops its buffer, so reopening shows the engine's
    /// fresh replay instead of it twice.
    pub panes: HashMap<String, VecDeque<StyledLine>>,
    /// How far back each pane is scrolled, in display lines below the
    /// viewport. Absent or zero follows the tail.
    scroll: HashMap<String, usize>,
    /// Sessions dismissed with `[x]`. They keep their roster row but give up
    /// their grid slot until `[o]` brings them back.
    closed: HashSet<String>,
    /// The keys whose tailers the engine currently holds open — tracked
    /// separately from the selection so a roster reorder that changes nothing
    /// visible does not churn them.
    open_panes: Vec<String>,
    /// Commands the run loop has not yet handed to the engine.
    cmds: Vec<UiCmd>,
    /// The active sort, filter and attention-first flag (#40's pure core).
    /// Everything that lists sessions — list mode, the grid's slots, the
    /// header chips — reads the fleet through [`App::visible_rows`] rather
    /// than `roster` directly, so it stays in sync with this.
    pub view: ViewState,
    /// The sort/filter overlay, open while `[s]`/`[f]` has it up.
    pub palette: Option<Palette>,
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
        if let Some(palette) = &mut self.palette {
            if palette.handle_key(key.code, &mut self.view) {
                self.palette = None;
            }
            self.resync_selection();
            self.sync_panes();
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
            KeyCode::Char('l') => self.toggle_mode(),
            KeyCode::Char('s') => {
                self.palette = Some(Palette::open(&self.view, PaletteFocus::Sort))
            }
            KeyCode::Char('f') => {
                self.palette = Some(Palette::open(&self.view, PaletteFocus::Filter))
            }
            KeyCode::Char('a') => {
                self.view.attention_first = !self.view.attention_first;
                self.resync_selection();
            }
            KeyCode::Char('c') => {
                self.view.clear();
                self.resync_selection();
            }
            code if self.mode == ViewMode::Grid => self.handle_grid_key(code),
            _ => {}
        }
        self.sync_panes();
    }

    /// The keys only grid mode answers to. List mode ignores them so its
    /// cursor and pane behaviour stay exactly as M2 left them.
    fn handle_grid_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab => self.next_page(),
            KeyCode::Enter => self.zoom = true,
            KeyCode::Char('z') => self.zoom = !self.zoom,
            KeyCode::Esc => self.zoom = false,
            KeyCode::PageUp => self.set_scroll(|offset| offset.saturating_add(SCROLL_STEP)),
            KeyCode::PageDown => self.set_scroll(|offset| offset.saturating_sub(SCROLL_STEP)),
            KeyCode::Char('g') => self.set_scroll(|_| SCROLL_TOP),
            KeyCode::Char('G') => self.set_scroll(|_| 0),
            KeyCode::Char('x') => {
                if let Some(key) = self.selected_key() {
                    self.closed.insert(key);
                    self.zoom = false;
                }
            }
            KeyCode::Char('o') => {
                if let Some(key) = self.selected_key() {
                    self.closed.remove(&key);
                }
            }
            _ => {}
        }
    }

    fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            ViewMode::List => ViewMode::Grid,
            ViewMode::Grid => ViewMode::List,
        };
        self.zoom = false;
    }

    fn selected_key(&self) -> Option<String> {
        self.selected_row().map(|row| row.key.clone())
    }

    fn set_scroll(&mut self, next: impl Fn(usize) -> usize) {
        let Some(key) = self.selected_key() else {
            return;
        };
        let offset = self.scroll.get(&key).copied().unwrap_or(0);
        self.scroll.insert(key, next(offset));
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
                self.sync_panes();
            }
            Event::Ticker(lines) => self.ticker = lines,
            Event::PaneLines { key, lines } => self.buffer_pane(&key, lines),
            Event::Lifecycle { key, change } => self.apply_lifecycle(&key, change),
            Event::Alert => {}
        }
    }

    /// Reacts to a lifecycle boundary the engine wants narrated. Eviction
    /// frees the grid slot exactly like `[x]` does — the session keeps its
    /// roster row, it just stops crowding a tile once max-panes reassigns
    /// its pane elsewhere — and a resurrection undoes that, since a session
    /// coming back to life is the opposite of dismissed. Both drop the
    /// pane's buffer: the engine's own tailer already closed or is about to
    /// reopen with a fresh replay, and the old tail left in place would
    /// either look frozen forever or show its history twice.
    fn apply_lifecycle(&mut self, key: &str, change: Lifecycle) {
        match change {
            Lifecycle::Evicted => {
                self.closed.insert(key.to_string());
                self.panes.remove(key);
                self.scroll.remove(key);
            }
            Lifecycle::Resumed => {
                self.closed.remove(key);
                self.panes.remove(key);
                self.scroll.remove(key);
            }
            Lifecycle::Started
            | Lifecycle::Finished(_)
            | Lifecycle::Attention(_)
            | Lifecycle::Cleared => {}
        }
    }

    /// Appends a fast tick's lines to a pane's buffer, dropping the oldest
    /// past [`PANE_SCROLLBACK`]. Lines for any other key are stale — in
    /// flight when the pane closed — and discarded.
    fn buffer_pane(&mut self, key: &str, lines: Vec<StyledLine>) {
        if !self.open_panes.iter().any(|open| open == key) {
            return;
        }
        let buffer = self.panes.entry(key.to_string()).or_default();
        buffer.extend(lines);
        while buffer.len() > PANE_SCROLLBACK {
            buffer.pop_front();
        }
    }

    /// The sessions the engine should be tailing: in list mode the selected
    /// one, in grid mode every session holding a slot on the visible page.
    fn wanted_panes(&self) -> Vec<String> {
        match self.mode {
            ViewMode::List => self.selected_key().into_iter().collect(),
            ViewMode::Grid => self.visible_slots(),
        }
    }

    /// Brings the engine's open tailers in line with what the screen shows.
    /// A pane that loses its slot is closed and its buffer dropped, since the
    /// engine replays history on reopen and the stale tail would show twice.
    fn sync_panes(&mut self) {
        let wanted = self.wanted_panes();
        if wanted == self.open_panes {
            return;
        }
        for old in &self.open_panes {
            if !wanted.contains(old) {
                self.panes.remove(old);
                self.scroll.remove(old);
                self.cmds.push(UiCmd::ClosePane(old.clone()));
            }
        }
        for new in &wanted {
            if !self.open_panes.contains(new) {
                self.cmds.push(UiCmd::OpenPane(new.clone()));
            }
        }
        self.open_panes = wanted;
    }

    /// The fleet sorted and filtered by [`App::view`] — what list mode
    /// shows and grid mode hands out slots from. Recomputed from `roster`
    /// each call rather than cached, since it is cheap and every mutation
    /// site would otherwise have to remember to invalidate it.
    pub fn visible_rows(&self) -> Vec<&RosterRow> {
        view::apply(&self.roster, &self.view).rows
    }

    /// Sessions eligible for a grid slot: the filtered, sorted fleet minus
    /// the ones dismissed with `[x]` — or, since the M4 lifecycle ticket, ones
    /// the engine itself evicted under `--max-panes` (see
    /// [`Self::apply_lifecycle`]), which land in the same `closed` set.
    fn grid_keys(&self) -> Vec<String> {
        self.visible_rows()
            .iter()
            .map(|row| row.key.clone())
            .filter(|key| !self.closed.contains(key))
            .collect()
    }

    /// The keys tiled on the current page.
    fn visible_slots(&self) -> Vec<String> {
        let keys = self.grid_keys();
        let size = page_size(keys.len());
        let page = self.page.min(self.page_count() - 1);
        keys.into_iter().skip(page * size).take(size).collect()
    }

    /// How many pages the fleet needs; at least one, even empty.
    fn page_count(&self) -> usize {
        let total = self.grid_keys().len();
        total.div_ceil(page_size(total)).max(1)
    }

    /// `[Tab]`: the next page, wrapping. The cursor comes along so zoom and
    /// the scroll keys keep acting on something that is on screen.
    fn next_page(&mut self) {
        let count = self.page_count();
        self.page = (self.page.min(count - 1) + 1) % count;
        let keys = self.grid_keys();
        if let Some(key) = keys.get(self.page * page_size(keys.len())) {
            self.selected_id = self
                .roster
                .iter()
                .find(|row| &row.key == key)
                .map(|row| row.id.clone());
        }
    }

    /// Pages the grid to wherever the cursor just landed, so walking off the
    /// bottom of one page steps onto the next.
    fn follow_selection(&mut self) {
        if self.mode != ViewMode::Grid {
            return;
        }
        let Some(key) = self.selected_key() else {
            return;
        };
        let keys = self.grid_keys();
        if let Some(index) = keys.iter().position(|k| *k == key) {
            self.page = index / page_size(keys.len());
        }
    }

    /// A session's pane, with whatever transcript and scroll it has so far.
    fn pane_for<'a>(&'a self, row: &'a RosterRow) -> pane::Pane<'a> {
        pane::Pane {
            key: &row.key,
            state: row.state,
            selected: self.selected_id.as_deref() == Some(row.id.as_str()),
            lines: self.panes.get(&row.key).unwrap_or(&NO_LINES),
            offset: self.scroll.get(&row.key).copied().unwrap_or(0),
            attn_elapsed: row.attn_elapsed,
        }
    }

    /// Hands the engine everything the last transition asked for.
    pub fn take_commands(&mut self) -> Vec<UiCmd> {
        std::mem::take(&mut self.cmds)
    }

    /// The selected session, or `None` while the roster is empty or the
    /// filter hides it.
    pub fn selected_row(&self) -> Option<&RosterRow> {
        let id = self.selected_id.clone()?;
        self.visible_rows().into_iter().find(|r| r.id == id)
    }

    /// Snaps the cursor onto a visible row when the current selection has
    /// gone missing — a session ending or a filter hiding it both look the
    /// same here: `selected_row` comes back empty. Falling back to the same
    /// position beats jumping to the top.
    fn resync_selection(&mut self) {
        let position = self.selected_index();
        if self.selected_row().is_none() {
            self.select_at(position);
        }
    }

    /// Where the selected session currently sits among the visible
    /// (filtered, sorted) rows; 0 when it is gone, which is also where an
    /// untouched cursor starts.
    pub fn selected_index(&self) -> usize {
        self.selected_id
            .as_ref()
            .and_then(|id| self.visible_rows().iter().position(|r| &r.id == id))
            .unwrap_or(0)
    }

    fn select_at(&mut self, position: usize) {
        let rows = self.visible_rows();
        let position = position.min(rows.len().saturating_sub(1));
        self.selected_id = rows.get(position).map(|r| r.id.clone());
    }

    fn select_next(&mut self) {
        self.select_at(self.selected_index() + 1);
        self.follow_selection();
    }

    fn select_prev(&mut self) {
        self.select_at(self.selected_index().saturating_sub(1));
        self.follow_selection();
    }
}

/// Sessions a grid page holds: the whole fleet when it fits, one slot short
/// of full when it does not — that last tile is the `+ N more` placeholder.
fn page_size(total: usize) -> usize {
    if total <= GRID_SLOTS {
        total.max(1)
    } else {
        GRID_SLOTS - 1
    }
}

/// The tiling for `n` panes as (rows, columns), per the artboards: one fills
/// the body, two split it left and right, up to four make a square, and the
/// rest go two deep by three across.
fn tiling(n: usize) -> (u16, u16) {
    match n {
        0 | 1 => (1, 1),
        2 => (1, 2),
        3 | 4 => (2, 2),
        _ => (2, 3),
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
    let result = run_terminal(app, &event_rx, &cmd_tx);
    restore_terminal();

    let _ = cmd_tx.send(UiCmd::Shutdown);
    let _ = engine.join();
    result
}

fn run_terminal(mut app: App, rx: &Receiver<Event>, cmd_tx: &Sender<UiCmd>) -> anyhow::Result<()> {
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
        // A dead engine is not the UI's problem to report: the failing
        // `Event` channel ends the loop on its own.
        for cmd in app.take_commands() {
            let _ = cmd_tx.send(cmd);
        }
        if dirty || last_draw.elapsed() >= REDRAW_INTERVAL {
            terminal.draw(|frame| draw(frame, &app))?;
            last_draw = Instant::now();
        }
    }
    Ok(())
}

/// A frame: the header chip bar (artboard 3a, only while a sort or filter is
/// active) over the active mode's body over a one-line footer, with the
/// sort/filter overlay and the help overlay on top when either is open.
pub fn draw(frame: &mut Frame, app: &App) {
    let header_height = u16::from(header_active(app));
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    if header_height > 0 {
        render_header(frame, header, app);
    }
    match app.mode {
        ViewMode::List => list::render(frame, body, app),
        ViewMode::Grid => render_grid(frame, body, app),
    }
    frame.render_widget(Paragraph::new(footer_text(app)), footer);

    if let Some(palette) = &app.palette {
        let (matched, total) = overlay::draft_matches(&app.roster, &app.view, &palette.input);
        overlay::render(frame, frame.area(), &app.view, palette, matched, total);
    }

    if app.show_help {
        let area = centered(frame.area(), 30, 13);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(HELP).block(Block::bordered().title("help")),
            area,
        );
    }
}

/// Whether the header chip bar has anything to show: an active sort or a
/// non-empty filter. `attention_first` alone (`[a]`) does not raise it —
/// that toggle has no chip of its own, per the artboard.
fn header_active(app: &App) -> bool {
    app.view.sort_key.is_some() || !app.view.filter.is_empty()
}

/// The chip bar (artboard 3a): `sort: cost ↓` plus one green chip per
/// filter term on the left, `N/M shown · [s]ort [f]ilter [c]lear`
/// right-aligned.
fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let out = view::apply(&app.roster, &app.view);

    let mut left: Vec<Span> = Vec::new();
    if let Some(key) = app.view.sort_key {
        left.push(Span::styled(
            format!("sort: {} {}", key.label(), app.view.sort_dir.arrow()),
            palette::style(Sem::Stat),
        ));
        left.push(Span::raw("  "));
    }
    for chip in app.view.filter.chips() {
        left.push(Span::styled(format!("[{chip}]"), palette::style(Sem::Ok)));
        left.push(Span::raw(" "));
    }

    let left_width: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right = format!(
        "{}/{} shown \u{b7} [s]ort [f]ilter [c]lear",
        out.matched, out.total
    );
    let width = area.width as usize;
    let pad = width.saturating_sub(left_width + right.chars().count());

    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(right, palette::style(Sem::Dim)));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn footer_text(app: &App) -> &'static str {
    match (app.mode, app.zoom) {
        (ViewMode::List, _) => FOOTER_LIST,
        (ViewMode::Grid, false) => FOOTER_GRID,
        (ViewMode::Grid, true) => FOOTER_ZOOM,
    }
}

/// Grid mode: the compact roster on top, the tiles under it. Zoom gives the
/// selected pane everything the collapsed roster leaves.
fn render_grid(frame: &mut Frame, area: Rect, app: &App) {
    let roster_rows = if app.roster.is_empty() {
        area.height
    } else if app.zoom {
        ZOOM_ROSTER_ROWS
    } else {
        GRID_ROSTER_ROWS.min(app.roster.len() as u16)
    };
    let [roster_area, tiles] =
        Layout::vertical([Constraint::Length(roster_rows), Constraint::Min(0)]).areas(area);
    roster::render(frame, roster_area, app);

    if tiles.is_empty() {
        return;
    }
    if app.zoom {
        if let Some(row) = app.selected_row() {
            pane::render(frame, tiles, &app.pane_for(row));
        }
        return;
    }
    render_tiles(frame, tiles, app);
}

fn render_tiles(frame: &mut Frame, area: Rect, app: &App) {
    let visible = app.visible_slots();
    let hidden = app.grid_keys().len() - visible.len();
    let slots = visible.len() + usize::from(hidden > 0);
    if slots == 0 {
        return;
    }

    let (rows, cols) = tiling(slots);
    let cells = tile_rects(area, rows, cols);
    for (cell, key) in cells.iter().zip(&visible) {
        if let Some(row) = app.roster.iter().find(|row| &row.key == key) {
            pane::render(frame, *cell, &app.pane_for(row));
        }
    }
    if hidden > 0
        && let Some(cell) = cells.get(visible.len())
    {
        render_placeholder(frame, *cell, hidden, app.page + 1, app.page_count());
    }
}

/// `rows` × `cols` equal cells, filled left to right and top to bottom.
fn tile_rects(area: Rect, rows: u16, cols: u16) -> Vec<Rect> {
    let bands = Layout::vertical(vec![Constraint::Ratio(1, rows.into()); rows.into()]).split(area);
    bands
        .iter()
        .flat_map(|band| {
            Layout::horizontal(vec![Constraint::Ratio(1, cols.into()); cols.into()])
                .split(*band)
                .to_vec()
        })
        .collect()
}

/// The tile standing in for the sessions this page has no room for.
fn render_placeholder(frame: &mut Frame, area: Rect, hidden: usize, page: usize, pages: usize) {
    let dim = palette::style(Sem::Dim);
    let label = format!("+ {hidden} more sessions \u{b7} page {page}/{pages}");
    frame.render_widget(
        Paragraph::new(Line::styled(label, dim))
            .centered()
            .block(Block::bordered().border_style(dim)),
        area,
    );
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
    use crate::source::{Attn, Liveness};
    use crate::view::SortKey;

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
            attn_elapsed: None,
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

    fn pane_line(text: &str) -> StyledLine {
        StyledLine(vec![crate::render::Seg::new(
            crate::render::Sem::Plain,
            text,
        )])
    }

    /// The engine only tails what the cursor is on, so every selection move
    /// closes one pane and opens the next.
    #[test]
    fn selection_opens_the_new_pane_and_closes_the_old_one() {
        let mut app = App::default();
        app.apply_event(Event::Roster(vec![row("a"), row("b")]));
        assert_eq!(
            app.take_commands(),
            vec![UiCmd::OpenPane("a".to_string())],
            "the first roster opens the selected session's pane"
        );

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.take_commands(),
            vec![
                UiCmd::ClosePane("a".to_string()),
                UiCmd::OpenPane("b".to_string()),
            ]
        );

        // A roster that leaves the cursor where it was churns nothing.
        app.apply_event(Event::Roster(vec![row("b"), row("a")]));
        assert!(app.take_commands().is_empty());
    }

    #[test]
    fn pane_lines_are_buffered_for_the_open_pane_only() {
        let mut app = App::default();
        app.apply_event(Event::Roster(vec![row("a"), row("b")]));
        app.apply_event(Event::PaneLines {
            key: "a".to_string(),
            lines: vec![pane_line("one"), pane_line("two")],
        });
        app.apply_event(Event::PaneLines {
            key: "b".to_string(),
            lines: vec![pane_line("not selected")],
        });

        let buffered: Vec<String> = app.panes["a"].iter().map(StyledLine::to_plain).collect();
        assert_eq!(buffered, ["one", "two"]);
        assert!(!app.panes.contains_key("b"), "stale lines were buffered");
    }

    /// Moving on drops the buffer: the engine replays history when the pane
    /// is reopened, and keeping the old tail would show it twice.
    #[test]
    fn closing_a_pane_drops_its_buffer() {
        let mut app = App::default();
        app.apply_event(Event::Roster(vec![row("a"), row("b")]));
        app.apply_event(Event::PaneLines {
            key: "a".to_string(),
            lines: vec![pane_line("one")],
        });
        app.handle_key(key(KeyCode::Char('j')));
        assert!(app.panes.is_empty());
    }

    #[test]
    fn the_pane_buffer_is_capped_and_keeps_the_newest_lines() {
        let mut app = App::default();
        app.apply_event(Event::Roster(vec![row("a")]));
        for chunk in 0..3 {
            app.apply_event(Event::PaneLines {
                key: "a".to_string(),
                lines: (0..2_000)
                    .map(|i| pane_line(&format!("line {}", chunk * 2_000 + i)))
                    .collect(),
            });
        }

        let buffer = &app.panes["a"];
        assert_eq!(buffer.len(), PANE_SCROLLBACK);
        assert_eq!(buffer.front().unwrap().to_plain(), "line 1000");
        assert_eq!(buffer.back().unwrap().to_plain(), "line 5999");
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
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        let app = App {
            paths: vec!["/tmp/claude".to_string()],
            ..App::default()
        };
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("no agent sessions found"), "{rendered}");
        assert!(rendered.contains("watching /tmp/claude"), "{rendered}");
        assert!(rendered.contains(FOOTER_LIST), "{rendered}");
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

    // --- grid mode ---

    fn state_row(key: &str, state: Liveness) -> RosterRow {
        RosterRow { state, ..row(key) }
    }

    /// A deck already switched into grid mode, with its slots open.
    fn grid(rows: Vec<RosterRow>) -> App {
        let mut app = App::default();
        app.handle_key(key(KeyCode::Char('l')));
        app.apply_event(Event::Roster(rows));
        app
    }

    fn buffer(app: &App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn screen(app: &App, width: u16, height: u16) -> String {
        let buf = buffer(app, width, height);
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn l_toggles_between_list_and_grid() {
        let mut app = App::default();
        assert_eq!(app.mode, ViewMode::List);
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.mode, ViewMode::Grid);
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.mode, ViewMode::List);
    }

    #[test]
    fn tiling_follows_the_artboards() {
        assert_eq!(tiling(1), (1, 1));
        assert_eq!(tiling(2), (1, 2));
        assert_eq!(tiling(3), (2, 2));
        assert_eq!(tiling(4), (2, 2));
        assert_eq!(tiling(5), (2, 3));
        assert_eq!(tiling(6), (2, 3));
    }

    /// A fleet that fits takes every slot; past that one slot goes to the
    /// placeholder, so a page carries five sessions.
    #[test]
    fn a_page_gives_up_a_slot_only_once_the_fleet_overflows() {
        assert_eq!(page_size(4), 4);
        assert_eq!(page_size(GRID_SLOTS), GRID_SLOTS);
        assert_eq!(page_size(GRID_SLOTS + 1), GRID_SLOTS - 1);
        assert_eq!(page_size(0), 1, "an empty deck still has one page");
    }

    #[test]
    fn four_sessions_tile_two_by_two_with_their_keys_and_state_colors() {
        let app = grid(vec![
            state_row("C:aaaaaa", Liveness::Live),
            state_row("H:bbbbbb", Liveness::Attention(Attn::PermWait)),
            state_row("O:cccccc", Liveness::Attention(Attn::Stuck)),
            state_row("C:dddddd", Liveness::Done),
        ]);
        let buf = buffer(&app, 80, 24);
        let rendered = screen(&app, 80, 24);

        for title in ["C:aaaaaa", "H:bbbbbb", "O:cccccc", "C:dddddd"] {
            assert!(rendered.contains(title), "{title} missing:\n{rendered}");
        }

        // Tiles start below the four roster rows: two bands of two columns.
        let corners = [(0, 4), (40, 4), (0, 14), (40, 14)];
        let expected = [
            palette::border_selected(), // the cursor starts on the first
            palette::style(Sem::User),
            palette::style(Sem::Error),
            palette::style(Sem::Dim),
        ];
        for ((x, y), style) in corners.into_iter().zip(expected) {
            assert_eq!(
                Some(buf[(x, y)].fg),
                style.fg,
                "border at {x},{y}:\n{rendered}"
            );
        }
        // The footer line is wider than the 80-column tiling grid above, so
        // it gets its own wider buffer rather than clipping mid-assertion.
        assert!(screen(&app, 100, 24).contains(FOOTER_GRID));
    }

    #[test]
    fn zoom_gives_the_selected_pane_the_body_and_collapses_the_roster() {
        let mut app = grid(vec![row("C:aaaaaa"), row("H:bbbbbb"), row("O:cccccc")]);
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Enter));
        let rendered = screen(&app, 80, 24);

        assert!(app.zoom);
        // Three roster rows, then one full-width pane: the other two tiles
        // are gone, and the zoomed one's border spans the screen.
        let lines: Vec<&str> = rendered.lines().collect();
        assert!(lines[3].starts_with('┌'), "{rendered}");
        assert!(lines[3].contains("H:bbbbbb"), "{rendered}");
        assert!(lines[3].ends_with('┐'), "{rendered}");
        assert!(!lines[3].contains("O:cccccc"), "{rendered}");
        assert!(rendered.contains(FOOTER_ZOOM), "{rendered}");

        app.handle_key(key(KeyCode::Esc));
        assert!(!app.zoom);
        app.handle_key(key(KeyCode::Char('z')));
        assert!(app.zoom, "z zooms back in");
        app.handle_key(key(KeyCode::Char('z')));
        assert!(!app.zoom, "z is the way back out too");
    }

    /// A pane narrow enough to wrap every line still draws — the widths that
    /// used to be the renderers' problem are the pane's now.
    #[test]
    fn a_sixty_column_grid_wraps_its_panes_without_panicking() {
        let mut app = grid(vec![row("C:aaaaaa"), row("H:bbbbbb"), row("O:cccccc")]);
        app.apply_event(Event::PaneLines {
            key: "C:aaaaaa".to_string(),
            lines: (0..40)
                .map(|i| pane_line(&format!("line {i}: {}", "wrap me please ".repeat(6))))
                .collect(),
        });
        for (w, h) in [(60u16, 20u16), (60, 10), (40, 8), (20, 6)] {
            screen(&app, w, h);
        }
        assert!(screen(&app, 60, 20).contains("wrap"), "nothing drawn");
    }

    fn scrollable() -> App {
        let mut app = grid(vec![row("C:aaaaaa")]);
        app.handle_key(key(KeyCode::Enter));
        app.apply_event(Event::PaneLines {
            key: "C:aaaaaa".to_string(),
            lines: (1..=30).map(|i| pane_line(&format!("line {i}"))).collect(),
        });
        app
    }

    #[test]
    fn paging_up_stops_following_the_tail_and_counts_what_is_below() {
        let mut app = scrollable();
        assert!(screen(&app, 80, 24).contains("line 30"), "not following");

        app.handle_key(key(KeyCode::PageUp));
        let rendered = screen(&app, 80, 24);
        assert!(
            rendered.contains("\u{25bc} 10 more"),
            "no scroll marker:\n{rendered}"
        );
        assert!(
            !rendered.contains("line 30"),
            "still following:\n{rendered}"
        );
        assert!(rendered.contains("line 20"), "{rendered}");
    }

    #[test]
    fn capital_g_resumes_following_the_tail_and_g_jumps_to_the_top() {
        let mut app = scrollable();
        app.handle_key(key(KeyCode::Char('g')));
        let top = screen(&app, 80, 24);
        assert!(top.contains("line 1 "), "not at the top:\n{top}");
        assert!(!top.contains("line 30"), "{top}");

        app.handle_key(key(KeyCode::Char('G')));
        let tail = screen(&app, 80, 24);
        assert!(tail.contains("line 30"), "not following again:\n{tail}");
        assert!(!tail.contains("more"), "marker left behind:\n{tail}");
    }

    #[test]
    fn ten_sessions_page_six_slots_with_a_placeholder_and_tab_shows_the_rest() {
        let rows: Vec<RosterRow> = (1..=10).map(|i| row(&format!("C:sess{i:02}"))).collect();
        let mut app = grid(rows);
        assert_eq!(app.page_count(), 2);

        let first = screen(&app, 120, 30);
        for i in 1..=5 {
            assert!(first.contains(&format!("C:sess{i:02}")), "{first}");
        }
        assert!(
            first.contains("+ 5 more sessions \u{b7} page 1/2"),
            "no placeholder tile:\n{first}"
        );

        app.handle_key(key(KeyCode::Tab));
        let second = screen(&app, 120, 30);
        assert_eq!(app.page, 1);
        for i in 6..=10 {
            assert!(
                second.contains(&format!("C:sess{i:02}")),
                "session {i} missing:\n{second}"
            );
        }
        assert!(
            second.contains("+ 5 more sessions \u{b7} page 2/2"),
            "{second}"
        );

        // Wrapping back round returns the first page.
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.page, 0);
    }

    /// The engine tails whatever holds a slot: every visible pane in grid
    /// mode, only the cursor's in list mode.
    #[test]
    fn open_panes_follow_the_grid_slots() {
        let mut app = App::default();
        app.apply_event(Event::Roster(vec![row("a"), row("b"), row("c")]));
        assert_eq!(app.take_commands(), vec![UiCmd::OpenPane("a".to_string())]);

        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(
            app.take_commands(),
            vec![
                UiCmd::OpenPane("b".to_string()),
                UiCmd::OpenPane("c".to_string()),
            ],
            "grid mode adds the rest of the slots"
        );

        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(
            app.take_commands(),
            vec![
                UiCmd::ClosePane("b".to_string()),
                UiCmd::ClosePane("c".to_string()),
            ],
            "list mode gives them back"
        );
    }

    #[test]
    fn paging_swaps_which_panes_the_engine_tails() {
        let rows: Vec<RosterRow> = (1..=10).map(|i| row(&format!("s{i:02}"))).collect();
        let mut app = grid(rows);
        app.take_commands();

        app.handle_key(key(KeyCode::Tab));
        let cmds = app.take_commands();
        for i in 1..=5 {
            assert!(
                cmds.contains(&UiCmd::ClosePane(format!("s{i:02}"))),
                "{cmds:?}"
            );
        }
        for i in 6..=10 {
            assert!(
                cmds.contains(&UiCmd::OpenPane(format!("s{i:02}"))),
                "{cmds:?}"
            );
        }
    }

    #[test]
    fn x_gives_up_a_slot_and_o_takes_it_back() {
        let mut app = grid(vec![row("a"), row("b")]);
        app.take_commands();

        app.handle_key(key(KeyCode::Char('x')));
        assert_eq!(app.take_commands(), vec![UiCmd::ClosePane("a".to_string())]);
        assert!(!screen(&app, 60, 16).contains("┌a"), "pane a still tiled");

        app.handle_key(key(KeyCode::Char('o')));
        assert_eq!(app.take_commands(), vec![UiCmd::OpenPane("a".to_string())]);
    }

    /// The engine evicting a finished pane to make room under `--max-panes`
    /// reads to the UI exactly like the user pressing `[x]`: the tile's
    /// slot frees up and its buffer goes with it, though the row stays on
    /// the roster.
    #[test]
    fn eviction_frees_the_grid_slot_like_a_manual_close() {
        let mut app = grid(vec![row("a"), row("b")]);
        app.apply_event(Event::PaneLines {
            key: "a".to_string(),
            lines: vec![pane_line("stale")],
        });

        app.apply_event(Event::Lifecycle {
            key: "a".to_string(),
            change: Lifecycle::Evicted,
        });

        assert!(!screen(&app, 60, 16).contains("┌a"), "pane a still tiled");
        assert!(
            !app.panes.contains_key("a"),
            "evicted pane's buffer lingered"
        );
    }

    /// A resurrection is the opposite of a dismissal: it undoes whatever
    /// `[x]` or an eviction did, without the user lifting a finger.
    #[test]
    fn resurrection_undoes_a_dismissal_and_drops_the_stale_buffer() {
        let mut app = grid(vec![row("a"), row("b")]);
        app.apply_event(Event::Lifecycle {
            key: "a".to_string(),
            change: Lifecycle::Evicted,
        });
        app.apply_event(Event::PaneLines {
            key: "a".to_string(),
            lines: vec![pane_line("stale")],
        });
        assert!(app.closed.contains("a"), "sanity: a was dismissed");
        assert!(
            app.panes.contains_key("a"),
            "sanity: buffer landed pre-resurrection"
        );

        app.apply_event(Event::Lifecycle {
            key: "a".to_string(),
            change: Lifecycle::Resumed,
        });

        assert!(!app.closed.contains("a"), "a stayed dismissed");
        assert!(!app.panes.contains_key("a"), "stale buffer was kept");
    }

    /// Moving the cursor past the end of a page turns to the next one, so
    /// zoom and the scroll keys always have a visible pane to act on.
    #[test]
    fn the_cursor_pages_the_grid_as_it_walks_off_the_bottom() {
        let rows: Vec<RosterRow> = (1..=10).map(|i| row(&format!("s{i:02}"))).collect();
        let mut app = grid(rows);
        for _ in 0..5 {
            app.handle_key(key(KeyCode::Char('j')));
        }
        assert_eq!(selected_key(&app), Some("s06"));
        assert_eq!(app.page, 1);

        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.page, 0);
    }

    #[test]
    fn grid_keys_do_nothing_in_list_mode() {
        let mut app = App::default();
        app.apply_event(Event::Roster(vec![row("a"), row("b")]));
        for code in [
            KeyCode::Tab,
            KeyCode::Enter,
            KeyCode::Char('z'),
            KeyCode::Char('x'),
            KeyCode::PageUp,
        ] {
            app.handle_key(key(code));
        }
        assert!(!app.zoom);
        assert_eq!(app.page, 0);
        assert_eq!(selected_key(&app), Some("a"));
        assert_eq!(app.take_commands(), vec![UiCmd::OpenPane("a".to_string())]);
    }

    #[test]
    fn an_empty_deck_still_draws_in_grid_mode() {
        let app = grid(Vec::new());
        let rendered = screen(&app, 60, 12);
        assert!(rendered.contains("no agent sessions found"), "{rendered}");
    }

    // ------------------------------------------------- sort/filter palette

    fn model_row(key: &str, model: &str) -> RosterRow {
        RosterRow {
            model: model.to_string(),
            ..row(key)
        }
    }

    fn fleet() -> Vec<RosterRow> {
        vec![
            model_row("C:aaaaaa", "claude-sonnet-5"),
            model_row("H:bbbbbb", "claude-opus-5"),
            model_row("O:cccccc", "gpt-6"),
        ]
    }

    #[test]
    fn s_opens_sort_focus_and_f_opens_filter_focus() {
        let mut app = App::default();
        app.handle_key(key(KeyCode::Char('s')));
        assert_eq!(app.palette.as_ref().unwrap().focus, PaletteFocus::Sort);
        app.handle_key(key(KeyCode::Esc));
        assert!(app.palette.is_none());

        app.handle_key(key(KeyCode::Char('f')));
        assert_eq!(app.palette.as_ref().unwrap().focus, PaletteFocus::Filter);
    }

    /// While the palette is open every key belongs to it — `q` must not
    /// quit and `j`/`k` must not move the roster cursor out from under it.
    #[test]
    fn the_open_palette_swallows_quit_and_navigation_keys() {
        let mut app = App::default();
        app.apply_event(Event::Roster(fleet()));
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('q')));
        app.handle_key(key(KeyCode::Char('j')));
        assert!(!app.quit);
        assert_eq!(selected_key(&app), Some("C:aaaaaa"));
        assert!(app.palette.is_some());
    }

    /// `[1]` then `[1]` again picks cost ascending, then flips to
    /// descending — the palette's sort focus routes straight to
    /// `ViewState::toggle_sort`.
    #[test]
    fn digit_then_same_digit_flips_sort_direction_through_the_app() {
        let mut app = App::default();
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('4'))); // [4] cost
        assert_eq!(app.view.sort_key, Some(SortKey::Cost));
        assert_eq!(app.view.sort_dir, crate::view::SortDir::Asc);
        app.handle_key(key(KeyCode::Char('4')));
        assert_eq!(app.view.sort_dir, crate::view::SortDir::Desc);
    }

    /// Typing a filter updates the live count, `[Enter]` commits it and
    /// closes the overlay, and it applies identically whether the fleet is
    /// shown as a list or a grid.
    #[test]
    fn typing_a_filter_updates_the_count_and_enter_applies_it() {
        let mut app = App::default();
        app.apply_event(Event::Roster(fleet()));
        app.handle_key(key(KeyCode::Char('f')));
        for c in "model=claude*".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        let (matched, total) =
            overlay::draft_matches(&app.roster, &app.view, &app.palette.as_ref().unwrap().input);
        assert_eq!((matched, total), (2, 3), "live count while still typing");

        app.handle_key(key(KeyCode::Enter));
        assert!(app.palette.is_none(), "enter closes the overlay");
        assert_eq!(app.view.filter.chips(), ["model=claude*"]);
        assert_eq!(
            app.visible_rows()
                .iter()
                .map(|r| r.key.as_str())
                .collect::<Vec<_>>(),
            ["C:aaaaaa", "H:bbbbbb"]
        );
    }

    /// `[Esc]` restores the sort and filter exactly as they were before the
    /// overlay opened, discarding whatever was picked or typed inside it.
    #[test]
    fn esc_restores_the_prior_sort_and_filter() {
        let mut app = App::default();
        app.apply_event(Event::Roster(fleet()));
        app.handle_key(key(KeyCode::Char('f')));
        for c in "model=claude*".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        let committed = app.view.clone();

        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('2'))); // [2] tool
        assert_ne!(
            app.view.sort_key, committed.sort_key,
            "sanity: chip applied"
        );
        app.handle_key(key(KeyCode::Esc));

        assert_eq!(app.view, committed);
        assert!(app.palette.is_none());
    }

    /// `[c]` in sort focus clears both the sort and the filter in one go.
    #[test]
    fn c_in_sort_focus_clears_sort_and_filter() {
        let mut app = App::default();
        app.apply_event(Event::Roster(fleet()));
        app.view.set_filter("model=claude*").unwrap();
        app.view.toggle_sort(SortKey::Cost);

        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('c')));
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.view.sort_key, None);
        assert!(app.view.filter.is_empty());
        assert_eq!(app.visible_rows().len(), 3, "clearing shows every session");
    }

    /// Outside the overlay, the header's own `[c]` clears immediately with
    /// no palette round trip.
    #[test]
    fn header_c_clears_without_opening_the_palette() {
        let mut app = App::default();
        app.view.toggle_sort(SortKey::Cost);
        app.handle_key(key(KeyCode::Char('c')));
        assert_eq!(app.view.sort_key, None);
        assert!(app.palette.is_none());
    }

    #[test]
    fn the_header_chip_bar_appears_only_once_a_sort_or_filter_is_active() {
        let mut app = App::default();
        app.apply_event(Event::Roster(fleet()));
        assert!(!screen(&app, 80, 20).contains("shown"), "no chips yet");

        app.view.toggle_sort(SortKey::Cost);
        let rendered = screen(&app, 80, 20);
        assert!(rendered.contains("sort: cost \u{2191}"), "{rendered}");
        assert!(rendered.contains("3/3 shown"), "{rendered}");
        assert!(rendered.contains("[s]ort [f]ilter [c]lear"), "{rendered}");
    }

    #[test]
    fn the_header_shows_a_green_chip_per_filter_term_and_the_live_count() {
        let mut app = App::default();
        app.apply_event(Event::Roster(fleet()));
        app.view.set_filter("model=claude*").unwrap();
        let rendered = screen(&app, 80, 20);
        assert!(rendered.contains("[model=claude*]"), "{rendered}");
        assert!(rendered.contains("2/3 shown"), "{rendered}");
    }

    /// List mode only shows the matching rows, and the cursor re-lands on
    /// one of them instead of tracking a now-hidden session.
    #[test]
    fn a_filter_hides_non_matching_rows_in_list_mode() {
        let mut app = App::default();
        app.apply_event(Event::Roster(fleet()));
        app.view.set_filter("model=claude*").unwrap();
        app.resync_selection();
        let rendered = screen(&app, 80, 20);
        assert!(rendered.contains("C:aaaaaa"), "{rendered}");
        assert!(rendered.contains("H:bbbbbb"), "{rendered}");
        assert!(!rendered.contains("O:cccccc"), "{rendered}");
    }

    /// Grid mode hands out slots from the same filtered set: only the
    /// matching sessions get tiled.
    #[test]
    fn a_filter_hides_non_matching_sessions_in_grid_mode() {
        let mut app = grid(fleet());
        app.view.set_filter("model=claude*").unwrap();
        let rendered = screen(&app, 80, 24);
        assert!(rendered.contains("C:aaaaaa"), "{rendered}");
        assert!(rendered.contains("H:bbbbbb"), "{rendered}");
        assert!(
            !rendered.contains("O:cccccc"),
            "grid still tiling a filtered-out session:\n{rendered}"
        );
    }

    /// The overlay itself: both sections visible, the active sort chip in
    /// its highlighted style, and the live match count for the typed
    /// filter.
    #[test]
    fn the_overlay_shows_both_sections_and_the_active_chip() {
        let mut app = App::default();
        app.apply_event(Event::Roster(fleet()));
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('4'))); // [4] cost, ascending
        let rendered = screen(&app, 80, 24);

        assert!(rendered.contains("sort / filter"), "{rendered}");
        for label in ["model", "tool", "in/out", "cost", "elapsed"] {
            assert!(
                rendered.contains(label),
                "chip {label} missing:\n{rendered}"
            );
        }
        assert!(rendered.contains("cost \u{2191}"), "{rendered}");
        assert!(rendered.contains("filter:"), "{rendered}");
        assert!(rendered.contains("matches: 3 of 3"), "{rendered}");
        assert!(rendered.contains("apply"), "{rendered}");
        assert!(rendered.contains("clear all"), "{rendered}");
        assert!(rendered.contains("cancel"), "{rendered}");

        let buf = buffer(&app, 80, 24);
        let overlay_rect = centered(buf.area, 62, 7);
        // The active chip's bracket is cyan (border_selected); an inactive
        // one is dim. "[1] model  [2] tool  [3] in/out  " is 33 columns, so
        // the [4] cost chip's own bracket starts right after that.
        let inactive_x = overlay_rect.x + 1; // "[1"
        let active_x = overlay_rect.x + 1 + 33; // "[4"
        let y = overlay_rect.y + 1; // first row inside the border
        assert_eq!(
            buf[(inactive_x, y)].fg,
            palette::style(Sem::Dim).fg.unwrap(),
            "inactive chip should not be highlighted"
        );
        assert_eq!(
            buf[(active_x, y)].fg,
            palette::border_selected().fg.unwrap(),
            "active chip's bracket should be cyan"
        );
    }

    /// A malformed filter shows the parse error instead of a stale count,
    /// and does not close the overlay or touch the committed filter.
    #[test]
    fn the_overlay_shows_a_parse_error_instead_of_a_count() {
        let mut app = App::default();
        app.handle_key(key(KeyCode::Char('f')));
        for c in "cost>abc".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert!(app.palette.is_some(), "a bad filter keeps the overlay open");
        let rendered = screen(&app, 80, 24);
        assert!(rendered.contains("not a number"), "{rendered}");
        assert!(app.view.filter.is_empty());
    }
}
