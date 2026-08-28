//! Command-line interface, ported from `hermon.py`'s argparse setup
//! (`add_source_flags` / `build_parser`).

use clap::{Args, Parser, Subcommand};

/// Live tmux monitor deck for Hermes and Claude Code sessions.
#[derive(Debug, Parser)]
#[command(name = "hermon", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the watcher daemon (owns the tmux session).
    Watch(SourceArgs),
    /// Print the roster once to stdout (no tmux).
    Ls(SourceArgs),
}

/// Flags shared by every subcommand, ported from `hermon.py:1404 add_source_flags`
/// plus the watcher tuning knobs (`--idle-timeout`, `--interval`, `--max-panes`,
/// `--linger`). Defaults match the Python implementation's.
#[derive(Debug, Args)]
pub struct SourceArgs {
    /// Claude Code transcript root.
    #[arg(long, default_value_t = default_claude_dir())]
    pub claude_dir: String,

    /// Hermes state.db path.
    #[arg(long, default_value_t = default_hermes_db())]
    pub hermes_db: String,

    /// OpenCode opencode.db path.
    #[arg(long, default_value_t = default_opencode_db())]
    pub opencode_db: String,

    /// Safety ceiling for a session stuck mid-turn with no activity.
    #[arg(long, default_value_t = 180.0)]
    pub idle_timeout: f64,

    /// Scan interval, in seconds.
    #[arg(long, default_value_t = 1.0)]
    pub interval: f64,

    /// Max session panes (finished panes evicted first).
    #[arg(long, default_value_t = 8)]
    pub max_panes: usize,

    /// Keep finished panes this long before unsplitting; 0 = forever.
    #[arg(long, default_value_t = 60.0)]
    pub linger: f64,
}

fn default_claude_dir() -> String {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".claude")
        .join("projects")
        .display()
        .to_string()
}

fn default_hermes_db() -> String {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".hermes")
        .join("state.db")
        .display()
        .to_string()
}

fn default_opencode_db() -> String {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db")
        .display()
        .to_string()
}
