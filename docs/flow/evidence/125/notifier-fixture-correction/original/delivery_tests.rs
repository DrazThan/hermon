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
