# Roadmap

hermon-rs is being built milestone by milestone as a greenfield Rust rewrite
of [`hermon.py`](../hermon.py), in this same repository so every issue and PR
stays connected to the Python implementation it replaces. This page is the
public source of truth for where that stands — it replaces an internal
planning doc that isn't tracked here.

Full detail (issues, acceptance criteria) lives on the
[GitHub milestones](https://github.com/DrazThan/hermon/milestones).

| Milestone | Status | Scope |
|---|---|---|
| [M1 — sources + roster, headless](https://github.com/DrazThan/hermon/milestone/1) | ✅ done | Port the data layer and `hermon ls`. No TUI. Roster output matches the Python implementation. |
| [M2 — TUI shell: list mode](https://github.com/DrazThan/hermon/milestone/2) | ✅ done | Engine thread, ratatui app, dense list rows with live tail summary, preview pane, fleet totals. |
| [M3 — session tail panes](https://github.com/DrazThan/hermon/milestone/3) | ✅ done | Three pure renderers, three tailers, grid mode, zoom, scrollback. |
| [M4 — lifecycle, liveness & attention states](https://github.com/DrazThan/hermon/milestone/4) | ✅ done | `lsof` liveness, linger/resurrect, eviction; PermWait and Stuck surfaced in the UI. |
| [M5 — fleet controls](https://github.com/DrazThan/hermon/milestone/5) | ✅ done | Sort, filter, pin, paging, attention-sort, palette overlay. |
| [M6 — notifications](https://github.com/DrazThan/hermon/milestone/6) | 🚧 in progress | `decide_alerts`/`AlertHistory` pure core is in ([#43](https://github.com/DrazThan/hermon/issues/43)); delivery (`osascript`), mute key and CLI flags are still open ([#44](https://github.com/DrazThan/hermon/issues/44)). |
| [M7 — polish & packaging](https://github.com/DrazThan/hermon/milestone/7) | 🚧 in progress | README + this roadmap ([#45](https://github.com/DrazThan/hermon/issues/45), this ticket), release build + brew tap ([#46](https://github.com/DrazThan/hermon/issues/46)), retiring `hermon.py` ([#47](https://github.com/DrazThan/hermon/issues/47)). |

## Sequencing notes

- M6 and M7 are the only milestones with open work. #45 (this ticket) is
  blocked on #44 landing first, since the README documents shipped behavior
  only.
- #46 (packaging) and #47 (retiring the Python implementation) are each their
  own ticket, explicitly out of scope here — see the "Python version" note in
  the [README](../README.md#python-version).
