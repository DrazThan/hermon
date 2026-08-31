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

use crate::roster::{RosterRow, fmt_cost};
use crate::source::{Attn, Liveness};
use crate::ui::palette;

#[cfg(target_os = "macos")]
mod mac;

/// One row in the dropdown menu — a session display or an action button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuRow {
    /// Fleet summary: "7 live · 3 done · Σ $8.87"
    Summary {
        live: usize,
        done: usize,
        cost: String,
    },
    /// A session row: glyph, key, model, tool, elapsed, and cost.
    Session {
        key: String,
        model: String,
        tool: String,
        elapsed: String,
        cost: String,
        glyph: &'static str,
    },
    /// "… N more sessions"
    More { count: usize },
    /// Separator before actions
    Separator,
    /// "Open hermon watch"
    OpenWatch,
    /// "Quit"
    Quit,
}

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

/// Format elapsed time for menu display: `12m`, `1h 23m`, etc.
fn fmt_elapsed(seconds: Option<f64>) -> String {
    match seconds {
        None => String::new(),
        Some(secs) => {
            let secs = secs.round() as u64;
            if secs < 60 {
                format!("{secs}s")
            } else if secs < 3600 {
                format!("{}m", secs / 60)
            } else {
                let h = secs / 3600;
                let m = (secs % 3600) / 60;
                if m == 0 {
                    format!("{h}h")
                } else {
                    format!("{h}h {m}m")
                }
            }
        }
    }
}

/// Build menu rows from the roster: attention-first sorted, capped at ~20,
/// with fleet summary at top and Open/Quit actions at bottom. This is a pure
/// function for testability — the caller wires it into the menu API.
pub fn build_menu(rows: &[RosterRow]) -> Vec<MenuRow> {
    let mut result = Vec::new();

    // Count states for the summary.
    let live_count = rows.iter().filter(|r| r.state == Liveness::Live).count();
    let done_count = rows.iter().filter(|r| r.state == Liveness::Done).count();
    let total_cost: f64 = rows.iter().filter_map(|r| r.cost).sum();

    // Summary row.
    result.push(MenuRow::Summary {
        live: live_count,
        done: done_count,
        cost: fmt_summary_cost(total_cost),
    });

    // Sort rows: attention first (⏸/⚠), then live, then done.
    let mut sorted = rows.to_vec();
    sorted.sort_by_key(|a| attn_rank(a.state));

    // Cap at 20 rows; if more, add a "… N more" indicator.
    if sorted.len() > 20 {
        let cap = &sorted[..20];
        result.extend(cap.iter().map(row_to_menu));
        result.push(MenuRow::More {
            count: sorted.len() - 20,
        });
    } else {
        result.extend(sorted.iter().map(row_to_menu));
    }

    // Action items.
    result.push(MenuRow::Separator);
    result.push(MenuRow::OpenWatch);
    result.push(MenuRow::Quit);

    result
}

/// Convert a RosterRow to a MenuRow session item.
fn row_to_menu(row: &RosterRow) -> MenuRow {
    MenuRow::Session {
        key: row.key.clone(),
        model: row.model.clone(),
        tool: row.last_tool.clone(),
        elapsed: fmt_elapsed(row.elapsed),
        cost: fmt_cost(row.cost),
        glyph: glyph(row.state),
    }
}

/// Format cost for the summary line: `—` if no data, else `$X.XX` (two decimals for brevity).
fn fmt_summary_cost(total: f64) -> String {
    if total == 0.0 {
        "—".to_string()
    } else {
        format!("${:.2}", total)
    }
}

/// Attention rank for sorting: ⏸/⚠ first, then live, then done.
fn attn_rank(state: Liveness) -> u8 {
    match state {
        Liveness::Attention(_) => 0,
        Liveness::Live => 1,
        Liveness::Done => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, state: Liveness, cost: Option<f64>, elapsed: Option<f64>) -> RosterRow {
        RosterRow {
            id: format!("id-{key}"),
            key: key.to_string(),
            state,
            model: "sonnet".to_string(),
            last_tool: "Bash".to_string(),
            last_line: String::new(),
            in_tok: 0,
            out_tok: 0,
            cost,
            elapsed,
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
            row("a", Liveness::Live, None, None),
            row("b", Liveness::Live, None, None),
            row("c", Liveness::Done, None, None),
            row("d", Liveness::Attention(Attn::PermWait), None, None),
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
        let rows = vec![
            row("a", Liveness::Live, None, None),
            row("b", Liveness::Attention(Attn::Stuck), None, None),
        ];
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
            row("a", Liveness::Live, None, None),
            row("b", Liveness::Attention(Attn::PermWait), None, None),
        ];
        let title = format_title(&rows);
        assert!(!title.contains(glyph(Liveness::Attention(Attn::Stuck))));
    }

    #[test]
    fn both_attention_kinds_append_in_order() {
        let rows = vec![
            row("a", Liveness::Live, None, None),
            row("b", Liveness::Attention(Attn::PermWait), None, None),
            row("c", Liveness::Attention(Attn::Stuck), None, None),
            row("d", Liveness::Attention(Attn::Stuck), None, None),
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

    // -------------------------------------------------------- menu building

    #[test]
    fn build_menu_empty_roster() {
        let menu = build_menu(&[]);
        assert_eq!(menu.len(), 4); // summary, separator, open watch, quit
        assert!(matches!(
            menu[0],
            MenuRow::Summary {
                live: 0,
                done: 0,
                ..
            }
        ));
        assert!(matches!(menu[1], MenuRow::Separator));
        assert!(matches!(menu[2], MenuRow::OpenWatch));
        assert!(matches!(menu[3], MenuRow::Quit));
    }

    #[test]
    fn build_menu_single_row() {
        let rows = vec![row("C:abc", Liveness::Live, Some(1.5), Some(120.0))];
        let menu = build_menu(&rows);
        assert_eq!(menu.len(), 5); // summary, session, separator, open watch, quit
        assert!(matches!(
            menu[0],
            MenuRow::Summary {
                live: 1,
                done: 0,
                ..
            }
        ));
        assert!(matches!(menu[1], MenuRow::Session { .. }));
    }

    #[test]
    fn build_menu_attention_first() {
        let rows = vec![
            row("live", Liveness::Live, Some(1.0), Some(60.0)),
            row("done", Liveness::Done, Some(0.5), None),
            row(
                "stuck",
                Liveness::Attention(Attn::Stuck),
                Some(2.0),
                Some(180.0),
            ),
            row(
                "perm",
                Liveness::Attention(Attn::PermWait),
                Some(1.5),
                Some(300.0),
            ),
        ];
        let menu = build_menu(&rows);
        // Summary + stuck + perm + live + done + separator + open + quit
        let session_rows: Vec<_> = menu
            .iter()
            .filter_map(|m| match m {
                MenuRow::Session { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect();
        // Attention rows (stuck and perm, in input order) then live, then done.
        assert_eq!(session_rows, vec!["stuck", "perm", "live", "done"]);
    }

    #[test]
    fn build_menu_cost_in_summary() {
        let rows = vec![
            row("a", Liveness::Live, Some(1.5), None),
            row("b", Liveness::Live, Some(2.5), None),
            row("c", Liveness::Done, Some(0.1), None),
        ];
        let menu = build_menu(&rows);
        if let MenuRow::Summary { cost, .. } = &menu[0] {
            // Total is 4.1, formatted as $4.10
            assert_eq!(cost, "$4.10");
        } else {
            panic!("expected summary");
        }
    }

    #[test]
    fn build_menu_unknown_cost_in_summary() {
        let rows = vec![
            row("a", Liveness::Live, None, None),
            row("b", Liveness::Live, Some(1.0), None),
        ];
        let menu = build_menu(&rows);
        if let MenuRow::Summary { cost, .. } = &menu[0] {
            // Only known costs count: 1.0
            assert_eq!(cost, "$1.00");
        } else {
            panic!("expected summary");
        }
    }

    #[test]
    fn build_menu_caps_at_20_rows() {
        let rows: Vec<_> = (0..30)
            .map(|i| row(&format!("r{i}"), Liveness::Live, Some(0.1), Some(60.0)))
            .collect();
        let menu = build_menu(&rows);
        let session_count = menu
            .iter()
            .filter(|m| matches!(m, MenuRow::Session { .. }))
            .count();
        assert_eq!(session_count, 20);
        assert!(
            menu.iter()
                .any(|m| matches!(m, MenuRow::More { count: 10 }))
        );
    }

    #[test]
    fn build_menu_no_more_when_under_cap() {
        let rows: Vec<_> = (0..15)
            .map(|i| row(&format!("r{i}"), Liveness::Live, Some(0.1), Some(60.0)))
            .collect();
        let menu = build_menu(&rows);
        assert!(!menu.iter().any(|m| matches!(m, MenuRow::More { .. })));
    }

    #[test]
    fn build_menu_actions_at_end() {
        let rows = vec![row("a", Liveness::Live, None, None)];
        let menu = build_menu(&rows);
        let last_three = &menu[menu.len() - 3..];
        assert!(matches!(last_three[0], MenuRow::Separator));
        assert!(matches!(last_three[1], MenuRow::OpenWatch));
        assert!(matches!(last_three[2], MenuRow::Quit));
    }

    #[test]
    fn fmt_elapsed_formats_durations() {
        assert_eq!(fmt_elapsed(Some(30.0)), "30s");
        assert_eq!(fmt_elapsed(Some(120.0)), "2m");
        assert_eq!(fmt_elapsed(Some(3600.0)), "1h");
        assert_eq!(fmt_elapsed(Some(3720.0)), "1h 2m");
        assert_eq!(fmt_elapsed(Some(7200.0)), "2h");
        assert_eq!(fmt_elapsed(None), "");
    }

    #[test]
    fn fmt_summary_cost_formats_totals() {
        assert_eq!(fmt_summary_cost(0.0), "—");
        assert_eq!(fmt_summary_cost(1.234), "$1.23");
        assert_eq!(fmt_summary_cost(10.0), "$10.00");
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
