//! Command-line interface, ported from `hermon.py`'s argparse setup
//! (`add_source_flags` / `build_parser`).

use clap::{Args, Parser, Subcommand};

use crate::arbitration::NotifyFlag;
use crate::notify::NotifyCfg;
use crate::source::Replay;

/// Live terminal monitor deck for Hermes, Claude Code, and OpenCode sessions.
#[derive(Debug, Parser)]
#[command(name = "hermon", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the live monitor deck (a ratatui TUI).
    Watch(SourceArgs),
    /// Run the live monitor deck in a native desktop window.
    Gui(SourceArgs),
    /// Print the roster once to stdout (no TUI).
    Ls(LsArgs),
    /// Stream one session's transcript to stdout until Ctrl-C.
    Render(RenderArgs),
    /// Live fleet counts in the macOS menu bar (macOS only).
    Menubar(MenubarArgs),
    /// Stream session frames over stdio for a host `hermon` process to
    /// consume — the in-container half of the remote wire protocol (#88,
    /// #89). `--interval` (from [`SourceArgs`]) is the `Snap` cadence.
    Agent(SourceArgs),
}

/// Menubar-specific options, including source flags and login-item management.
#[derive(Debug, Args)]
pub struct MenubarArgs {
    #[command(flatten)]
    pub source: SourceArgs,

    /// Write a LaunchAgent plist to ~/.config/hermon/LaunchAgents/dev.hermon.menubar.plist
    /// and register it with launchctl so `hermon menubar` starts at login.
    #[arg(long)]
    pub install_login_item: bool,

    /// Unregister and delete the LaunchAgent plist, removing launch-at-login.
    #[arg(long)]
    pub uninstall_login_item: bool,
}

/// `hermon render C:0f865f`: one session's pane body on stdout, the parity
/// harness the per-source renderers are diffed against
/// (`hermon.py:1443`, which takes a file or `--hermes`/`--opencode` id
/// where this takes the roster key `hermon ls` already prints).
#[derive(Debug, Args)]
pub struct RenderArgs {
    /// Roster key of the session to tail, e.g. `C:0f865f` (see `hermon ls`).
    pub key: String,

    #[command(flatten)]
    pub source: SourceArgs,

    /// Look this many seconds back for the session named by KEY.
    #[arg(long, default_value_t = 3600.0)]
    pub fresh_window: f64,
}

/// `hermon ls`: the source flags plus its own, wider lookback
/// (`hermon.py:1463`, where `ls` defaults `--fresh-window` to an hour while
/// `watch` keeps 300s).
#[derive(Debug, Args)]
pub struct LsArgs {
    #[command(flatten)]
    pub source: SourceArgs,

    /// Include sessions active within this many seconds.
    #[arg(long, default_value_t = 3600.0)]
    pub fresh_window: f64,
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

    /// Hermes agent.log (roster API-call ticker).
    #[arg(long, default_value_t = default_hermes_log())]
    pub hermes_log: String,

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

    /// Skip desktop notifications entirely.
    #[arg(long)]
    pub no_notify: bool,

    /// Notify from this process even if a running menubar already is.
    #[arg(long, conflicts_with = "no_notify")]
    pub notify: bool,

    /// Per-(session, kind) cooldown before a turn-done/error alert can fire
    /// again for the same session.
    #[arg(long, default_value_t = NotifyCfg::default().cooldown_secs)]
    pub notify_cooldown: f64,

    /// Don't alert when a session finishes a clean turn.
    #[arg(long)]
    pub no_notify_turn_done: bool,

    /// Don't alert when a tool call looks wedged.
    #[arg(long)]
    pub no_notify_stuck: bool,

    /// Don't alert when a session is waiting on a permission prompt.
    #[arg(long)]
    pub no_notify_perm_wait: bool,

    /// Don't alert on an observed error line.
    #[arg(long)]
    pub no_notify_error: bool,

    /// Bytes of history a freshly opened pane replays from a file-backed
    /// source (Claude transcripts); ignored by DB-backed sources.
    #[arg(long, default_value_t = Replay::DEFAULT.bytes)]
    pub replay_bytes: u64,

    /// Rows of history a freshly opened pane replays from a DB-backed
    /// source (Hermes, OpenCode); ignored by file-backed sources.
    #[arg(long, default_value_t = Replay::DEFAULT.rows)]
    pub replay_lines: u32,
}

impl SourceArgs {
    /// Builds [`NotifyCfg`] from the `--no-notify*`/`--notify-cooldown`
    /// flags. `--no-notify` is the master switch: it wins over any per-kind
    /// flag rather than combining with it, so `--no-notify
    /// --no-notify-error` isn't a contradiction to reason about.
    pub fn notify_cfg(&self) -> NotifyCfg {
        let enabled = !self.no_notify;
        NotifyCfg {
            turn_done: enabled && !self.no_notify_turn_done,
            error: enabled && !self.no_notify_error,
            stuck: enabled && !self.no_notify_stuck,
            perm_wait: enabled && !self.no_notify_perm_wait,
            cooldown_secs: self.notify_cooldown,
            ..NotifyCfg::default()
        }
    }

    /// Whether the user said anything about notifications at all —
    /// [`crate::arbitration`] only decides the `Default` case.
    pub fn notify_flag(&self) -> NotifyFlag {
        match (self.notify, self.no_notify) {
            (true, _) => NotifyFlag::ForceOn,
            (_, true) => NotifyFlag::ForceOff,
            _ => NotifyFlag::Default,
        }
    }
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

fn default_hermes_log() -> String {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".hermes")
        .join("logs")
        .join("agent.log")
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
