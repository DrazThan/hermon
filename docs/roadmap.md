# Roadmap

hermon-rs was built milestone by milestone as a greenfield Rust rewrite of
`hermon.py`, in this same repository so every issue and PR stayed connected to
the Python implementation it replaces. That implementation has since been
retired — see
[Predecessor](../packaging/RELEASE_NOTES.md#7-predecessor-the-python-implementation)
for how to reach it. This page is the public source of truth for where the
milestones stand — it replaces an internal planning doc that isn't tracked
here.

Full detail (issues, acceptance criteria) lives on the
[GitHub milestones](https://github.com/DrazThan/hermon/milestones).

| Milestone | Status | Scope |
|---|---|---|
| [M1 — sources + roster, headless](https://github.com/DrazThan/hermon/milestone/1) | ✅ done | Port the data layer and `hermon ls`. No TUI. Roster output matches the Python implementation. |
| [M2 — TUI shell: list mode](https://github.com/DrazThan/hermon/milestone/2) | ✅ done | Engine thread, ratatui app, dense list rows with live tail summary, preview pane, fleet totals. |
| [M3 — session tail panes](https://github.com/DrazThan/hermon/milestone/3) | ✅ done | Three pure renderers, three tailers, grid mode, zoom, scrollback. |
| [M4 — lifecycle, liveness & attention states](https://github.com/DrazThan/hermon/milestone/4) | ✅ done | `lsof` liveness, linger/resurrect, eviction; PermWait and Stuck surfaced in the UI. |
| [M5 — fleet controls](https://github.com/DrazThan/hermon/milestone/5) | ✅ done | Sort, filter, pin, paging, attention-sort, palette overlay. |
| [M6 — notifications](https://github.com/DrazThan/hermon/milestone/6) | ✅ done | `decide_alerts`/`AlertHistory` pure core ([#43](https://github.com/DrazThan/hermon/issues/43)); delivery (`osascript`), mute key and CLI flags ([#44](https://github.com/DrazThan/hermon/issues/44)). |
| [M7 — polish & packaging](https://github.com/DrazThan/hermon/milestone/7) | ✅ done | README + this roadmap ([#45](https://github.com/DrazThan/hermon/issues/45)), release build + brew tap ([#46](https://github.com/DrazThan/hermon/issues/46)), retiring `hermon.py` ([#47](https://github.com/DrazThan/hermon/issues/47)). |
| [M8 — menu bar launch-at-login](https://github.com/DrazThan/hermon/milestone/8) | ✅ done | `--install-login-item` / `--uninstall-login-item` flags on `hermon menubar` to register with launchd ([#73](https://github.com/DrazThan/hermon/issues/73)); formula update, README docs. |
| [M9 — Hermon.app: bundle, icon, release CI, brew cask](https://github.com/DrazThan/hermon/milestone/9) | ✅ done | GUI shell (`hermon gui`, [#74](https://github.com/DrazThan/hermon/issues/74)–[#77](https://github.com/DrazThan/hermon/issues/77)) ships as `Hermon.app`: `.icns` from the repo logo, `cargo-bundle` producing a Dock app that launches `gui`, release CI attaching a zipped bundle, `Casks/hermon-app.rb` in the tap alongside the CLI formula ([#78](https://github.com/DrazThan/hermon/issues/78)). Unsigned for now — README documents the right-click-open first launch. |
| [M10 — remote agents: containers, hosts](https://github.com/DrazThan/hermon/milestone/10) | ✅ done | `hermon agent` streams sessions over stdio to a host `hermon` ([#88](https://github.com/DrazThan/hermon/issues/88)–[#89](https://github.com/DrazThan/hermon/issues/89)); host-side `RemoteSource` treats the stream as untrusted input — framed, capped, sanitized ([#90](https://github.com/DrazThan/hermon/issues/90), [#95](https://github.com/DrazThan/hermon/issues/95)); `--remote docker:`/`ssh:`/`cmd:` and `--docker-auto` label discovery attach one ([#91](https://github.com/DrazThan/hermon/issues/91), [#92](https://github.com/DrazThan/hermon/issues/92)); release CI cross-compiles static `x86_64`/`aarch64-unknown-linux-musl` agent binaries and attaches them to the release, README documents the architecture, transports, version/clock-skew UX, and security posture ([#93](https://github.com/DrazThan/hermon/issues/93)). |

## Sequencing notes

- Every milestone is closed. M8 brings launch-at-login support to the menu bar
  via `--install-login-item` / `--uninstall-login-item`, shipping with a formula
  update in the tap and README documentation. M9 closes out the desktop app:
  `Hermon.app` wraps the M8-era `hermon gui` window in a proper macOS bundle,
  installable via a new cask that sits next to the existing CLI formula.
  Signing/notarization stayed out of scope — right-click-open is the documented
  workaround, and a paid Developer ID is called out as future work.
- The Rust binary is now the only implementation. The Python one is reachable
  at the `python-final` tag — see
  [Predecessor](../packaging/RELEASE_NOTES.md#7-predecessor-the-python-implementation).
