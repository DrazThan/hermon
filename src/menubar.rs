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

use crate::render::{Rgb, Sem};
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
    /// "Mute notifications", a checkmark item bound to the same mute the
    /// TUI's `[m]` flips ([`crate::notify::AlertHistory::set_muted`]). The
    /// mute is per-process: muting here does not mute a running `watch`.
    MuteToggle { muted: bool },
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

/// The status item's full title: [`format_title`] plus the mute glyph
/// (`🔕`, or `[muted]` under `HERMON_ASCII`) while banners are silenced, so
/// the menu bar says so without the dropdown having to be opened.
pub fn status_title(rows: &[RosterRow], muted: bool) -> String {
    let title = format_title(rows);
    match palette::mute_indicator(muted) {
        "" => title,
        glyph => format!("{title} {glyph}"),
    }
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
pub fn build_menu(rows: &[RosterRow], muted: bool) -> Vec<MenuRow> {
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
    result.push(MenuRow::MuteToggle { muted });
    result.push(MenuRow::OpenWatch);
    result.push(MenuRow::Quit);

    result
}

/// The text one [`MenuRow`] shows in the dropdown. Pure and portable, so the
/// macOS wiring only ever maps strings onto menu items — and so the labels
/// are testable on any platform.
pub fn menu_label(row: &MenuRow) -> String {
    match row {
        MenuRow::Summary { live, done, cost } => {
            format!("{live} live · {done} done · Σ {cost}")
        }
        MenuRow::Session {
            key,
            model,
            tool,
            elapsed,
            cost,
            glyph,
        } => {
            let mut parts = vec![format!("{glyph} {key}")];
            parts.extend(
                [model, tool, elapsed, cost]
                    .into_iter()
                    .filter(|f| !f.is_empty())
                    .cloned(),
            );
            parts.join(" · ")
        }
        MenuRow::More { count } => format!("… {count} more sessions"),
        MenuRow::Separator => String::new(),
        MenuRow::MuteToggle { .. } => "Mute notifications".to_string(),
        MenuRow::OpenWatch => "Open hermon watch".to_string(),
        MenuRow::Quit => "Quit".to_string(),
    }
}

// ------------------------------------------------------------- status icon

/// Pixel size of the status-bar dot. The tray backend scales whatever it is
/// given down to 18pt, so this is drawn at 2× for retina.
pub const ICON_PX: u32 = 36;

/// Alpha the dot is drawn at while muted — still legible, visibly dimmed.
const MUTED_ALPHA: u8 = 90;

/// What the dot is currently saying. Tracked by the tray loop so the icon is
/// only re-encoded when it would actually change, not on every roster tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconState {
    /// The most actionable attention state on the deck, if any.
    pub attn: Option<Attn>,
    pub muted: bool,
}

/// The icon's state from the same [`Liveness`] counts [`format_title`]
/// reads. [`Attn::PermWait`] outranks [`Attn::Stuck`]: a session waiting on a
/// permission prompt is blocked on the user specifically, where a wedged tool
/// may still come back on its own.
pub fn icon_state(rows: &[RosterRow], muted: bool) -> IconState {
    let attn = if attention_count(rows, Attn::PermWait) > 0 {
        Some(Attn::PermWait)
    } else if attention_count(rows, Attn::Stuck) > 0 {
        Some(Attn::Stuck)
    } else {
        None
    };
    IconState { attn, muted }
}

/// The dot as raw RGBA, so the menu bar needs no icon asset on disk: green
/// while the fleet is fine, amber/red the moment something wants a human,
/// dimmed while muted. Redundant with the counts in the title, which is the
/// point — color is what peripheral vision catches.
pub fn icon_rgba(state: IconState) -> Vec<u8> {
    let color = match state.attn {
        Some(Attn::PermWait) => Sem::User.color(),
        Some(Attn::Stuck) => Sem::Error.color(),
        None => Sem::Ok.color(),
    };
    let alpha = if state.muted { MUTED_ALPHA } else { 255 };
    dot(color, alpha)
}

/// A filled circle with a one-pixel feathered edge — without the feather it
/// reads as a square once the menu bar scales it down.
fn dot(color: Rgb, alpha: u8) -> Vec<u8> {
    let center = f64::from(ICON_PX - 1) / 2.0;
    let radius = f64::from(ICON_PX) / 2.0 - 1.0;
    let mut px = Vec::with_capacity((ICON_PX * ICON_PX * 4) as usize);
    for y in 0..ICON_PX {
        for x in 0..ICON_PX {
            let dx = f64::from(x) - center;
            let dy = f64::from(y) - center;
            let coverage = (radius - dx.hypot(dy)).clamp(0.0, 1.0);
            let a = (f64::from(alpha) * coverage).round() as u8;
            px.extend_from_slice(&[color.r, color.g, color.b, a]);
        }
    }
    px
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
        let menu = build_menu(&[], false);
        assert_eq!(menu.len(), 5); // summary, separator, mute, open watch, quit
        assert!(matches!(
            menu[0],
            MenuRow::Summary {
                live: 0,
                done: 0,
                ..
            }
        ));
        assert!(matches!(menu[1], MenuRow::Separator));
        assert!(matches!(menu[2], MenuRow::MuteToggle { muted: false }));
        assert!(matches!(menu[3], MenuRow::OpenWatch));
        assert!(matches!(menu[4], MenuRow::Quit));
    }

    #[test]
    fn build_menu_single_row() {
        let rows = vec![row("C:abc", Liveness::Live, Some(1.5), Some(120.0))];
        let menu = build_menu(&rows, false);
        assert_eq!(menu.len(), 6); // summary, session, separator, mute, open watch, quit
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
        let menu = build_menu(&rows, false);
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
        let menu = build_menu(&rows, false);
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
        let menu = build_menu(&rows, false);
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
        let menu = build_menu(&rows, false);
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
        let menu = build_menu(&rows, false);
        assert!(!menu.iter().any(|m| matches!(m, MenuRow::More { .. })));
    }

    #[test]
    fn build_menu_actions_at_end() {
        let rows = vec![row("a", Liveness::Live, None, None)];
        let menu = build_menu(&rows, false);
        let tail = &menu[menu.len() - 4..];
        assert!(matches!(tail[0], MenuRow::Separator));
        assert!(matches!(tail[1], MenuRow::MuteToggle { .. }));
        assert!(matches!(tail[2], MenuRow::OpenWatch));
        assert!(matches!(tail[3], MenuRow::Quit));
    }

    #[test]
    fn build_menu_mute_toggle_carries_the_current_state() {
        let menu = build_menu(&[], true);
        assert!(
            menu.iter()
                .any(|m| matches!(m, MenuRow::MuteToggle { muted: true }))
        );
    }

    // ------------------------------------------------------------ mute + icon

    #[test]
    fn status_title_appends_the_mute_glyph_only_when_muted() {
        let rows = vec![row("a", Liveness::Live, None, None)];
        assert_eq!(status_title(&rows, false), format_title(&rows));
        let muted = status_title(&rows, true);
        assert!(muted.starts_with(&format_title(&rows)));
        assert!(muted.ends_with(palette::mute_indicator(true)));
    }

    #[test]
    fn icon_state_is_calm_without_attention() {
        let rows = vec![
            row("a", Liveness::Live, None, None),
            row("b", Liveness::Done, None, None),
        ];
        assert_eq!(
            icon_state(&rows, false),
            IconState {
                attn: None,
                muted: false
            }
        );
    }

    #[test]
    fn icon_state_flips_on_any_attention_row() {
        let stuck = vec![row("a", Liveness::Attention(Attn::Stuck), None, None)];
        assert_eq!(icon_state(&stuck, false).attn, Some(Attn::Stuck));

        // A permission prompt blocks on the user specifically, so it wins
        // over a merely wedged tool.
        let both = vec![
            row("a", Liveness::Attention(Attn::Stuck), None, None),
            row("b", Liveness::Attention(Attn::PermWait), None, None),
        ];
        assert_eq!(icon_state(&both, false).attn, Some(Attn::PermWait));
    }

    #[test]
    fn icon_state_carries_mute() {
        assert!(icon_state(&[], true).muted);
        assert!(!icon_state(&[], false).muted);
    }

    /// The center pixel is the dot's fill; the corner is outside the circle.
    fn pixel(px: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * ICON_PX + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }

    #[test]
    fn icon_is_a_full_rgba_buffer_with_a_transparent_corner() {
        let px = icon_rgba(icon_state(&[], false));
        assert_eq!(px.len(), (ICON_PX * ICON_PX * 4) as usize);
        assert_eq!(pixel(&px, 0, 0)[3], 0, "corner is outside the dot");
        assert_eq!(pixel(&px, ICON_PX / 2, ICON_PX / 2)[3], 255);
    }

    #[test]
    fn icon_color_tracks_the_attention_state() {
        let calm = icon_rgba(IconState {
            attn: None,
            muted: false,
        });
        let stuck = icon_rgba(IconState {
            attn: Some(Attn::Stuck),
            muted: false,
        });
        let perm = icon_rgba(IconState {
            attn: Some(Attn::PermWait),
            muted: false,
        });
        let mid = ICON_PX / 2;
        assert_ne!(pixel(&calm, mid, mid), pixel(&stuck, mid, mid));
        assert_ne!(pixel(&stuck, mid, mid), pixel(&perm, mid, mid));
    }

    #[test]
    fn muting_dims_the_icon_without_changing_its_color() {
        let state = IconState {
            attn: Some(Attn::Stuck),
            muted: false,
        };
        let lit = icon_rgba(state);
        let dim = icon_rgba(IconState {
            muted: true,
            ..state
        });
        let mid = ICON_PX / 2;
        assert_eq!(pixel(&lit, mid, mid)[..3], pixel(&dim, mid, mid)[..3]);
        assert!(pixel(&dim, mid, mid)[3] < pixel(&lit, mid, mid)[3]);
    }

    // --------------------------------------------------------- menu labels

    #[test]
    fn menu_labels_read_as_one_line_each() {
        let rows = vec![row("C:abc", Liveness::Live, Some(1.5), Some(120.0))];
        let menu = build_menu(&rows, true);
        let labels: Vec<String> = menu.iter().map(menu_label).collect();
        assert_eq!(labels[0], "1 live · 0 done · Σ $1.50");
        assert_eq!(
            labels[1],
            format!(
                "{} C:abc · sonnet · Bash · 2m · $1.5000",
                glyph(Liveness::Live)
            )
        );
        assert_eq!(labels[labels.len() - 3], "Mute notifications");
        assert_eq!(labels[labels.len() - 2], "Open hermon watch");
        assert_eq!(labels[labels.len() - 1], "Quit");
    }

    #[test]
    fn a_session_label_skips_fields_it_has_no_data_for() {
        let mut r = row("H:zz", Liveness::Done, None, None);
        r.model = String::new();
        r.last_tool = String::new();
        let label = menu_label(&build_menu(std::slice::from_ref(&r), false)[1]);
        assert_eq!(label, format!("{} H:zz · —", glyph(Liveness::Done)));
    }

    #[test]
    fn overflow_label_counts_the_hidden_sessions() {
        assert_eq!(
            menu_label(&MenuRow::More { count: 10 }),
            "… 10 more sessions"
        );
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
            remotes: Vec::new(),
            remote_flags: Vec::new(),
            docker_auto: false,
        };
        let err = run(config).unwrap_err();
        assert!(err.to_string().contains("macOS only"));
    }
}
