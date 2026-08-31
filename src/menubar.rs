//! `hermon menubar`: a persistent macOS status-bar item showing live fleet
//! counts, the glance target the notification story (banner → glance → act)
//! points at. A second in-process consumer of [`crate::engine::Engine`] —
//! the stores are read-only and multi-consumer-safe, so this is one more
//! thread talking to the same engine `watch` uses, not a daemon or IPC.
//!
//! Kept in its own module with a minimal, stable surface for the tickets
//! that build on it: [`format_title`] is the pure, testable roster→title
//! logic; the event-loop wiring that drives it lives in [`mac`] so #71's
//! dropdown menu and #72's arbitration have a narrow seam to extend.

use crate::roster::RosterRow;
use crate::source::{Attn, Liveness};
use crate::ui::palette;

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
pub use mac::run;

/// Non-macOS builds still expose the `menubar` subcommand (so `cargo build`
/// succeeds everywhere) but refuse to run it — no tray backend exists there
/// yet (Linux tray is future work).
#[cfg(not(target_os = "macos"))]
pub fn run(_config: crate::config::EngineConfig) -> anyhow::Result<()> {
    anyhow::bail!("hermon menubar: macOS only")
}

/// The status item's title: the live count always, `⏸ n` / `⚠ n` appended
/// only when nonzero — menu bar space is precious, so the common case (an
/// idle or all-live fleet) stays short.
pub fn format_title(rows: &[RosterRow]) -> String {
    let live = rows.iter().filter(|r| r.state == Liveness::Live).count();
    let perm_wait = attention_count(rows, Attn::PermWait);
    let stuck = attention_count(rows, Attn::Stuck);

    let mut title = format!("{} {live}", glyph(Liveness::Live));
    if perm_wait > 0 {
        title.push_str(&format!(
            " {} {perm_wait}",
            glyph(Liveness::Attention(Attn::PermWait))
        ));
    }
    if stuck > 0 {
        title.push_str(&format!(
            " {} {stuck}",
            glyph(Liveness::Attention(Attn::Stuck))
        ));
    }
    title
}

fn attention_count(rows: &[RosterRow], attn: Attn) -> usize {
    rows.iter()
        .filter(|r| r.state == Liveness::Attention(attn))
        .count()
}

fn glyph(liveness: Liveness) -> &'static str {
    palette::glyph_for_liveness(liveness).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: Liveness) -> RosterRow {
        RosterRow {
            id: "id".to_string(),
            key: "C:aaaaaa".to_string(),
            state,
            model: "m".to_string(),
            last_tool: "-".to_string(),
            last_line: String::new(),
            in_tok: 0,
            out_tok: 0,
            cost: Some(0.0),
            elapsed: None,
            last_ts: 0.0,
            title: String::new(),
            attn_elapsed: None,
        }
    }

    #[test]
    fn empty_roster_is_zero_live() {
        assert_eq!(format_title(&[]), format!("{} 0", glyph(Liveness::Live)));
    }

    #[test]
    fn counts_only_live_sessions_in_the_live_count() {
        let rows = vec![
            row(Liveness::Live),
            row(Liveness::Live),
            row(Liveness::Done),
            row(Liveness::Attention(Attn::PermWait)),
        ];
        assert_eq!(
            format_title(&rows),
            format!(
                "{} 2 {} 1",
                glyph(Liveness::Live),
                glyph(Liveness::Attention(Attn::PermWait))
            )
        );
    }

    #[test]
    fn perm_wait_is_omitted_when_zero() {
        let rows = vec![row(Liveness::Live), row(Liveness::Attention(Attn::Stuck))];
        let title = format_title(&rows);
        assert!(!title.contains(glyph(Liveness::Attention(Attn::PermWait))));
        assert_eq!(
            title,
            format!(
                "{} 1 {} 1",
                glyph(Liveness::Live),
                glyph(Liveness::Attention(Attn::Stuck))
            )
        );
    }

    #[test]
    fn stuck_is_omitted_when_zero() {
        let rows = vec![
            row(Liveness::Live),
            row(Liveness::Attention(Attn::PermWait)),
        ];
        let title = format_title(&rows);
        assert!(!title.contains(glyph(Liveness::Attention(Attn::Stuck))));
    }

    #[test]
    fn both_attention_kinds_append_in_order() {
        let rows = vec![
            row(Liveness::Live),
            row(Liveness::Attention(Attn::PermWait)),
            row(Liveness::Attention(Attn::Stuck)),
            row(Liveness::Attention(Attn::Stuck)),
        ];
        assert_eq!(
            format_title(&rows),
            format!(
                "{} 1 {} 1 {} 2",
                glyph(Liveness::Live),
                glyph(Liveness::Attention(Attn::PermWait)),
                glyph(Liveness::Attention(Attn::Stuck))
            )
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_run_reports_unsupported() {
        let config = crate::config::EngineConfig {
            claude_dir: String::new(),
            hermes_db: String::new(),
            opencode_db: String::new(),
            hermes_log: String::new(),
            idle_timeout: 180.0,
            replay: crate::source::Replay::DEFAULT,
            fresh_window: 300.0,
            interval: std::time::Duration::from_secs(1),
            linger: 60.0,
            max_panes: 8,
            notify: crate::notify::NotifyCfg::default(),
        };
        let err = run(config).unwrap_err();
        assert!(err.to_string().contains("macOS only"));
    }
}
