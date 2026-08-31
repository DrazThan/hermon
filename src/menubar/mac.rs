//! The macOS tray backend: an NSStatusItem via `tray-icon`, driven by
//! `tao`'s event loop (its required host — see the crate's platform notes).
//! The engine runs on its own thread exactly as `watch` does
//! ([`crate::ui::run_tui`]); this loop drains [`Event::Roster`] and keeps
//! the status item's title in sync. #71's dropdown menu is built via the pure
//! `build_menu` function; wiring it to the tray API is follow-up work with
//! version-specific muda integration.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use tao::event::{Event as TaoEvent, StartCause};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::TrayIcon;
use tray_icon::TrayIconBuilder;

use crate::config::EngineConfig;
use crate::engine::{Engine, Event, UiCmd};

use super::format_title;

/// How often the callback checks for a fresh roster — well under the ~2s
/// the acceptance criteria asks the status item to react within.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub fn run(config: EngineConfig) -> anyhow::Result<()> {
    let (event_tx, event_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let engine = Engine::spawn(config, event_tx, cmd_rx);
    let mut engine = Some(engine);

    // Accessory: no Dock icon, no Cmd-Tab entry — a menu-bar-only app.
    let mut event_loop = EventLoop::new();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let mut tray: Option<TrayIcon> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL);
        match event {
            // Created only once the loop is actually running, matching
            // tray-icon's own guidance (creating it any earlier has shown up
            // as a blank item on macOS).
            TaoEvent::NewEvents(StartCause::Init) => {
                tray = match TrayIconBuilder::new().with_title(format_title(&[])).build() {
                    Ok(icon) => Some(icon),
                    Err(err) => {
                        eprintln!("hermon menubar: failed to create tray icon: {err}");
                        None
                    }
                };
            }
            TaoEvent::MainEventsCleared => {
                for engine_event in event_rx.try_iter() {
                    if let Event::Roster(rows) = engine_event
                        && let Some(tray) = &tray
                    {
                        tray.set_title(Some(format_title(&rows)));
                    }
                }
            }
            // Fires once, right before the process exits, once something
            // sets `control_flow` to `Exit` — nothing does yet (#71's Quit
            // menu item is the trigger), but the shutdown path is wired for
            // it: tell the engine to stop and wait for its thread.
            TaoEvent::LoopDestroyed => {
                let _ = cmd_tx.send(UiCmd::Shutdown);
                if let Some(handle) = engine.take() {
                    let _ = handle.join();
                }
            }
            _ => {}
        }
    })
}
