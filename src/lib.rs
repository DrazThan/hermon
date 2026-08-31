//! hermon-rs: live terminal monitor deck for Hermes, Claude Code, and OpenCode sessions.
//!
//! Greenfield rewrite of `hermon.py`, developed in the same repository so
//! issues and PRs stay connected to the Python implementation it replaces.

pub mod arbitration;
pub mod cli;
pub mod config;
pub mod engine;
pub mod gui;
pub mod menubar;
pub mod notify;
pub mod render;
pub mod roster;
pub mod source;
pub mod ui;
pub mod view;

use std::io::IsTerminal;
use std::path::Path;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use clap::Parser;

use arbitration::UiKind;
use cli::{Cli, Command, LsArgs};
use config::EngineConfig;
use engine::PANE_TICK;
use notify::NotifyCfg;
use roster::{Sources, TICKER_LIMIT, api_call_ticker, build_roster, resolve_key, roster_lines};
use source::Replay;

/// The notify config this process actually runs with: what the flags asked
/// for, minus the notifier role if a higher-precedence hermon UI already
/// holds it (#72). Losing the role is worth a word on stderr — silence would
/// just read as broken notifications.
fn arbitrated_notify_cfg(args: &cli::SourceArgs, kind: UiKind) -> NotifyCfg {
    let cfg = args.notify_cfg();
    let dir = arbitration::runtime_dir();
    let decision = arbitration::decide(dir.as_deref(), kind, args.notify_flag());
    if decision.notify {
        return cfg;
    }
    if let Some(owner) = decision.yielded_to {
        eprintln!("hermon: {owner} instance is notifying; pass --notify to override");
    }
    cfg.silenced()
}

fn replay_from(src: &cli::SourceArgs) -> Replay {
    Replay {
        bytes: src.replay_bytes,
        rows: src.replay_lines,
    }
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Command::Watch(args) => {
            // `watch` keeps the Python default 300s fresh window
            // (`hermon.py:1463`); only `ls` widens it to an hour.
            let notify = arbitrated_notify_cfg(&args, UiKind::Watch);
            let replay = replay_from(&args);
            let config = EngineConfig {
                claude_dir: args.claude_dir,
                hermes_db: args.hermes_db,
                opencode_db: args.opencode_db,
                hermes_log: args.hermes_log,
                idle_timeout: args.idle_timeout,
                fresh_window: 300.0,
                interval: Duration::from_secs_f64(args.interval),
                linger: args.linger,
                max_panes: args.max_panes,
                notify,
                replay,
            };
            ui::run_tui(config)
        }
        // The window is a second engine consumer, not a second engine
        // configuration: same flags and same 300s fresh window as `watch`.
        // Arbitrated the same way `watch` is (#72/#77): a running menubar
        // outranks it, so a `gui` alongside one silences its own banners.
        Command::Gui(args) => {
            let notify = arbitrated_notify_cfg(&args, UiKind::Gui);
            let replay = replay_from(&args);
            let config = EngineConfig {
                claude_dir: args.claude_dir,
                hermes_db: args.hermes_db,
                opencode_db: args.opencode_db,
                hermes_log: args.hermes_log,
                idle_timeout: args.idle_timeout,
                fresh_window: 300.0,
                interval: Duration::from_secs_f64(args.interval),
                linger: args.linger,
                max_panes: args.max_panes,
                notify,
                replay,
            };
            gui::run_gui(config)
        }
        Command::Ls(args) => {
            ls(&args);
            Ok(())
        }
        Command::Render(args) => render(&args),
        Command::Menubar(args) => {
            // Same fresh window as `watch` (`hermon.py:1463`); menubar is
            // the same live-fleet view, just in the status bar. Nothing
            // outranks it, so this only ever honours an explicit flag — but
            // it goes through the same seam so #77's `gui` slots in as one
            // more arm.
            let notify = arbitrated_notify_cfg(&args, UiKind::Menubar);
            let replay = replay_from(&args);
            let config = EngineConfig {
                claude_dir: args.claude_dir,
                hermes_db: args.hermes_db,
                opencode_db: args.opencode_db,
                hermes_log: args.hermes_log,
                idle_timeout: args.idle_timeout,
                fresh_window: 300.0,
                interval: Duration::from_secs_f64(args.interval),
                linger: args.linger,
                max_panes: args.max_panes,
                notify,
                replay,
            };
            menubar::run(config)
        }
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Tail one session to stdout until Ctrl-C, which kills the process the
/// ordinary way — nothing here holds the terminal (`hermon.py:1443`).
///
/// The roster is built once, only to turn the key into a source and a
/// session id; from there the tailer is the whole loop.
fn render(args: &cli::RenderArgs) -> anyhow::Result<()> {
    let src = &args.source;
    let mut sources = Sources::new(&src.claude_dir, &src.hermes_db, &src.opencode_db);
    let now = now_secs();

    let rows = build_roster(&mut sources, now, args.fresh_window, src.idle_timeout);
    let row = resolve_key(&rows, &args.key)?;
    let mut tailer = sources
        .open_tailer(&row.key, &row.id, replay_from(src))
        .ok_or_else(|| anyhow!("{}: this source cannot tail sessions yet", row.key))?;

    // Same rule as `hermon.py:67 USE_COLOR`.
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    loop {
        for line in tailer.poll() {
            println!(
                "{}",
                if color {
                    line.to_ansi()
                } else {
                    line.to_plain()
                }
            );
        }
        thread::sleep(PANE_TICK);
    }
}

/// Print the roster once. Never fails: every source degrades to "no
/// sessions" on a missing or unreadable store, so an empty deck prints an
/// empty roster rather than an error (`hermon.py:1103 cmd_summary`, once).
fn ls(args: &LsArgs) {
    let src = &args.source;
    let mut sources = Sources::new(&src.claude_dir, &src.hermes_db, &src.opencode_db);
    let now = now_secs();

    let rows = build_roster(&mut sources, now, args.fresh_window, src.idle_timeout);
    let ticker = api_call_ticker(Path::new(&src.hermes_log), TICKER_LIMIT);

    // Same rule as `hermon.py:67 USE_COLOR`.
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    for line in roster_lines(&rows, &ticker, now) {
        println!(
            "{}",
            if color {
                line.to_ansi()
            } else {
                line.to_plain()
            }
        );
    }
}
