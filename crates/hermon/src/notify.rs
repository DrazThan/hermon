//! Notification delivery through the available desktop banner tool.
//!
//! Decisions and cooldown state live in `hermon_core::notify` and are
//! reexported here for the application’s existing callers.

pub use hermon_core::notify::*;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::render::sanitize;

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
pub(crate) fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The `osascript -e` argument for one banner. A session title is untrusted
/// text — it can contain `"`, `\`, or terminal control sequences, so every
/// field is sanitized and escaped for both control sequences and AppleScript syntax.
fn build_osascript(title: &str, subtitle: &str, body: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\" subtitle \"{}\"",
        applescript_escape(&sanitize(body)),
        applescript_escape(&sanitize(title)),
        applescript_escape(&sanitize(subtitle))
    )
}

/// Fires one banner through whatever [`probe`] found, or does nothing for
/// [`Notifier::Silent`]. Fire-and-forget: spawns the child and returns
/// without waiting on it (no `.wait()`/`.output()`/`.status()` anywhere in
/// here), so a slow or hung notifier process can never stall the engine's
/// tick. A failed spawn is swallowed the same way — a missing or broken
/// notifier degrades to no banner, not a crash.
pub fn deliver(notifier: &Notifier, title: &str, subtitle: &str, body: &str) {
    let title = sanitize(title);
    let subtitle = sanitize(subtitle);
    let body = sanitize(body);
    let mut cmd = match notifier {
        Notifier::TerminalNotifier(bin) => {
            let mut cmd = Command::new(bin);
            cmd.args([
                "-title",
                &title,
                "-subtitle",
                &subtitle,
                "-message",
                &body,
                "-sound",
                "default",
            ]);
            cmd
        }
        Notifier::Osascript(bin) => {
            let mut cmd = Command::new(bin);
            cmd.args(["-e", &build_osascript(&title, &subtitle, &body)]);
            cmd
        }
        Notifier::NotifySend(bin) => {
            let mut cmd = Command::new(bin);
            let full_body = if subtitle.is_empty() {
                body.to_string()
            } else {
                format!("{subtitle}\n{body}")
            };
            cmd.args([&title, &full_body]);
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

    #[test]
    fn build_osascript_sanitizes_control_sequences_in_title() {
        let script = build_osascript("title\x1b]0;hack\x07ed", "sub", "body");
        // Should not contain raw ESC or BEL bytes, only escaped replacement chars
        assert!(!script.contains('\x1b'));
        assert!(!script.contains('\x07'));
    }

    #[test]
    fn deliver_terminal_notifier_receives_sanitized_fields() {
        let out = tempfile::tempdir().unwrap();
        let out_file = out.path().join("argv.txt");
        let dir = shim_dir("terminal-notifier", &out_file);
        let notifier = Notifier::TerminalNotifier(dir.path().join("terminal-notifier"));

        deliver(
            &notifier,
            "title\x1b]0;hack\x07",
            "sub\x00tle",
            "body\x1bcode",
        );

        let got = read_eventually(&out_file);
        // Should not contain raw control bytes in the arguments
        assert!(!got.contains('\x1b'));
        assert!(!got.contains('\x00'));
    }

    #[test]
    fn deliver_osascript_receives_sanitized_and_escaped_fields() {
        let out = tempfile::tempdir().unwrap();
        let out_file = out.path().join("argv.txt");
        let dir = shim_dir("osascript", &out_file);
        let notifier = Notifier::Osascript(dir.path().join("osascript"));

        deliver(&notifier, "hostile\x1b]0;title\x07", "subtitle", "body");

        let got = read_eventually(&out_file);
        // The script should be well-formed and contain sanitized content
        assert!(got.contains("display notification"));
        // No raw escape or control bytes should reach the shell
        assert!(!got.contains('\x1b'));
        assert!(!got.contains('\x00'));
    }
}
