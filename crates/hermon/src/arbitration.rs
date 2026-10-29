//! Which hermon process gets to deliver desktop banners.
//!
//! Every UI (`watch`, `gui`, `menubar`) runs its own [`crate::engine::Engine`],
//! and the engine fires notifications — so two UIs open at once means every
//! banner arrives twice. The fix is a claim, not a lock: whichever process is
//! actually notifying writes a pidfile named after its [`UiKind`] into the
//! user's runtime dir, and a starting process that finds a *higher-precedence*
//! kind already holding one defaults its own notifications off.
//!
//! Precedence is [`UiKind::rank`]: `menubar` > `gui` > `watch`, the
//! always-running process winning over the transient one. An explicit
//! `--notify` / `--no-notify` always beats the default resolution, so the
//! arbitration only ever decides what happens when the user said nothing.
//!
//! The pidfile is state about hermon itself, not about the agents, so this
//! leaves the engine's read-only-stores principle alone. Nothing here mutates
//! or reads a session store.
//!
//! Two caveats, both deliberate: a second instance of the *same* kind
//! overwrites the first's pidfile rather than deferring to it (two menu-bar
//! icons is already user error), and mute state is per-process — muting in the
//! TUI does not mute the menu bar.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A hermon UI that can hold the notifier role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiKind {
    Watch,
    Gui,
    Menubar,
}

impl UiKind {
    /// Highest precedence first, so [`decide`] can stop at the first holder
    /// it finds.
    const BY_RANK: [UiKind; 3] = [UiKind::Menubar, UiKind::Gui, UiKind::Watch];

    /// Bigger wins. The menu bar is the process that is always up, so it is
    /// the one banners should come from.
    fn rank(self) -> u8 {
        match self {
            UiKind::Watch => 0,
            UiKind::Gui => 1,
            UiKind::Menubar => 2,
        }
    }

    fn name(self) -> &'static str {
        match self {
            UiKind::Watch => "watch",
            UiKind::Gui => "gui",
            UiKind::Menubar => "menubar",
        }
    }

    fn pidfile(self) -> String {
        format!("{}.pid", self.name())
    }
}

impl fmt::Display for UiKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What the user asked for on the command line. [`NotifyFlag::Default`] — no
/// flag at all — is the only case arbitration gets a say in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyFlag {
    Default,
    /// `--notify`
    ForceOn,
    /// `--no-notify`
    ForceOff,
}

/// The resolved answer for one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// Whether this process should deliver banners at all.
    pub notify: bool,
    /// `Some(kind)` only when notifications defaulted off because that kind
    /// is already notifying — the subject of `watch`'s one-line notice. Stays
    /// `None` for an explicit `--no-notify`, which nobody needs telling about.
    pub yielded_to: Option<UiKind>,
}

/// Where the pidfiles live: `$XDG_RUNTIME_DIR/hermon` where the platform has
/// one, else the per-user cache dir (macOS has no runtime dir). `None` when
/// neither exists, which simply disables arbitration — every process then
/// notifies, the pre-#72 behaviour.
pub fn runtime_dir() -> Option<PathBuf> {
    dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .map(|d| d.join("hermon"))
}

/// Claims the notifier role for `kind` by writing this process's pid into
/// `dir`. Only call this once the process has actually decided to notify —
/// the pidfile means "this kind is delivering banners", not "this kind is
/// running", so a `menubar --no-notify` must not claim it.
pub fn claim(dir: &Path, kind: UiKind) -> io::Result<PidGuard> {
    fs::create_dir_all(dir)?;
    let path = dir.join(kind.pidfile());
    fs::write(&path, format!("{}\n", std::process::id()))?;
    Ok(PidGuard { path: Some(path) })
}

/// Removes the pidfile [`claim`] wrote, on drop or on an explicit
/// [`PidGuard::release`]. The macOS event loop exits the process without
/// unwinding, so the tray backend calls `release` itself rather than trusting
/// the destructor to run.
#[derive(Debug)]
pub struct PidGuard {
    path: Option<PathBuf>,
}

impl PidGuard {
    pub fn release(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        self.release();
    }
}

/// `true` if `kind` has a pidfile in `dir` naming a process that still exists.
/// A missing file, an unreadable or malformed one, and a stale pid all read
/// the same way: nobody is holding that role.
pub fn is_running(dir: &Path, kind: UiKind) -> bool {
    is_running_with(dir, kind, pid_alive)
}

/// Resolves whether this process notifies. `dir` is `None` when the platform
/// gave us nowhere to look, in which case no arbitration happens.
pub fn decide(dir: Option<&Path>, kind: UiKind, flag: NotifyFlag) -> Decision {
    decide_with(dir, kind, flag, pid_alive)
}

fn is_running_with(dir: &Path, kind: UiKind, alive: impl Fn(u32) -> bool) -> bool {
    read_pid(&dir.join(kind.pidfile())).is_some_and(alive)
}

fn decide_with(
    dir: Option<&Path>,
    kind: UiKind,
    flag: NotifyFlag,
    alive: impl Fn(u32) -> bool + Copy,
) -> Decision {
    match flag {
        NotifyFlag::ForceOn => {
            return Decision {
                notify: true,
                yielded_to: None,
            };
        }
        NotifyFlag::ForceOff => {
            return Decision {
                notify: false,
                yielded_to: None,
            };
        }
        NotifyFlag::Default => {}
    }

    let owner = dir.and_then(|dir| {
        UiKind::BY_RANK
            .into_iter()
            .find(|other| other.rank() > kind.rank() && is_running_with(dir, *other, alive))
    });
    Decision {
        notify: owner.is_none(),
        yielded_to: owner,
    }
}

fn read_pid(path: &Path) -> Option<u32> {
    let pid: u32 = fs::read_to_string(path).ok()?.trim().parse().ok()?;
    // `kill -0 0` addresses our own process group, which would always look
    // alive; a pidfile claiming pid 0 is corrupt, not a live process.
    (pid != 0).then_some(pid)
}

/// Does this pid exist? hermon has no `libc` dependency, so the POSIX
/// `kill(pid, 0)` probe is spawned as `kill(1)` instead. That costs a process
/// spawn, which is fine: this runs once at startup, never per tick. A `kill`
/// we cannot run at all reads as "cannot prove it is alive" — the failure mode
/// is a duplicate banner the user can silence with `--no-notify`, not a
/// permanently muted hermon.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pid that is guaranteed not to be running: spawn a process, wait for
    /// it, and use the id the kernel has now freed.
    fn reaped_pid() -> u32 {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    fn write_pid(dir: &Path, kind: UiKind, pid: u32) {
        fs::write(dir.join(kind.pidfile()), format!("{pid}\n")).unwrap();
    }

    // ------------------------------------------------ pidfile liveness check

    #[test]
    fn a_missing_pidfile_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_running(dir.path(), UiKind::Menubar));
    }

    #[test]
    fn a_live_pid_is_running() {
        let dir = tempfile::tempdir().unwrap();
        write_pid(dir.path(), UiKind::Menubar, std::process::id());
        assert!(is_running(dir.path(), UiKind::Menubar));
    }

    #[test]
    fn a_stale_pid_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        write_pid(dir.path(), UiKind::Menubar, reaped_pid());
        assert!(!is_running(dir.path(), UiKind::Menubar));
    }

    #[test]
    fn a_malformed_pidfile_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(UiKind::Menubar.pidfile()), "not a pid").unwrap();
        assert!(!is_running(dir.path(), UiKind::Menubar));
        // Pid 0 would signal our own process group rather than a process.
        fs::write(dir.path().join(UiKind::Menubar.pidfile()), "0").unwrap();
        assert!(!is_running(dir.path(), UiKind::Menubar));
    }

    #[test]
    fn each_kind_has_its_own_pidfile() {
        let dir = tempfile::tempdir().unwrap();
        write_pid(dir.path(), UiKind::Menubar, std::process::id());
        assert!(is_running(dir.path(), UiKind::Menubar));
        assert!(!is_running(dir.path(), UiKind::Watch));
    }

    // ------------------------------------------------------------- the claim

    #[test]
    fn claim_writes_our_pid_and_the_guard_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("hermon");
        {
            let _guard = claim(&nested, UiKind::Menubar).unwrap();
            assert_eq!(
                read_pid(&nested.join("menubar.pid")),
                Some(std::process::id())
            );
            assert!(is_running(&nested, UiKind::Menubar));
        }
        assert!(!nested.join("menubar.pid").exists());
    }

    #[test]
    fn release_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut guard = claim(dir.path(), UiKind::Menubar).unwrap();
        guard.release();
        guard.release();
        assert!(!dir.path().join("menubar.pid").exists());
    }

    // -------------------------------------------------- defaults resolution

    /// Precedence tests inject liveness so they neither spawn `kill` nor
    /// depend on which pids the machine happens to have.
    fn live(_pid: u32) -> bool {
        true
    }
    fn dead(_pid: u32) -> bool {
        false
    }

    fn decide_in(dir: &Path, kind: UiKind, alive: fn(u32) -> bool) -> Decision {
        decide_with(Some(dir), kind, NotifyFlag::Default, alive)
    }

    #[test]
    fn watch_notifies_when_nothing_else_does() {
        let dir = tempfile::tempdir().unwrap();
        let decision = decide_in(dir.path(), UiKind::Watch, live);
        assert_eq!(
            decision,
            Decision {
                notify: true,
                yielded_to: None
            }
        );
    }

    #[test]
    fn watch_yields_to_a_running_menubar() {
        let dir = tempfile::tempdir().unwrap();
        write_pid(dir.path(), UiKind::Menubar, 4242);
        let decision = decide_in(dir.path(), UiKind::Watch, live);
        assert_eq!(
            decision,
            Decision {
                notify: false,
                yielded_to: Some(UiKind::Menubar)
            }
        );
    }

    #[test]
    fn watch_ignores_a_stale_menubar_pidfile() {
        let dir = tempfile::tempdir().unwrap();
        write_pid(dir.path(), UiKind::Menubar, 4242);
        let decision = decide_in(dir.path(), UiKind::Watch, dead);
        assert!(decision.notify);
    }

    #[test]
    fn the_menubar_never_yields_to_a_lesser_ui() {
        let dir = tempfile::tempdir().unwrap();
        write_pid(dir.path(), UiKind::Watch, 4242);
        write_pid(dir.path(), UiKind::Gui, 4243);
        let decision = decide_in(dir.path(), UiKind::Menubar, live);
        assert!(decision.notify);
    }

    /// The seam #77 extends: `gui` sits between the two, so it yields upward
    /// and holds against `watch` without any further change here.
    #[test]
    fn gui_yields_up_but_not_down() {
        let dir = tempfile::tempdir().unwrap();
        write_pid(dir.path(), UiKind::Watch, 4242);
        assert!(decide_in(dir.path(), UiKind::Gui, live).notify);

        write_pid(dir.path(), UiKind::Menubar, 4243);
        assert_eq!(
            decide_in(dir.path(), UiKind::Gui, live).yielded_to,
            Some(UiKind::Menubar)
        );
    }

    #[test]
    fn the_menubar_outranks_the_gui_as_the_reported_owner() {
        let dir = tempfile::tempdir().unwrap();
        write_pid(dir.path(), UiKind::Gui, 4242);
        write_pid(dir.path(), UiKind::Menubar, 4243);
        assert_eq!(
            decide_in(dir.path(), UiKind::Watch, live).yielded_to,
            Some(UiKind::Menubar)
        );
    }

    #[test]
    fn explicit_notify_beats_a_running_menubar() {
        let dir = tempfile::tempdir().unwrap();
        write_pid(dir.path(), UiKind::Menubar, 4242);
        let decision = decide_with(Some(dir.path()), UiKind::Watch, NotifyFlag::ForceOn, live);
        assert_eq!(
            decision,
            Decision {
                notify: true,
                yielded_to: None
            }
        );
    }

    #[test]
    fn explicit_no_notify_beats_an_empty_field() {
        let dir = tempfile::tempdir().unwrap();
        let decision = decide_with(
            Some(dir.path()),
            UiKind::Menubar,
            NotifyFlag::ForceOff,
            live,
        );
        assert_eq!(
            decision,
            Decision {
                notify: false,
                yielded_to: None
            }
        );
    }

    #[test]
    fn no_runtime_dir_means_no_arbitration() {
        let decision = decide_with(None, UiKind::Watch, NotifyFlag::Default, live);
        assert!(decision.notify);
    }
}
