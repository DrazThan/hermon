//! The macOS tray backend: an NSStatusItem via `tray-icon`, driven by
//! `tao`'s event loop (its required host — see the crate's platform notes).
//! The engine runs on its own thread exactly as `watch` does
//! ([`crate::ui::run_tui`]); this loop drains [`Event::Roster`] and keeps the
//! status item's title, icon, and dropdown in sync.
//!
//! The dropdown is `muda`'s, built by mapping #71's pure [`build_menu`] rows
//! onto menu items. It is rebuilt only when those rows actually change, both
//! to avoid rebuilding an NSMenu every tick and because replacing the menu
//! while the user has it open would dismiss it — session labels are minute-
//! granular, so a quiet fleet leaves the menu alone entirely.

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tao::event::{Event as TaoEvent, StartCause};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::arbitration::{self, PidGuard, UiKind};
use crate::config::EngineConfig;
use crate::engine::{Engine, Event, UiCmd};
use crate::roster::RosterRow;

use super::{
    ICON_PX, IconState, MenuRow, build_menu, icon_rgba, icon_state, menu_label, status_title,
};

/// How often the callback checks for a fresh roster — well under the ~2s
/// the acceptance criteria asks the status item to react within.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Ids for the three actionable rows. Menus are rebuilt wholesale, so the
/// handler matches on these rather than on retained item handles.
const ID_MUTE: &str = "hermon:mute";
const ID_OPEN: &str = "hermon:open";
const ID_QUIT: &str = "hermon:quit";

pub fn run(config: EngineConfig) -> anyhow::Result<()> {
    // The pidfile claims the notifier role, not merely "menubar is running":
    // a `menubar --no-notify` must leave `watch` notifying (#72).
    let pidfile = config.notify.any_enabled().then(claim_notifier).flatten();

    let (event_tx, event_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let engine = Engine::spawn(config, event_tx, cmd_rx);
    let mut engine = Some(engine);
    let mut pidfile = pidfile;

    // Accessory: no Dock icon, no Cmd-Tab entry — a menu-bar-only app.
    let mut event_loop = EventLoop::new();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let mut tray: Option<TrayIcon> = None;
    let mut rows: Vec<RosterRow> = Vec::new();
    // The menubar's own mute, mirroring what the engine's `AlertHistory`
    // holds. Per-process by design: the TUI's `[m]` does not reach here.
    let mut muted = false;
    let mut shown: Vec<MenuRow> = Vec::new();
    let mut icon: Option<IconState> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL);
        match event {
            // Created only once the loop is actually running, matching
            // tray-icon's own guidance (creating it any earlier has shown up
            // as a blank item on macOS).
            TaoEvent::NewEvents(StartCause::Init) => {
                shown = build_menu(&rows, muted);
                icon = Some(icon_state(&rows, muted));
                let mut builder = TrayIconBuilder::new().with_title(status_title(&rows, muted));
                if let Some(menu) = menu_from(&shown) {
                    builder = builder.with_menu(Box::new(menu));
                }
                if let Some(icon) = icon.and_then(build_icon) {
                    builder = builder.with_icon(icon);
                }
                tray = match builder.build() {
                    Ok(item) => Some(item),
                    Err(err) => {
                        eprintln!("hermon menubar: failed to create tray icon: {err}");
                        None
                    }
                };
            }
            TaoEvent::MainEventsCleared => {
                let mut fresh = false;
                for engine_event in event_rx.try_iter() {
                    if let Event::Roster(new_rows) = engine_event {
                        rows = new_rows;
                        fresh = true;
                    }
                }

                for menu_event in MenuEvent::receiver().try_iter() {
                    match menu_event.id.as_ref() {
                        ID_MUTE => {
                            muted = !muted;
                            if cmd_tx.send(UiCmd::SetMuted(muted)).is_err() {
                                *control_flow = ControlFlow::Exit;
                            }
                            fresh = true;
                        }
                        ID_OPEN => open_watch(),
                        ID_QUIT => *control_flow = ControlFlow::Exit,
                        // Session and summary rows are readouts, not actions.
                        _ => {}
                    }
                }

                if fresh && let Some(tray) = &tray {
                    tray.set_title(Some(status_title(&rows, muted)));

                    let wanted = build_menu(&rows, muted);
                    if wanted != shown {
                        tray.set_menu(menu_from(&wanted).map(|m| Box::new(m) as _));
                        shown = wanted;
                    }

                    let wanted = icon_state(&rows, muted);
                    if icon != Some(wanted) {
                        if let Some(built) = build_icon(wanted) {
                            let _ = tray.set_icon(Some(built));
                        }
                        icon = Some(wanted);
                    }
                }
            }
            // Fires once, right before the process exits, once the Quit item
            // sets `control_flow` to `Exit`: tell the engine to stop, wait for
            // its thread, and drop the notifier claim. The claim is released
            // by hand because the event loop exits the process outright —
            // nothing here unwinds, so `PidGuard`'s destructor never runs.
            TaoEvent::LoopDestroyed => {
                let _ = cmd_tx.send(UiCmd::Shutdown);
                if let Some(handle) = engine.take() {
                    let _ = handle.join();
                }
                if let Some(guard) = &mut pidfile {
                    guard.release();
                }
            }
            _ => {}
        }
    })
}

fn claim_notifier() -> Option<PidGuard> {
    let dir = arbitration::runtime_dir()?;
    match arbitration::claim(&dir, UiKind::Menubar) {
        Ok(guard) => Some(guard),
        Err(err) => {
            // Not fatal: the menubar still notifies, a concurrent `watch`
            // just won't know to stand down.
            eprintln!("hermon menubar: could not claim the notifier pidfile: {err}");
            None
        }
    }
}

/// Maps the pure [`MenuRow`] list onto a `muda` menu. `None` if the menu API
/// rejects an item, which leaves the status item menu-less rather than
/// half-built.
fn menu_from(items: &[MenuRow]) -> Option<Menu> {
    let menu = Menu::new();
    for item in items {
        let label = menu_label(item);
        let appended = match item {
            MenuRow::Separator => menu.append(&PredefinedMenuItem::separator()),
            MenuRow::MuteToggle { muted } => {
                menu.append(&CheckMenuItem::with_id(ID_MUTE, label, true, *muted, None))
            }
            MenuRow::OpenWatch => menu.append(&MenuItem::with_id(ID_OPEN, label, true, None)),
            MenuRow::Quit => menu.append(&MenuItem::with_id(ID_QUIT, label, true, None)),
            // Readouts: shown, never clickable.
            _ => menu.append(&MenuItem::new(label, false, None)),
        };
        if let Err(err) = appended {
            eprintln!("hermon menubar: failed to build the dropdown: {err}");
            return None;
        }
    }
    Some(menu)
}

fn build_icon(state: IconState) -> Option<Icon> {
    Icon::from_rgba(icon_rgba(state), ICON_PX, ICON_PX).ok()
}

/// Opens this same binary's TUI in Terminal.app — `open -a` cannot pass a
/// subcommand, so it goes through AppleScript. Fire-and-forget, like
/// [`crate::notify::deliver`]: a failure costs the user a menu click, not the
/// menubar process.
fn open_watch() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let script = format!(
        "tell application \"Terminal\" to do script \"{} watch\"\n\
         tell application \"Terminal\" to activate",
        crate::notify::applescript_escape(&exe.display().to_string())
    );
    let _ = Command::new("osascript")
        .args(["-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
