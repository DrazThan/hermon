# hermon

**Live monitor deck for Hermes, Claude Code, and OpenCode agent sessions.**
*Hermes + monitor — and the mountain.*

A terminal UI for devs working with [Hermes](https://github.com/NousResearch/hermes-agent):
one window you drag onto a spare monitor, where every agent session running
on your machine — Hermes TUI/CLI/gateway sessions, sub-agents, small one-shot
calls, `claude -p` invocations, `opencode run` invocations, calls to any
provider — shows up live, with its own tail pane.

hermon is read-only and needs zero changes to Hermes or your orchestration:
it watches the on-disk stores those tools already write to, so nothing has to
be instrumented, wrapped, or run through hermon to be seen. It never sends
input to a session and never kills one.

*(screenshot / asciinema link goes here — recorded after packaging, #46)*

## How it captures sessions

hermon watches the places sessions already leave a live trail — including
the stores of tools Hermes shells out to, not just Hermes itself:

| Source | Key prefix | Store | What it captures |
|---|---|---|---|
| Claude Code | `C:` | `~/.claude/projects/**/*.jsonl` | every Claude Code session: interactive or `claude -p`, including ones Hermes spawns as subprocesses |
| Hermes | `H:` | `~/.hermes/state.db` (`sessions` + `messages`, WAL SQLite) | every Hermes session: TUI, CLI, gateway, sub-agents — any provider. Model, tool calls, tokens, cost, live-written mid-session |
| OpenCode | `O:` | `~/.local/share/opencode/opencode.db` (`session`/`message`/`part`, WAL SQLite) | every OpenCode CLI session: `opencode run` or interactive, including ones Hermes spawns as subprocesses |
| API ticker | — | `~/.hermes/logs/agent.log` | per-API-call ticker in the roster (model, provider, tokens, latency) — catches small/auxiliary calls too |

Each store path is overridable per subcommand: `--claude-dir`, `--hermes-db`,
`--opencode-db`, `--hermes-log`. Both SQLite stores are opened read-only
(`file:…?mode=ro`), safe alongside the real tool running (WAL).

## Install

```bash
brew tap drazthan/hermon
brew install hermon
```

The tap builds from source against Homebrew's `rust`, so the first install
spends about half a minute compiling; there are no bottles or prebuilt
binaries yet.

From a checkout instead:

```bash
cargo install --path .
```

Either way `hermon --version` should print `hermon 0.1.0`.

## Quickstart

```bash
# the roster + tail panes in one self-contained TUI (no tmux)
hermon watch

# the roster once, to stdout, for scripting or a quick glance
hermon ls

# tail one session's transcript to stdout until Ctrl-C
hermon render C:0f865f   # key from `hermon ls`
```

`hermon watch` is the whole app: a ratatui screen with a roster and live tail
panes, redrawn from an engine thread polling all three stores. There's no
tmux involved — that was the Python version's mechanism; see
[Python version](#python-version).

## Views and keybindings

`hermon watch` has two view modes, `l` toggles between them:

- **List** (default) — one dense row per session, with a preview pane for
  the selected session, fleet totals in the footer.
- **Grid** — the roster plus a wall of tiled live panes (up to 6 at once;
  `Tab` pages through the rest). `Enter`/`z` zooms the selected pane to full
  size.

| Key | View | Action |
|---|---|---|
| `q`, `Ctrl-C` | any | Quit |
| `j`/`↓`, `k`/`↑` | any | Select next / previous session |
| `l` | any | Toggle list / grid |
| `s` | any | Open the sort palette |
| `f` | any | Open the filter palette |
| `a` | any | Toggle attention-first grouping |
| `p` | any | Pin / unpin the selected session (held panes survive `--max-panes` eviction) |
| `c` | any | Clear sort + filter |
| `?` | any | Toggle the help overlay |
| `Tab` | grid | Next page of tiles |
| `Enter`, `z` | grid | Zoom the selected pane |
| `Esc` | grid, zoomed | Leave zoom |
| `PageUp`/`PageDown` | grid | Scroll the selected pane's scrollback |
| `g` / `G` | grid | Jump to oldest / back to the tail |
| `x` / `o` | grid | Close / reopen the selected pane |

Sort/filter palette (open with `s`/`f`; `Esc` cancels, `Enter` commits):

| Key | Focus | Action |
|---|---|---|
| `1`-`5` | sort | Pick sort key (model / tool / in-out tokens / cost / elapsed); pressing the active one flips direction |
| `c` | sort | Clear sort + filter |
| any character, `Backspace` | filter | Edit the filter text |

The header shows chips only when something's active: an error-styled
`📌 N over --max-panes` chip if pinned sessions exceed `--max-panes`, the
current sort key and direction, one chip per filter term, and a
`N/M shown` count on the right.

## Notifications

`hermon watch` fires a desktop banner when a session finishes a clean turn,
hits an error, gets stuck mid-tool-call, or sits waiting on a permission
prompt. Delivery degrades gracefully depending on what's on `PATH`, probed
once at startup:

1. [`terminal-notifier`](https://github.com/julienXX/terminal-notifier) —
   richest option: a real app icon and `-sound`.
2. `osascript -e 'display notification …'` — ships with every Mac, no
   install required. **Caveat:** banners delivered this way show a generic
   script-editor icon, since `osascript` has no notion of a custom app icon;
   install `terminal-notifier` (`brew install terminal-notifier`) if that
   bothers you.
3. `notify-send` on Linux.
4. Silent no-op if none of the above is found — alerts are still decided and
   logged, just never shown.

A banner never blocks the monitor: it's fired with a plain spawn-and-forget,
so a slow or hung notifier can't stall a scan tick.

`[m]` in the TUI mutes every banner for the rest of the session (shown as
🔕 in the footer); it doesn't touch the roster or panes. Flags, for scripting
or turning individual alert kinds off:

| Flag | Default | Meaning |
|---|---|---|
| `--no-notify` | off | skip desktop notifications entirely |
| `--notify-cooldown` | `120` | seconds before the same session/kind can alert again |
| `--no-notify-turn-done` | off | don't alert when a session finishes a clean turn |
| `--no-notify-stuck` | off | don't alert when a tool call looks wedged |
| `--no-notify-perm-wait` | off | don't alert when a session is waiting on a permission prompt |
| `--no-notify-error` | off | don't alert on an observed error line |

## What a session pane shows

| Event | Rendering |
|---|---|
| assistant text | plain wrapped text |
| tool call | `▶ ToolName {…first 120 chars of args}` (dim) |
| tool result | `◀ toolname …first 200 chars` (dim); `◀ ERROR …` in red |
| usage / cost | `Σ in:<n> out:<n> $<cost>` |
| user / orchestrator prompt | `» user: …` |
| anything unrecognized | a single dim `· <type>` line |

For OpenCode, the same shape maps onto its `part` rows instead of a flat
message stream: a `tool` part starts as `▶ tool {input}` and is *updated in
place* (not a new row) once it completes, so hermon detects that status
change and appends `◀ result`/`◀ ERROR` — both lines appear together if the
tool finished between two polls. `text`/`reasoning`/`file`/`patch`/`step-*`
parts render once, on first sight.

None of the three schemas (Claude transcript, Hermes DB, OpenCode DB) is a
stable public API, so all three parsers are defensive: malformed rows become
a dim `· parse-skip` marker, unknown shapes a `· <type>` line — never a
crash, never a raw JSON dump.

A freshly opened pane seeds its scrollback from `--replay-bytes` (file-backed
sources — Claude transcripts) or `--replay-lines` (DB-backed sources —
Hermes, OpenCode); the other budget is ignored by whichever kind of store
backs that pane.

## Liveness & attention

Every session is one of:

| State | Glyph (ASCII fallback) | Meaning |
|---|---|---|
| Live | `●` (`*`) | actively producing output, or a turn genuinely in progress |
| Attention → PermWait | `⏸` (`||`) | a tool call has sat unanswered past the permission-prompt threshold — probably waiting on a human |
| Attention → Stuck | `⚠` (`!`) | a tool call has been "running" long enough to be presumed wedged, but still fresh enough to surface |
| Done | `✓` (`.`) | the session finished |

Set `HERMON_ASCII=1` to force the ASCII glyph set (e.g. a font without them).

For Claude transcripts: **live** on a fresh mtime, or a process holding the
file open *for writing* (via `lsof` — hermon's own read-only panes don't
count).

For Hermes and OpenCode, an explicit "session closed" flag (`ended_at`,
`time_archived`) is set rarely in practice — interactive sessions routinely
sit for hours between turns without it ever being set — so it can't be the
primary signal. hermon rides on each tool's own turn-completion signal
instead: Hermes's last message `finish_reason`, OpenCode's last message
`finish` (`'tool-calls'` / `'stop'`) — same shape, same classifier:

- a clean stop with no pending tool call ⇒ the assistant closed its turn and
  is idle waiting on the next user message — **done immediately**, no
  timeout wait.
- a pending tool call ⇒ a tool (shell command, web fetch, sub-agent) is
  actually *running* and can legitimately take minutes without a new
  message row appearing — **live**, with a generous ceiling (`5 ×
  --idle-timeout`) against a truly orphaned turn.
- anything else mid-turn (a tool result awaiting the assistant's next
  completion, a user message not yet answered) should resolve within
  normal API latency — **live**, but with the tighter `--idle-timeout`
  ceiling; a multi-minute gap there is genuinely suspicious.
- the explicit closed flag, when it is set ⇒ done, always.

Either way, finished sessions stay on the roster for `--fresh-window`, and a
finished pane in grid mode closes after `--linger` seconds unless pinned —
if the session resumes, its pane comes back.

## Notifications

Attention states surface live in the TUI today: the roster glyph, a pane's
border color, and (in grid mode) a status line under an attention pane's
transcript (`⏸ waiting on permission prompt · <elapsed>` /
`⚠ tool pending <elapsed> — no output`).

Desktop notifications (`osascript`, a mute key, cooldowns per session/kind)
have their decision core built and unit-tested (`decide_alerts`,
`AlertHistory`) but aren't wired to actual delivery or CLI flags yet — that's
[#44](https://github.com/DrazThan/hermon/issues/44), tracked under
[M6](docs/roadmap.md).

## CLI reference

```
hermon watch  [--claude-dir DIR] [--hermes-db PATH] [--opencode-db PATH]
              [--hermes-log PATH] [--idle-timeout SEC] [--interval SEC]
              [--max-panes N] [--linger SEC] [--replay-bytes N] [--replay-lines N]
hermon ls     [...same source flags as watch...] [--fresh-window SEC]
hermon render KEY [...same source flags as watch...] [--fresh-window SEC]
```

Defaults make `hermon watch` correct with zero flags.

| Flag | Default | Meaning |
|---|---|---|
| `--claude-dir` | `~/.claude/projects` | Claude Code transcript root |
| `--hermes-db` | `~/.hermes/state.db` | Hermes state.db path |
| `--opencode-db` | `~/.local/share/opencode/opencode.db` | OpenCode opencode.db path |
| `--hermes-log` | `~/.hermes/logs/agent.log` | Hermes agent.log (roster API-call ticker) |
| `--idle-timeout` | `180` | safety ceiling for a session stuck mid-turn with no activity |
| `--interval` | `1` | roster scan interval, seconds |
| `--max-panes` | `8` | pane cap (grid mode); finished panes are evicted first, the roster always lists everything |
| `--linger` | `60` | finished panes stay this long before closing; `0` = keep forever |
| `--replay-bytes` | `20480` | history a freshly opened pane replays from a file-backed source (Claude) |
| `--replay-lines` | `40` | history a freshly opened pane replays from a DB-backed source (Hermes, OpenCode) |
| `--fresh-window` (`ls`, `render` only) | `3600` | roster lookback for recently-finished sessions; `watch` isn't given this flag and keeps the tighter 300s Python default |

`hermon ls` and `hermon render` honor `NO_COLOR` (and fall back to plain text
automatically when stdout isn't a terminal); the ratatui screen (`hermon
watch`) always renders in color — use `HERMON_ASCII=1` there instead if you
need a plainer glyph set.

Run any subcommand with `--help` for the exact flags and defaults shipped in
this build, or `hermon --version` for the build version.

## Adding another source

Hermes shells out to more than these three tools, and each one tends to keep
its own session store the same way Claude Code and OpenCode do — hermon
watches that store directly rather than going through Hermes. Two traits in
[`src/source/mod.rs`](src/source/mod.rs) are the whole contract:

```rust
pub trait Source {
    fn sessions(&mut self) -> Vec<SessionMeta>;
    fn last_tool(&mut self, session_id: &str) -> String;
    fn open_tailer(&self, session_id: &str, replay: Replay) -> Option<Box<dyn Tailer>> { None }
}

pub trait Tailer {
    fn poll(&mut self) -> Vec<StyledLine>;
}
```

Adding a tool is:

1. Implement `Source` for it — `sessions()` returns the shared
   `SessionMeta` shape (id, timestamps, model, tokens, cost, `turn_done`,
   `tool_pending`, …); `classify()` (`src/source/mod.rs`) then gives you
   live/attention/done for free. `ClaudeSource`/`HermesSource`/
   `OpenCodeSource` (`src/source/{claude,hermes,opencode}.rs`) are the
   templates — pick whichever is closer to the new store's shape (flat file
   vs. row-per-message SQLite with a turn-completion signal).
2. Implement `Tailer` for its live view, and a renderer turning one raw row
   into `StyledLine`s (`src/render/{claude,hermes,opencode}.rs` are the
   templates; `hermon render KEY` is the parity harness these are diffed
   against).
3. Add a field to `Sources` and a match arm in its `open_tailer`
   (`src/roster.rs`), and a loop over the new source in `build_roster` —
   that's the entire touch-point list now, versus the Python version's five.

If the tool has no turn-completion signal at all, it still works via the
flat `now - last_ts <= idle_timeout` fallback (like Claude transcripts) —
just less precise about mid-turn silence.

## Non-goals

Not a session manager — read-only, never sends input, never kills a session.
No web UI, no persistence or analytics beyond what's on screen. No Windows
(macOS primary, Linux best-effort); no plans to change that.

## Python version

`hermon.py` at the repo root is the original implementation this rewrite
replaces — same read-only model, but driving real tmux panes instead of a
built-in TUI. It's kept working (and its test suite green) until parity is
signed off; removing it is tracked in
[#47](https://github.com/DrazThan/hermon/issues/47). Don't build new features
on it.

## Tests

```bash
cargo test
```

Fixtures only — synthetic transcripts and fixture SQLite DBs matching each
tool's real schema; nothing here touches a real session or a real tmux/TUI
session.

## Roadmap

See [docs/roadmap.md](docs/roadmap.md) for the milestone-by-milestone status
and links to the GitHub milestones.

## License

MIT — see [LICENSE](LICENSE).
