//! hermon-rs: live tmux monitor deck for Hermes and Claude Code sessions.
//!
//! Greenfield rewrite of `hermon.py`, developed in the same repository so
//! issues and PRs stay connected to the Python implementation it replaces.

pub mod cli;
pub mod config;
pub mod engine;
pub mod notify;
pub mod render;
pub mod roster;
pub mod source;
pub mod ui;
pub mod view;

use std::io::IsTerminal;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::bail;
use clap::Parser;

use cli::{Cli, Command, LsArgs};
use roster::{Sources, TICKER_LIMIT, api_call_ticker, build_roster, roster_lines};

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Command::Watch(_args) => bail!("hermon watch: not yet implemented"),
        Command::Ls(args) => {
            ls(&args);
            Ok(())
        }
    }
}

/// Print the roster once. Never fails: every source degrades to "no
/// sessions" on a missing or unreadable store, so an empty deck prints an
/// empty roster rather than an error (`hermon.py:1103 cmd_summary`, once).
fn ls(args: &LsArgs) {
    let src = &args.source;
    let mut sources = Sources::new(&src.claude_dir, &src.hermes_db, &src.opencode_db);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());

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
