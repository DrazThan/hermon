//! Runtime configuration derived from CLI flags.

use std::time::Duration;

use crate::notify::NotifyCfg;
use crate::source::Replay;

/// Everything [`crate::engine::Engine`] needs to scan sources and pace its
/// loop: the same store locations `Sources::new` takes, plus the watcher
/// tuning knobs (`hermon.py:1294 cmd_watch`).
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub claude_dir: String,
    pub hermes_db: String,
    pub opencode_db: String,
    /// Hermes `agent.log`, scanned each tick for the API-call ticker.
    pub hermes_log: String,
    /// Safety ceiling for a session stuck mid-turn with no activity.
    pub idle_timeout: f64,
    /// History replayed when a pane is freshly opened (`--replay-bytes` /
    /// `--replay-lines`).
    pub replay: Replay,
    /// How long a finished session stays on the roster before aging out.
    pub fresh_window: f64,
    /// How often the engine rescans every source.
    pub interval: Duration,
    /// How long a finished session keeps its pane open before the engine
    /// closes the tailer and frees the slot; `0` keeps it open forever
    /// (`hermon.py:1294 cmd_watch`'s `args.linger`).
    pub linger: f64,
    /// Most sessions tailed at once; a new live one evicts the oldest
    /// finished pane to make room (`hermon.py:1389 self_evict`).
    pub max_panes: usize,
    /// Which alert kinds are enabled and their cooldowns, from `--no-notify*`
    /// / `--notify-cooldown` (#44).
    pub notify: NotifyCfg,
}
