//! Runtime configuration derived from CLI flags.

use std::time::Duration;

/// Everything [`crate::engine::Engine`] needs to scan sources and pace its
/// loop: the same store locations `Sources::new` takes, plus the watcher
/// tuning knobs (`hermon.py:1294 cmd_watch`).
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub claude_dir: String,
    pub hermes_db: String,
    pub opencode_db: String,
    /// Safety ceiling for a session stuck mid-turn with no activity.
    pub idle_timeout: f64,
    /// How long a finished session stays on the roster before aging out.
    pub fresh_window: f64,
    /// How often the engine rescans every source.
    pub interval: Duration,
}
