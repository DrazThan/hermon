//! Watch must keep unavailable-store diagnostics on the TUI, not on stderr.
//!
//! A shared terminal paints stderr into the same frame as ratatui. This case
//! captures the PTY and stderr separately. Headless `ls` must still surface
//! the same store failure. Unix PTY only; the driver is Python 3 stdlib.

#![cfg(unix)]

mod common;

use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::hermes_profiles::{
    absent_source_args, child_env, discovery_ls_args, empty_home, hermon, seed_finished,
};

const INITIAL_SLUG: &str = "bad";
const LATE_SLUG: &str = "latebad";
const TITLE: &str = "watch-fixture";
const SENTINEL: &str = "sentinel-watch";

#[test]
fn watch_keeps_unavailable_profile_diagnostics_on_the_ui_not_the_terminal() {
    let python = python3().unwrap_or_else(|| panic!("harness: python3 is not on PATH"));
    let home = empty_home();
    seed_finished(&home.main_db(), "sess-watch", TITLE, SENTINEL);
    write_corrupt(&home.profile_db(INITIAL_SLUG));

    let listed = hermon(
        home.path(),
        &discovery_ls_args(home.path()),
        Duration::from_secs(20),
    );
    assert!(
        listed.success(),
        "harness: headless ls failed\n{}",
        dump(&listed.stdout, &listed.stderr)
    );
    let listed_text = listed.combined();
    assert!(
        listed.stdout.contains(TITLE),
        "harness: ls did not render the fixture roster\n{listed_text}"
    );
    assert!(
        listed_text.contains("unavailable") && listed_text.contains(INITIAL_SLUG),
        "headless ls must keep a useful unavailable-store diagnostic\n{listed_text}"
    );
    assert!(
        listed
            .stderr
            .contains("hermon: Hermes bad: store unavailable"),
        "headless ls must keep the unavailable-profile diagnostic on stderr\n{}",
        listed.stderr
    );

    let screen_path = home.path().join("tmp").join("watch.screen");
    let stderr_path = home.path().join("tmp").join("watch.stderr");
    let stop_path = home.path().join("tmp").join("watch.stop");
    let pid_path = home.path().join("tmp").join("watch.pid");
    let script_path = home.path().join("tmp").join("pty_watch.py");
    fs::write(&script_path, PTY_DRIVER).expect("write pty driver");
    let mut watch_args = vec![
        "watch".to_string(),
        "--no-notify".to_string(),
        "--interval".to_string(),
        "0.5".to_string(),
    ];
    watch_args.extend(absent_source_args(home.path(), true));

    let mut cmd = Command::new(&python);
    cmd.arg(&script_path)
        .arg(env!("CARGO_BIN_EXE_hermon"))
        .arg(&screen_path)
        .arg(&stderr_path)
        .arg(&stop_path)
        .arg(&pid_path)
        .arg("24")
        .args(&watch_args)
        .env_remove("HERMES_HOME")
        .env("TERM", "xterm-256color")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (key, value) in child_env(home.path()) {
        cmd.env(key, value);
    }
    let driver = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("harness: spawn python3 pty driver: {e}"));
    let mut guard = DriverGuard {
        child: Some(driver),
        stop: stop_path.clone(),
        pid_path: pid_path.clone(),
        python,
    };

    let initial = wait_for_text(&screen_path, Duration::from_secs(8), |text| {
        text.contains(SENTINEL) && text.contains(&format!("Hermes {INITIAL_SLUG}:"))
    });
    let screen = visible_text(&initial);
    if !drew_tui(&initial) {
        panic!(
            "harness: watch did not draw a TUI\nscreen:\n{screen}\nstderr:\n{}",
            read_text(&stderr_path)
        );
    }
    assert!(
        screen.contains(SENTINEL) && screen.contains(TITLE),
        "watch must render the fixture roster and transcript\n{screen}"
    );
    assert!(
        screen.contains(&format!("Hermes {INITIAL_SLUG}:")) && screen.contains("store unavailable"),
        "unavailable profile warning must be on the TUI, not only stderr\n{screen}"
    );

    write_corrupt(&home.profile_db(LATE_SLUG));
    let refreshed = wait_for_text(&screen_path, Duration::from_secs(8), |text| {
        text.contains(&format!("Hermes {LATE_SLUG}:"))
    });
    let screen = visible_text(&refreshed);
    assert!(
        screen.contains(&format!("Hermes {LATE_SLUG}:")) && screen.contains("store unavailable"),
        "a store that fails after watch is already active must appear on the TUI within the discovery refresh\n{screen}"
    );

    let (status, driver_err) = guard.finish();
    assert!(
        status.success(),
        "harness: pty driver exited {status:?}\n{driver_err}"
    );
    let stderr = read_text(&stderr_path);
    let marker = paint_marker(&stderr);
    assert!(
        marker.is_none(),
        "watch stderr would paint a shared live terminal ({})\n{stderr}",
        marker.unwrap_or("diagnostic")
    );
}

fn python3() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|dir| {
            let candidate = dir.join("python3");
            candidate.is_file().then_some(candidate)
        })
    })
}

fn write_corrupt(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, b"not a sqlite database").unwrap();
}

fn dump(stdout: &str, stderr: &str) -> String {
    format!("stdout:\n{stdout}\nstderr:\n{stderr}")
}

fn read_text(path: &Path) -> String {
    String::from_utf8_lossy(&fs::read(path).unwrap_or_default()).into_owned()
}

fn wait_for_text(path: &Path, deadline: Duration, pred: impl Fn(&str) -> bool) -> String {
    let end = Instant::now() + deadline;
    loop {
        let raw = read_text(path);
        if pred(&visible_text(&raw)) || Instant::now() >= end {
            return raw;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn drew_tui(raw: &str) -> bool {
    raw.contains('\u{1b}') || visible_text(raw).contains('─')
}

fn visible_text(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\u{7}' {
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn paint_marker(stderr: &str) -> Option<&'static str> {
    ["unavailable", "lsof not found", "hermon:"]
        .into_iter()
        .find(|marker| stderr.contains(marker))
}

fn wait_bounded(child: &mut Child, deadline: Duration) -> Option<ExitStatus> {
    let end = Instant::now() + deadline;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() >= end => return None,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(err) => panic!("harness: wait for pty driver: {err}"),
        }
    }
}

fn signal_owned(python: &Path, pid: u32, group: bool, kill: bool) {
    if pid <= 1 {
        return;
    }
    let call = if group { "os.killpg" } else { "os.kill" };
    let sig = if kill { "SIGKILL" } else { "SIGTERM" };
    let _ = Command::new(python)
        .arg("-c")
        .arg(format!(
            "import os, signal, sys; {call}(int(sys.argv[1]), signal.{sig})"
        ))
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn reap_owned(python: &Path, pid_path: &Path, driver_pid: u32) {
    let watch_pid = fs::read_to_string(pid_path)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .filter(|pid| *pid > 1);
    if let Some(pid) = watch_pid {
        signal_owned(python, pid, true, false);
        thread::sleep(Duration::from_millis(200));
        signal_owned(python, pid, true, true);
    }
    signal_owned(python, driver_pid, true, false);
}

struct DriverGuard {
    child: Option<Child>,
    stop: PathBuf,
    pid_path: PathBuf,
    python: PathBuf,
}

impl DriverGuard {
    fn finish(&mut self) -> (ExitStatus, String) {
        let _ = fs::write(&self.stop, b"stop");
        let mut child = self.child.take().expect("pty driver");
        let status = wait_bounded(&mut child, Duration::from_secs(4)).unwrap_or_else(|| {
            reap_owned(&self.python, &self.pid_path, child.id());
            wait_bounded(&mut child, Duration::from_secs(1)).unwrap_or_else(|| {
                let _ = child.kill();
                wait_bounded(&mut child, Duration::from_secs(1))
                    .unwrap_or_else(|| panic!("harness: pty driver did not exit after owned reap"))
            })
        });
        let mut err = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut err);
        }
        (status, err)
    }
}

impl Drop for DriverGuard {
    fn drop(&mut self) {
        let _ = fs::write(&self.stop, b"stop");
        let Some(mut child) = self.child.take() else {
            return;
        };
        if wait_bounded(&mut child, Duration::from_secs(3)).is_some() {
            return;
        }
        reap_owned(&self.python, &self.pid_path, child.id());
        if wait_bounded(&mut child, Duration::from_secs(1)).is_none() {
            let _ = child.kill();
            let _ = wait_bounded(&mut child, Duration::from_secs(1));
        }
    }
}

const PTY_DRIVER: &str = r#"
import errno, fcntl, os, pty, select, signal, struct, subprocess, sys, termios, time

def read_fd(fd):
    try:
        return os.read(fd, 65536)
    except OSError as exc:
        if exc.errno in (errno.EIO, errno.EBADF):
            return b""
        raise

def take(ready, screen, err, master, live):
    for fd in ready:
        chunk = read_fd(fd)
        if not chunk:
            live.discard(fd)
            continue
        out = screen if fd == master else err
        out.write(chunk)
        out.flush()

def drain(live, screen, err, master, seconds):
    end = time.monotonic() + seconds
    while live and time.monotonic() < end:
        remaining = max(0, end - time.monotonic())
        readable, _, _ = select.select(list(live), [], [], min(0.05, remaining))
        if not readable:
            continue
        take(readable, screen, err, master, live)

def reap(proc):
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except OSError:
        pass
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except OSError:
            proc.kill()
        proc.wait(timeout=2)

def main():
    binary, screen_path, err_path, stop_path, pid_path, seconds, *watch_args = sys.argv[1:]
    deadline = time.monotonic() + float(seconds)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 160, 0, 0))
    proc = subprocess.Popen(
        [binary, *watch_args],
        stdin=slave,
        stdout=slave,
        stderr=subprocess.PIPE,
        start_new_session=True,
        close_fds=True,
    )
    os.close(slave)
    with open(pid_path, "w", encoding="ascii") as pid_file:
        pid_file.write(str(proc.pid))
    err_fd = proc.stderr.fileno()
    live = {master, err_fd}
    screen = open(screen_path, "wb")
    err = open(err_path, "wb")
    exited_early = False
    try:
        while time.monotonic() < deadline and not os.path.exists(stop_path):
            if proc.poll() is not None:
                exited_early = True
                break
            if not live:
                time.sleep(0.05)
                continue
            readable, _, _ = select.select(list(live), [], [], 0.1)
            take(readable, screen, err, master, live)
        drain(live, screen, err, master, 0.5)
    finally:
        reap(proc)
        screen.close()
        err.close()
        os.close(master)
    if exited_early:
        sys.exit(3)

if __name__ == "__main__":
    main()
"#;
