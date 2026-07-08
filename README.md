# hermon

**Live monitor deck for Hermes and Claude Code agent sessions.**
*Hermes + monitor — and the mountain.*

A CLI companion for devs working with [Hermes](https://github.com/NousResearch/hermes-agent):
one terminal window you drag onto a spare monitor, where every agent session
running on your machine — Hermes TUI/CLI/gateway sessions, sub-agents, small
one-shot calls, `claude -p` invocations, calls to any provider — appears live
in its own pane. The window **splits when a session starts and unsplits when
it ends** (after a configurable linger, default 60s).

*(demo GIF goes here)*

## How it captures sessions

hermon is read-only and needs zero changes to Hermes or your orchestration.
It watches the places sessions already leave a live trail:

| Source | What it captures |
|---|---|
| `~/.hermes/state.db` (`sessions` + `messages`, WAL SQLite) | every Hermes session: TUI, CLI, gateway, sub-agents — any provider. Model, tool calls, tokens, cost, live-written mid-session |
| `~/.claude/projects/**/*.jsonl` | every Claude Code session: interactive or `claude -p`, including ones Hermes spawns as subprocesses |
| `~/.hermes/logs/agent.log` | per-API-call ticker in the roster (model, provider, tokens, latency) — catches small/auxiliary calls too |

It never sends input to sessions and never kills them.

## Install

Single file, Python 3.9+ stdlib only. Runtime binaries: `tmux` (required),
`lsof` (optional — sharper liveness for Claude transcripts).

## Quickstart (iTerm2)

```bash
# 1. start the daemon (any terminal, or launchd/background)
python3 hermon.py watch &

# 2. attach as one native iTerm2 window — drop it on a monitor
tmux -CC attach -t hermon
```

Under iTerm2's `-CC` control mode the deck is one native window whose split
panes appear and disappear as sessions come and go. Plain `tmux attach -t
hermon` works in any terminal.

The top pane is a **roster**: every recent session with state (● live /
✓ done), model, last tool, cumulative tokens, cost, and elapsed time, plus a
ticker of the last few raw Hermes API calls. Each other pane tails one
session, labeled on its border: `C:0f865f` (Claude) / `H:b356d8` (Hermes),
`✓`-prefixed once finished.

## CLI

```
hermon watch  [--session NAME] [--interval SEC] [--fresh-window SEC]
              [--idle-timeout SEC] [--linger SEC] [--max-panes N]
              [--claude-root DIR] [--hermes-db PATH] [--hermes-log PATH]
              [--no-claude] [--no-hermes]
hermon render FILE [--replay-bytes N]          # tail a Claude transcript
hermon render --hermes SESSION_ID              # tail a Hermes session
hermon render --summary                        # the roster (used by pane 0)
hermon ls                                      # roster once, to stdout, no tmux
```

Defaults make `hermon watch` correct with zero flags.

| Flag | Default | Meaning |
|---|---|---|
| `--idle-timeout` | `60` | no activity for this long ⇒ session finished |
| `--linger` | `60` | finished panes stay this long before unsplitting; `0` = keep forever |
| `--max-panes` | `8` | pane cap; finished panes are evicted first, the roster always lists everything |
| `--fresh-window` | `300` | roster lookback for recently-finished sessions |
| `--interval` | `1` | scan interval, seconds |

Why 60s linger: panes share one screen (unlike windows), so a dead pane
squeezes the live ones — but a `claude -p` that ran for ten seconds should
survive long enough to glance at. If a session resumes after its pane was
unsplit, the pane comes back.

## What a session pane shows

| Event | Rendering |
|---|---|
| assistant text | plain wrapped text |
| tool call | `▶ ToolName {…first 120 chars of args}` (dim) |
| tool result | `◀ toolname …first 200 chars` (dim); `◀ ERROR …` in red |
| usage / cost | `Σ in:<n> out:<n> $<cost>` |
| user / orchestrator prompt | `» user: …` |
| anything unrecognized | a single dim `· <type>` line |

Neither the Claude transcript format nor the Hermes schema is a stable public
API, so both parsers are defensive: malformed lines become a dim
`· parse-skip` marker, unknown shapes a `· <type>` line — never a crash,
never a raw JSON dump. The `state.db` is opened read-only
(`file:…?mode=ro`), safe alongside a running Hermes (WAL).

## Liveness

A session is **live** when it shows recent activity: for Hermes, a message
row or `ended_at` within `--idle-timeout` (Hermes doesn't always set
`ended_at`, so idleness is the fallback); for Claude transcripts, a fresh
mtime or a process holding the file open *for writing* (via `lsof` — hermon's
own read-only panes don't count). Finished ⇒ pane marked `✓`, unsplit after
`--linger`, resurrected if the session resumes.

The daemon is idempotent: restart it and it re-adopts existing panes by
title instead of duplicating them, and it recreates the tmux session if you
kill it externally.

## Tests

```bash
python3 -m unittest discover -s tests
```

Fixtures only — synthetic transcripts and a fixture SQLite db; no tmux, no
real sessions.

## Non-goals (v1)

Not a session manager (read-only), no web UI, no persistence or analytics,
no Windows (macOS primary, Linux best-effort).

## License

MIT — see [LICENSE](LICENSE).
