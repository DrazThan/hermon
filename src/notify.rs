//! Decision core plus delivery shell for desktop/terminal notifications:
//! given the tick's lifecycle transitions, decide which alerts should fire
//! ([`decide_alerts`]), then hand them to whatever banner tool the machine
//! actually has ([`probe`] + [`deliver`]).
//!
//! [`LifecycleTransition`] is defined here rather than imported from
//! [`crate::engine`]: at the time of writing, `engine::Lifecycle` does not
//! yet carry attention transitions or done-causes (that's #39, still open),
//! so this module declares the shape it needs and the engine-wiring ticket
//! will be responsible for producing it from real `Event::Lifecycle` data.
//! Keeping it as `notify`'s own type also matches the ticket's "engine
//! wiring is out of scope here" boundary.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::render::fmt_elapsed;
use crate::source::{Attn, Liveness};

/// Why a session went from not-done to done. Only [`DoneCause::TurnDone`]
/// and [`DoneCause::Ended`] are a clean finish worth alerting on;
/// [`DoneCause::Timeout`] means `classify()` gave up waiting, not that the
/// agent actually stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoneCause {
    /// The assistant closed its turn (`SessionMeta::turn_done`).
    TurnDone,
    /// The source's own "session closed" flag (`SessionMeta::ended`).
    Ended,
    /// The idle or tool-pending ceiling blew (`classify()`'s timeout path).
    Timeout,
}

/// One session's liveness before/after this tick, plus whatever
/// `decide_alerts` needs to judge and describe an alert. Sent once per
/// session per tick — including ticks where `from == to` — so ongoing
/// [`Liveness::Attention`] can be re-evaluated for the Stuck/PermWait
/// refire cadence, not just the tick it was first entered.
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleTransition {
    /// [`crate::roster::RosterRow::key`].
    pub key: String,
    /// Display label for the alert banner (roster key or title).
    pub label: String,
    pub from: Liveness,
    pub to: Liveness,
    /// Only meaningful when `to == Liveness::Done && from != Liveness::Done`.
    pub done_cause: Option<DoneCause>,
    /// When the session itself started — used for the `<10s` one-shot guard
    /// and the elapsed shown in a `TurnDone` alert.
    pub started_at: f64,
    /// When the current `to` state was entered — used for the "pending Nm"
    /// duration in a Stuck/PermWait alert. Ignored otherwise.
    pub state_since: f64,
    pub cost: f64,
    pub last_tool: String,
    /// `Some(line)` when a `Sem::Error` line was observed for this session
    /// since the last tick, independent of `from`/`to`.
    pub error_line: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertKind {
    TurnDone,
    Error,
    Stuck,
    PermWait,
}

/// One notification to deliver. `detail` is the banner body, e.g.
/// `"$0.43 · 12m"` for a finished turn or `` "tool `bash` pending 15m" ``
/// for a stuck/waiting session.
#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub key: String,
    pub label: String,
    pub kind: AlertKind,
    pub detail: String,
}

/// Static config, plumbed from CLI flags in the next ticket.
#[derive(Debug, Clone, PartialEq)]
pub struct NotifyCfg {
    pub turn_done: bool,
    pub error: bool,
    pub stuck: bool,
    pub perm_wait: bool,
    /// Per-(session, kind) cooldown for `TurnDone`/`Error`.
    pub cooldown_secs: f64,
    /// `TurnDone` is suppressed for sessions younger than this — the
    /// one-shot `claude -p` spam guard.
    pub min_session_secs: f64,
}

impl Default for NotifyCfg {
    fn default() -> Self {
        NotifyCfg {
            turn_done: true,
            error: true,
            stuck: true,
            perm_wait: true,
            cooldown_secs: 120.0,
            min_session_secs: 10.0,
        }
    }
}

/// Stuck/PermWait refire cadence while the condition persists — longer than
/// the generic cooldown so an abandoned session doesn't re-alert every
/// couple of minutes.
const ATTENTION_REFIRE_SECS: f64 = 300.0;

/// Mutable alert state: per-(session, kind) last-fired times, the global
/// mute flag, and the startup-grace latch. Not persisted across runs.
#[derive(Debug, Default)]
pub struct AlertHistory {
    muted: bool,
    /// Flips to `true` after the first [`decide_alerts`] call; that first
    /// call alerts on nothing, since every "transition" it sees is really
    /// just the boot-time snapshot, not a fresh event.
    booted: bool,
    fired: HashMap<(String, AlertKind), f64>,
}

impl AlertHistory {
    pub fn new() -> Self {
        AlertHistory::default()
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// `true` and records `now` if this (key, kind) is out of its cooldown;
    /// `false` (no side effect) if it's still cooling down.
    fn try_fire(&mut self, key: &str, kind: AlertKind, now: f64, interval: f64) -> bool {
        let due = match self.fired.get(&(key.to_string(), kind)) {
            Some(last) => now - last >= interval,
            None => true,
        };
        if due {
            self.fired.insert((key.to_string(), kind), now);
        }
        due
    }

    /// Leaving a Stuck/PermWait state forgets its history, so re-entering
    /// it later (even soon after) counts as fresh rather than still being
    /// on the old refire clock — new activity earns an immediate alert
    /// next time the condition recurs.
    fn clear_if_left(&mut self, t: &LifecycleTransition) {
        for attn in [Attn::Stuck, Attn::PermWait] {
            if t.from == Liveness::Attention(attn) && t.to != Liveness::Attention(attn) {
                let kind = alert_kind_for(attn);
                self.fired.remove(&(t.key.clone(), kind));
            }
        }
    }

    /// Starts a Stuck/PermWait session's refire clock at `now` without
    /// alerting — what the startup-grace tick does for a session already
    /// mid-attention when hermon (re)starts. Without this, the *next* tick
    /// would see no history for that (key, kind) and read it as a fresh
    /// entry, alerting immediately once grace lifts — the restart would
    /// still bang out a banner, just one tick late.
    fn seed_attention(&mut self, key: &str, kind: AlertKind, now: f64) {
        self.fired.insert((key.to_string(), kind), now);
    }
}

fn alert_kind_for(attn: Attn) -> AlertKind {
    match attn {
        Attn::Stuck => AlertKind::Stuck,
        Attn::PermWait => AlertKind::PermWait,
    }
}

/// Decide which alerts this tick's transitions should raise.
///
/// Called once per engine tick with one [`LifecycleTransition`] per live
/// session (see that type's docs on why ongoing, not just fresh, ticks are
/// included). Pure aside from `hist`, which threads cooldown/refire state
/// and the startup-grace latch between calls.
pub fn decide_alerts(
    transitions: &[LifecycleTransition],
    now: f64,
    cfg: &NotifyCfg,
    hist: &mut AlertHistory,
) -> Vec<Alert> {
    let startup_grace = !hist.booted;
    hist.booted = true;

    let mut alerts = Vec::new();

    for t in transitions {
        hist.clear_if_left(t);

        if startup_grace {
            // A session already needing attention at boot must not alert
            // the instant grace lifts — seed its refire clock now instead
            // of leaving the next tick to treat the ongoing wait as fresh.
            for attn in [Attn::Stuck, Attn::PermWait] {
                if t.to == Liveness::Attention(attn) {
                    hist.seed_attention(&t.key, alert_kind_for(attn), now);
                }
            }
        }

        if startup_grace || hist.muted {
            continue;
        }

        if cfg.turn_done && t.to == Liveness::Done && t.from != Liveness::Done {
            let clean = matches!(
                t.done_cause,
                Some(DoneCause::TurnDone) | Some(DoneCause::Ended)
            );
            let duration = now - t.started_at;
            if clean
                && duration >= cfg.min_session_secs
                && hist.try_fire(&t.key, AlertKind::TurnDone, now, cfg.cooldown_secs)
            {
                alerts.push(Alert {
                    key: t.key.clone(),
                    label: t.label.clone(),
                    kind: AlertKind::TurnDone,
                    detail: format!("${:.2} · {}", t.cost, fmt_elapsed(Some(duration))),
                });
            }
        }

        if cfg.error
            && let Some(line) = &t.error_line
            && hist.try_fire(&t.key, AlertKind::Error, now, cfg.cooldown_secs)
        {
            alerts.push(Alert {
                key: t.key.clone(),
                label: t.label.clone(),
                kind: AlertKind::Error,
                detail: line.clone(),
            });
        }

        for (enabled, attn, kind) in [
            (cfg.stuck, Attn::Stuck, AlertKind::Stuck),
            (cfg.perm_wait, Attn::PermWait, AlertKind::PermWait),
        ] {
            if enabled
                && t.to == Liveness::Attention(attn)
                && hist.try_fire(&t.key, kind, now, ATTENTION_REFIRE_SECS)
            {
                alerts.push(Alert {
                    key: t.key.clone(),
                    label: t.label.clone(),
                    kind,
                    detail: format!(
                        "tool `{}` pending {}",
                        t.last_tool,
                        fmt_elapsed(Some(now - t.state_since))
                    ),
                });
            }
        }
    }

    alerts
}

// --------------------------------------------------------------- delivery

/// The banner tool [`probe`] found on the machine, holding its resolved
/// absolute path so [`deliver`] never has to search `PATH` again (and so
/// tests can point it at a shim without touching the process environment).
/// Tried in this order — richer tool first, most-available fallback last —
/// and cached at engine startup rather than re-probed per alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notifier {
    /// `terminal-notifier` on `PATH`: supports `-sound` and a real app icon.
    TerminalNotifier(PathBuf),
    /// `osascript -e 'display notification …'`, the macOS fallback with no
    /// external dependency — see the README's generic-icon caveat.
    Osascript(PathBuf),
    /// `notify-send`, the Linux fallback.
    NotifySend(PathBuf),
    /// None of the above found: alerts are decided but never shown.
    Silent,
}

/// Finds an executable named `name` on `path_var` (a `PATH`-shaped
/// `:`-separated list), the way a shell would. `None` if it's missing, not a
/// file, or (on Unix) not executable.
fn find_on_path(path_var: &OsStr, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path_var).find_map(|dir| {
        let candidate = dir.join(name);
        is_executable_file(&candidate).then_some(candidate)
    })
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// [`probe`], parameterized on the `PATH` to search — the seam the delivery
/// probe tests use to point at a temp-dir shim instead of the real machine.
pub fn probe_from_path(path_var: Option<&OsStr>) -> Notifier {
    let empty = OsStr::new("");
    let path = path_var.unwrap_or(empty);
    if let Some(bin) = find_on_path(path, "terminal-notifier") {
        return Notifier::TerminalNotifier(bin);
    }
    if let Some(bin) = find_on_path(path, "osascript") {
        return Notifier::Osascript(bin);
    }
    if let Some(bin) = find_on_path(path, "notify-send") {
        return Notifier::NotifySend(bin);
    }
    Notifier::Silent
}

/// Probes once for the best banner tool on `PATH`, in delivery-quality
/// order: `terminal-notifier` → `osascript` → `notify-send` → silent no-op.
/// Call once at engine startup and reuse the result — this touches the
/// filesystem, an alert on every tick should not.
pub fn probe() -> Notifier {
    probe_from_path(std::env::var_os("PATH").as_deref())
}

/// Escapes `"` and `\` for embedding in a double-quoted AppleScript string
/// literal. Order matters: backslashes first, so escaping a quote doesn't
/// double-escape the backslash that `"` → `\"` just introduced.
fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The `osascript -e` argument for one banner. A session title is untrusted
/// text — it can contain `"`, `\`, or a fragment like `"; do shell script
/// "…` aimed at escaping the string literal — so every field is escaped the
/// same way regardless of source.
fn build_osascript(title: &str, subtitle: &str, body: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\" subtitle \"{}\"",
        applescript_escape(body),
        applescript_escape(title),
        applescript_escape(subtitle)
    )
}

/// Fires one banner through whatever [`probe`] found, or does nothing for
/// [`Notifier::Silent`]. Fire-and-forget: spawns the child and returns
/// without waiting on it (no `.wait()`/`.output()`/`.status()` anywhere in
/// here), so a slow or hung notifier process can never stall the engine's
/// tick. A failed spawn is swallowed the same way — a missing or broken
/// notifier degrades to no banner, not a crash.
pub fn deliver(notifier: &Notifier, title: &str, subtitle: &str, body: &str) {
    let mut cmd = match notifier {
        Notifier::TerminalNotifier(bin) => {
            let mut cmd = Command::new(bin);
            cmd.args([
                "-title",
                title,
                "-subtitle",
                subtitle,
                "-message",
                body,
                "-sound",
                "default",
            ]);
            cmd
        }
        Notifier::Osascript(bin) => {
            let mut cmd = Command::new(bin);
            cmd.args(["-e", &build_osascript(title, subtitle, body)]);
            cmd
        }
        Notifier::NotifySend(bin) => {
            let mut cmd = Command::new(bin);
            let full_body = if subtitle.is_empty() {
                body.to_string()
            } else {
                format!("{subtitle}\n{body}")
            };
            cmd.args([title, &full_body]);
            cmd
        }
        Notifier::Silent => return,
    };
    let _ = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// A temp dir containing one executable shim named `name`, whose body
    /// appends its argv (one per line) to `out_file` — enough to prove a
    /// probed [`Notifier`] actually invokes the binary it resolved with the
    /// right arguments, without the test popping a real system notification.
    fn shim_dir(name: &str, out_file: &Path) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join(name);
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do echo \"$a\" >> {:?}; done\n",
                out_file
            ),
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    #[test]
    fn probe_finds_terminal_notifier_first() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("terminal-notifier"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            dir.path().join("terminal-notifier"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let notifier = probe_from_path(Some(dir.path().as_os_str()));
        assert_eq!(
            notifier,
            Notifier::TerminalNotifier(dir.path().join("terminal-notifier"))
        );
    }

    #[test]
    fn probe_falls_back_to_osascript() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("osascript"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            dir.path().join("osascript"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let notifier = probe_from_path(Some(dir.path().as_os_str()));
        assert_eq!(notifier, Notifier::Osascript(dir.path().join("osascript")));
    }

    #[test]
    fn probe_falls_back_to_notify_send() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notify-send"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            dir.path().join("notify-send"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let notifier = probe_from_path(Some(dir.path().as_os_str()));
        assert_eq!(
            notifier,
            Notifier::NotifySend(dir.path().join("notify-send"))
        );
    }

    #[test]
    fn probe_prefers_terminal_notifier_over_osascript() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["terminal-notifier", "osascript"] {
            fs::write(dir.path().join(name), "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            fs::set_permissions(dir.path().join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let notifier = probe_from_path(Some(dir.path().as_os_str()));
        assert_eq!(
            notifier,
            Notifier::TerminalNotifier(dir.path().join("terminal-notifier"))
        );
    }

    #[test]
    fn probe_is_silent_with_nothing_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let notifier = probe_from_path(Some(dir.path().as_os_str()));
        assert_eq!(notifier, Notifier::Silent);
    }

    #[test]
    fn probe_ignores_a_non_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("terminal-notifier"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            dir.path().join("terminal-notifier"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let notifier = probe_from_path(Some(dir.path().as_os_str()));
        #[cfg(unix)]
        assert_eq!(notifier, Notifier::Silent);
    }

    #[test]
    fn deliver_is_a_noop_when_silent() {
        // No child ever spawned — nothing to assert on beyond "doesn't panic".
        deliver(&Notifier::Silent, "hermon", "C:0f865f", "detail");
    }

    /// Polls `path` until it has content or `tries` is exhausted, standing in
    /// for `.wait()` on the spawned shim without the engine-side delivery
    /// path ever doing that itself — this is test-only synchronization.
    fn read_eventually(path: &Path) -> String {
        for _ in 0..200 {
            if let Ok(contents) = fs::read_to_string(path)
                && !contents.is_empty()
            {
                return contents;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("shim never wrote to {}", path.display());
    }

    #[test]
    fn deliver_invokes_terminal_notifier_with_the_banner_fields() {
        let out = tempfile::tempdir().unwrap();
        let out_file = out.path().join("argv.txt");
        let dir = shim_dir("terminal-notifier", &out_file);
        let notifier = Notifier::TerminalNotifier(dir.path().join("terminal-notifier"));

        deliver(&notifier, "hermon", "C:0f865f", "$0.43 \u{b7} 20s");

        let got = read_eventually(&out_file);
        assert!(got.contains("hermon"));
        assert!(got.contains("C:0f865f"));
        assert!(got.contains("$0.43"));
    }

    #[test]
    fn deliver_invokes_osascript_with_an_inert_hostile_title() {
        let out = tempfile::tempdir().unwrap();
        let out_file = out.path().join("argv.txt");
        let dir = shim_dir("osascript", &out_file);
        let notifier = Notifier::Osascript(dir.path().join("osascript"));

        let hostile = "\"; do shell script \"echo pwned\"; --";
        deliver(&notifier, hostile, "sub", "body");

        let got = read_eventually(&out_file);
        // The hostile title's quotes must have been escaped before ever
        // reaching the shell/AppleScript layer — the shim just sees the
        // whole -e script as one inert argv entry.
        assert!(got.contains("display notification"));
        assert!(got.contains("\\\"; do shell script"));
    }

    #[test]
    fn applescript_escaping_neutralizes_quotes_and_backslashes() {
        let hostile = "\"; do shell script \"rm -rf /\"; --";
        let escaped = applescript_escape(hostile);
        assert_eq!(escaped, "\\\"; do shell script \\\"rm -rf /\\\"; --");
        // No unescaped `"` survives — every one is preceded by a backslash.
        for (i, c) in escaped.char_indices() {
            if c == '"' {
                assert!(i > 0 && escaped.as_bytes()[i - 1] == b'\\');
            }
        }
    }

    #[test]
    fn applescript_escaping_handles_a_literal_backslash() {
        assert_eq!(applescript_escape("a\\b"), "a\\\\b");
    }

    #[test]
    fn build_osascript_embeds_all_three_fields_escaped() {
        let script = build_osascript("ti\"tle", "sub\\title", "bo\"dy");
        assert_eq!(
            script,
            "display notification \"bo\\\"dy\" with title \"ti\\\"tle\" subtitle \"sub\\\\title\""
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(key: &str, from: Liveness, to: Liveness) -> LifecycleTransition {
        LifecycleTransition {
            key: key.to_string(),
            label: key.to_string(),
            from,
            to,
            done_cause: None,
            started_at: 0.0,
            state_since: 0.0,
            cost: 0.43,
            last_tool: "bash".to_string(),
            error_line: None,
        }
    }

    fn boot(hist: &mut AlertHistory) {
        // Consume the startup-grace tick with an empty scan so subsequent
        // calls in a test are past it.
        decide_alerts(&[], 0.0, &NotifyCfg::default(), hist);
    }

    #[test]
    fn turn_done_fires_on_clean_finish() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let mut tr = t("k1", Liveness::Live, Liveness::Done);
        tr.done_cause = Some(DoneCause::TurnDone);
        tr.started_at = -20.0; // 20s old at now=0
        let alerts = decide_alerts(&[tr], 0.0, &NotifyCfg::default(), &mut hist);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, AlertKind::TurnDone);
        assert_eq!(alerts[0].detail, "$0.43 · 20s");
    }

    #[test]
    fn turn_done_fires_on_ended_too() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let mut tr = t("k1", Liveness::Live, Liveness::Done);
        tr.done_cause = Some(DoneCause::Ended);
        tr.started_at = -20.0;
        let alerts = decide_alerts(&[tr], 0.0, &NotifyCfg::default(), &mut hist);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, AlertKind::TurnDone);
    }

    #[test]
    fn timeout_death_never_turn_dones() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let mut tr = t("k1", Liveness::Live, Liveness::Done);
        tr.done_cause = Some(DoneCause::Timeout);
        tr.started_at = -1000.0;
        let alerts = decide_alerts(&[tr], 0.0, &NotifyCfg::default(), &mut hist);
        assert!(alerts.is_empty());
    }

    #[test]
    fn attention_to_done_also_turn_dones() {
        // classify() lets a session go straight from Attention(PermWait) to
        // Done once the user answers and the turn closes clean.
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let mut tr = t("k1", Liveness::Attention(Attn::PermWait), Liveness::Done);
        tr.done_cause = Some(DoneCause::TurnDone);
        tr.started_at = -20.0;
        let alerts = decide_alerts(&[tr], 0.0, &NotifyCfg::default(), &mut hist);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, AlertKind::TurnDone);
    }

    #[test]
    fn short_session_turn_done_suppressed() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let mut tr = t("k1", Liveness::Live, Liveness::Done);
        tr.done_cause = Some(DoneCause::TurnDone);
        tr.started_at = -9.999; // just under 10s
        let alerts = decide_alerts(&[tr], 0.0, &NotifyCfg::default(), &mut hist);
        assert!(alerts.is_empty());
    }

    #[test]
    fn ten_second_boundary_is_not_suppressed() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let mut tr = t("k1", Liveness::Live, Liveness::Done);
        tr.done_cause = Some(DoneCause::TurnDone);
        tr.started_at = -10.0; // exactly at the floor
        let alerts = decide_alerts(&[tr], 0.0, &NotifyCfg::default(), &mut hist);
        assert_eq!(alerts.len(), 1);
    }

    #[test]
    fn error_fires_on_observed_line() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let mut tr = t("k1", Liveness::Live, Liveness::Live);
        tr.error_line = Some("bash: command not found".to_string());
        let alerts = decide_alerts(&[tr], 0.0, &NotifyCfg::default(), &mut hist);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, AlertKind::Error);
        assert_eq!(alerts[0].detail, "bash: command not found");
    }

    #[test]
    fn stuck_fires_on_entry() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let mut tr = t("k1", Liveness::Live, Liveness::Attention(Attn::Stuck));
        tr.state_since = 0.0;
        let alerts = decide_alerts(&[tr], 900.0, &NotifyCfg::default(), &mut hist);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, AlertKind::Stuck);
        assert_eq!(alerts[0].detail, "tool `bash` pending 15m00s");
    }

    #[test]
    fn perm_wait_fires_on_entry() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let tr = t("k1", Liveness::Live, Liveness::Attention(Attn::PermWait));
        let alerts = decide_alerts(&[tr], 30.0, &NotifyCfg::default(), &mut hist);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, AlertKind::PermWait);
    }

    #[test]
    fn every_kind_fires_only_on_its_own_transition() {
        // A transition that is none of the alertable shapes raises nothing.
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let boring = t("k1", Liveness::Live, Liveness::Live);
        let started = t("k1", Liveness::Live, Liveness::Live); // no-op tick
        let resumed = t("k1", Liveness::Done, Liveness::Live);
        let alerts = decide_alerts(
            &[boring, started, resumed],
            100.0,
            &NotifyCfg::default(),
            &mut hist,
        );
        assert!(alerts.is_empty());
    }

    #[test]
    fn cooldown_suppresses_within_window_and_refires_after() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let cfg = NotifyCfg::default(); // cooldown_secs = 120
        let mut err = |now: f64| {
            let mut tr = t("k1", Liveness::Live, Liveness::Live);
            tr.error_line = Some("boom".to_string());
            decide_alerts(&[tr], now, &cfg, &mut hist)
        };
        assert_eq!(err(0.0).len(), 1, "first error fires");
        assert_eq!(err(119.0).len(), 0, "still cooling down at +119s");
        assert_eq!(err(121.0).len(), 1, "cooldown cleared by +121s");
    }

    #[test]
    fn stuck_refires_every_five_minutes_while_persisting() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let cfg = NotifyCfg::default();
        let mut stuck_tick = |from: Liveness, now: f64| {
            let tr = t("k1", from, Liveness::Attention(Attn::Stuck));
            decide_alerts(&[tr], now, &cfg, &mut hist)
        };
        assert_eq!(stuck_tick(Liveness::Live, 0.0).len(), 1, "enters stuck");
        assert_eq!(
            stuck_tick(Liveness::Attention(Attn::Stuck), 100.0).len(),
            0,
            "still within 5min refire window"
        );
        assert_eq!(
            stuck_tick(Liveness::Attention(Attn::Stuck), 300.0).len(),
            1,
            "5 minutes since first fire — refires"
        );
    }

    #[test]
    fn stuck_clears_on_activity_and_refires_immediately() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let cfg = NotifyCfg::default();

        let enter = t("k1", Liveness::Live, Liveness::Attention(Attn::Stuck));
        assert_eq!(decide_alerts(&[enter], 0.0, &cfg, &mut hist).len(), 1);

        // Activity resumes — session leaves Stuck well within the 5min window.
        let cleared = t("k1", Liveness::Attention(Attn::Stuck), Liveness::Live);
        assert_eq!(decide_alerts(&[cleared], 50.0, &cfg, &mut hist).len(), 0);

        // It gets stuck again soon after — since history was cleared, this
        // counts as fresh and fires immediately rather than waiting out the
        // rest of the original 5 minutes.
        let again = t("k1", Liveness::Live, Liveness::Attention(Attn::Stuck));
        let alerts = decide_alerts(&[again], 80.0, &cfg, &mut hist);
        assert_eq!(alerts.len(), 1);
    }

    #[test]
    fn startup_grace_alerts_on_nothing() {
        let mut hist = AlertHistory::new();
        let mut tr = t("k1", Liveness::Live, Liveness::Done);
        tr.done_cause = Some(DoneCause::TurnDone);
        tr.started_at = -100.0;
        let mut tr2 = t("k2", Liveness::Live, Liveness::Attention(Attn::Stuck));
        tr2.state_since = 0.0;
        // First call ever — everything "finishing" at boot is history.
        let alerts = decide_alerts(&[tr, tr2], 100.0, &NotifyCfg::default(), &mut hist);
        assert!(alerts.is_empty());

        // A genuinely new transition on the next tick fires normally.
        let mut tr3 = t("k3", Liveness::Live, Liveness::Done);
        tr3.done_cause = Some(DoneCause::TurnDone);
        tr3.started_at = 50.0;
        let alerts = decide_alerts(&[tr3], 200.0, &NotifyCfg::default(), &mut hist);
        assert_eq!(alerts.len(), 1);
    }

    /// A session already mid-attention when hermon (re)starts must not bang
    /// out a banner the instant grace lifts either — the whole point of
    /// restart being silent is that the *ongoing* wait doesn't retroactively
    /// count as a fresh entry once the boot tick is behind it.
    #[test]
    fn restart_mid_attention_does_not_alert_once_grace_lifts() {
        let mut hist = AlertHistory::new();
        let stuck = t(
            "k1",
            Liveness::Attention(Attn::Stuck),
            Liveness::Attention(Attn::Stuck),
        );
        // Boot tick: startup grace, nothing fires.
        let alerts = decide_alerts(
            std::slice::from_ref(&stuck),
            0.0,
            &NotifyCfg::default(),
            &mut hist,
        );
        assert!(alerts.is_empty());

        // The very next tick, still stuck, no new activity: must stay quiet
        // rather than reading the still-ongoing wait as a fresh entry.
        let alerts = decide_alerts(&[stuck], 1.0, &NotifyCfg::default(), &mut hist);
        assert!(
            alerts.is_empty(),
            "a restart must not re-alert on old attention state"
        );
    }

    #[test]
    fn mute_drops_everything() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        hist.set_muted(true);
        let mut tr = t("k1", Liveness::Live, Liveness::Done);
        tr.done_cause = Some(DoneCause::TurnDone);
        tr.started_at = -20.0;
        let mut tr2 = t("k2", Liveness::Live, Liveness::Attention(Attn::PermWait));
        let mut tr3 = t("k3", Liveness::Live, Liveness::Live);
        tr3.error_line = Some("boom".to_string());
        tr2.state_since = 0.0;
        let alerts = decide_alerts(&[tr, tr2, tr3], 0.0, &NotifyCfg::default(), &mut hist);
        assert!(alerts.is_empty());

        hist.set_muted(false);
        let mut tr4 = t("k4", Liveness::Live, Liveness::Done);
        tr4.done_cause = Some(DoneCause::TurnDone);
        tr4.started_at = -20.0;
        let alerts = decide_alerts(&[tr4], 1.0, &NotifyCfg::default(), &mut hist);
        assert_eq!(alerts.len(), 1, "unmuting restores normal behavior");
    }

    #[test]
    fn per_kind_disable_suppresses_only_that_kind() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let cfg = NotifyCfg {
            turn_done: false,
            ..NotifyCfg::default()
        };

        let mut tr = t("k1", Liveness::Live, Liveness::Done);
        tr.done_cause = Some(DoneCause::TurnDone);
        tr.started_at = -20.0;
        assert!(decide_alerts(&[tr], 0.0, &cfg, &mut hist).is_empty());

        let tr2 = t("k2", Liveness::Live, Liveness::Attention(Attn::Stuck));
        assert_eq!(decide_alerts(&[tr2], 0.0, &cfg, &mut hist).len(), 1);
    }

    #[test]
    fn per_kind_disable_covers_error_stuck_perm_wait() {
        let mut hist = AlertHistory::new();
        boot(&mut hist);
        let cfg = NotifyCfg {
            error: false,
            stuck: false,
            perm_wait: false,
            ..NotifyCfg::default()
        };

        let mut tr_err = t("k1", Liveness::Live, Liveness::Live);
        tr_err.error_line = Some("boom".to_string());
        let tr_stuck = t("k2", Liveness::Live, Liveness::Attention(Attn::Stuck));
        let tr_perm = t("k3", Liveness::Live, Liveness::Attention(Attn::PermWait));

        let alerts = decide_alerts(&[tr_err, tr_stuck, tr_perm], 0.0, &cfg, &mut hist);
        assert!(alerts.is_empty());
    }
}
