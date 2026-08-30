# Parity signoff — issue #47 (retire `hermon.py`)

**Date:** 2026-08-29
**Branch:** `issue-47-retire-python` (based on `main` @ `e1a0428`)
**Verdict: ✅ SIGNOFF PASSES — deletion performed.**

This is the second pass. The first pass (against `main` @ `8624818`) **failed**
on a real functional divergence and, per the issue's own protocol ("If parity
signoff FAILS on something, that's a new bug issue"), refused to delete
anything. That became **#66**, fixed and merged as **PR #67** (`e1a0428`,
"Claude scan: skip subagents/ dirs for Python parity"). Everything below is
re-captured against the fixed binary.

---

## Summary

| # | Area | Divergence | Class | Status |
|---|------|-----------|-------|--------|
| 1 | `ls` session discovery | Rust counted Claude **subagent** transcripts as top-level sessions | **Was blocking** | ✅ **fixed in #66/#67 — verified gone** |
| 2 | `render` tool-call args | Rust emits compact, key-sorted JSON; Python emits spaced, insertion-ordered JSON | Cosmetic, undocumented | ⚠️ non-blocking, see below |
| 3 | `ls` cost column | Rust `0.0000` where Python `-` | Cosmetic, **documented** deviation | ✅ accepted |
| 4 | `ls` totals footer | Rust adds `N live · N done · Σ $X · Y in`; Python has no footer | Intentional Rust feature | ✅ accepted |
| 5 | `render` line breaking / whitespace | Rust emits one logical line per source line with internal whitespace collapsed; Python wraps to terminal width | Intentional Rust design, **documented** | ✅ accepted |
| 6 | `render` banner | Python prints `hermon · tailing <path>`; Rust prints none | CLI shape difference (key vs path) | ✅ accepted |

No functional divergence remains. Every difference below is either a
documented, intentional design choice or a cosmetic JSON-formatting artifact.

---

## ✅ Blocker #1 resolved — `hermon ls` vs `python3 hermon.py ls`

Captured back-to-back, same second (`23:21:18` / `23:21:19`):

```
=== python3 hermon.py ls ===
hermon · 9 session(s) · 23:21:19
  id        model                   last tool                 in      out     cost  elapsed  title
● C:ba007c  claude-opus-5           Bash                 418,557    4,380        -      24s
✓ H:30cf4c  claude-fable-5          terminal          71,496,517  247,118   0.6181   33h11m  Review and merge PRs one by one #2
● C:e07967  claude-opus-5           Bash               1,141,010    9,569        -    2m00s
✓ C:643498  claude-fable-5          Bash                 133,512    3,470        -      19s
✓ C:b1db5f  claude-opus-5           Bash               5,960,003   69,435        -    9m55s
✓ C:fa7a49  claude-fable-5          Read                 134,692    8,702        -      46s
✓ C:c89dd6  claude-opus-5           Bash               3,912,327   46,427        -    7m16s
✓ C:2b32b1  claude-sonnet-5         Bash               2,572,330   16,906        -    1m41s
✓ C:374fb7  claude-sonnet-5         Bash              35,039,983  149,208        -   20m20s

  recent hermes API calls:
  23:20:53 30cf4c #277 deepseek-v4-flash@deepseek in=207,526 out=182 3.7s
  23:21:05 30cf4c #278 deepseek-v4-flash@deepseek in=207,742 out=305 3.4s
  23:21:08 30cf4c #279 deepseek-v4-flash@deepseek in=208,138 out=85 3.1s
  23:21:12 30cf4c #280 deepseek-v4-flash@deepseek in=208,285 out=179 3.5s

=== hermon ls (Rust) ===
hermon · 9 session(s) · 23:21:18
  id        model                   last tool                 in      out     cost  elapsed  title
● C:ba007c  claude-opus-5           Bash                 418,557    4,380   0.0000      24s
✓ H:30cf4c  claude-fable-5          terminal          71,496,517  247,118   0.6181   33h11m  Review and merge PRs one by one #2
● C:e07967  claude-opus-5           Bash               1,141,010    9,569   0.0000    2m00s
✓ C:643498  claude-fable-5          Bash                 133,512    3,470   0.0000      19s
✓ C:b1db5f  claude-opus-5           Bash               5,960,003   69,435   0.0000    9m55s
✓ C:fa7a49  claude-fable-5          Read                 134,692    8,702   0.0000      46s
✓ C:c89dd6  claude-opus-5           Bash               3,912,327   46,427   0.0000    7m16s
✓ C:2b32b1  claude-sonnet-5         Bash               2,572,330   16,906   0.0000    1m41s
✓ C:374fb7  claude-sonnet-5         Bash              35,039,983  149,208   0.0000   20m20s
2 live · 7 done · Σ $0.62 · 120,808,931 in

  recent hermes API calls:
  23:20:53 30cf4c #277 deepseek-v4-flash@deepseek in=207,526 out=182 3.7s
  23:21:05 30cf4c #278 deepseek-v4-flash@deepseek in=207,742 out=305 3.4s
  23:21:08 30cf4c #279 deepseek-v4-flash@deepseek in=208,138 out=85 3.1s
  23:21:12 30cf4c #280 deepseek-v4-flash@deepseek in=208,285 out=179 3.5s
```

**9 rows vs 9 rows**, same ids, same order, same models, tools, token counts,
elapsed times and titles. `diff` (trailing whitespace stripped) reduces
*entirely* to accepted items 3 and 4 plus the one-second clock skew — no row
appears on one side and not the other.

Session count re-checked on three further interleaved runs: `9`/`9`,
`9`/`9`, `9`/`9`.

**Totals now reconcile too.** Rust's footer reports `120,808,931 in`, which is
exactly the sum of the nine visible `in` values. In the failed first pass Rust
reported `127,485,986` against Python's `126,298,130` — the subagent
transcript's tokens double-counted on top of the parent session. That gap is
closed.

---

## Render parity — one session per source

Method: Rust captured first, Python second, both killed with
`perl -e 'alarm N; exec @ARGV'`; Python run with `COLUMNS=400` so its
`textwrap` wrapping stops dominating the diff. Replay depth pinned equal on
both sides. Python's banner line dropped before comparing (item 6).

Rather than eyeball the diffs, each pair was compared **mechanically**: strip
the `▶` tool-call lines (the only place item 2 lives), delete all whitespace
from what remains, and hash. Identical hashes mean the two implementations
emit the same content, same rows, same order.

| Source | Session | Non-tool content, whitespace-stripped | Tool-call lines |
|---|---|---|---|
| Claude | `C:2b32b1` | `ecf6abbe0204` == `ecf6abbe0204` ✅ | 1 vs 1, same name + args modulo item 2 |
| Hermes | `H:30cf4c` | 5,767 chars, byte-identical ✅ | none in window |
| OpenCode | `O:LSN6Vy` | `2d5a9abf7469` == `2d5a9abf7469` ✅ | 6 vs 6, same names + args modulo item 2 |

### Claude — `C:2b32b1`

```
rust:   hermon render C:2b32b1 --replay-bytes 8000
python: COLUMNS=400 python3 -u hermon.py render --replay-bytes 8000 \
          ~/.claude/projects/-Users-taloz-code-hermon-wt-issue-44-notify-delivery/\
          f5ace4b7-e8a4-4e32-bf94-3e420b2b32b1.jsonl
```

Rust output verbatim (5 display lines after the tool line; long lines elided
here with `[…]` for readability only — the hash above is over the full text):

```
▶ Bash {"command":"git add src/cli.rs src/config.rs src/lib.rs tests/engine.rs tests/lifecycle.rs tests/live_notify_check.rs &…
Σ in:73,410 out:962
◀ result On branch issue-44-notify-delivery Your branch is up to date with 'origin/issue-44-notify-delivery'. All conflicts fixed but you are still merging. (use "git commit" to conclude merge) Changes to be …
· attachment
All conflicts resolved, staged, not committed. Merge left in progress as instructed. ## Resolution summary **src/cli.rs** — kept both: #44's `NotifyCfg` import […]
Σ in:74,598 out:724
· last-prompt
```

Python emits the same seven logical units; the only diff hunks are the
tool-call line (item 2) and the assistant text block, which Python wraps to
398 columns and where Python leaves the source's `\n\n` paragraph breaks as
literal double spaces while Rust collapses whitespace runs (item 5, documented
at `src/render/claude.rs:7-10`).

### Hermes — `H:30cf4c` (`20260828_141000_30cf4c`)

```
rust:   hermon render H:30cf4c --replay-lines 40
python: COLUMNS=400 python3 -u hermon.py render --hermes 20260828_141000_30cf4c --replay-msgs 40
```

Both streams stripped of whitespace come to **5,767 characters and are
byte-identical**. Every diff hunk is Rust preserving a source newline
(paragraph breaks, markdown list items, fenced-block lines) where Python folds
it into one wrapped flow — item 5, nothing else. Same `▶`/`◀` markers, same
`» user:` lines, same order.

### OpenCode — `O:LSN6Vy` (`ses_fc1226b04ffeSz7SFzhoLSN6Vy`)

```
rust:   hermon render O:LSN6Vy --fresh-window 900000 --replay-lines 40
python: COLUMNS=400 python3 -u hermon.py render --opencode ses_fc1226b04ffeSz7SFzhoLSN6Vy --replay-parts 40
```

This session was last updated 2026-08-26, outside both the default
`--fresh-window` (3600 s) and Rust's `RECENCY_WINDOW`, so `--fresh-window
900000` was needed to address it at all. It correctly appears in neither
roster above.

Six tool calls on each side, same tools in the same order
(`bash` ×5, `webfetch`), same result and `Σ` lines. Diff is items 2 and 5.

---

## ⚠️ Non-blocking: tool-call JSON formatting (item 2)

Undocumented and cosmetic, but present on every tool-call line, so worth a
follow-up issue rather than silence.

**Separator spacing** — Python's `json.dumps(..., ensure_ascii=False)`
(`hermon.py:219`, `:849`) uses the default `", "` / `": "` separators; Rust's
`serde_json::to_string` (`src/render/claude.rs:72`) is compact:

```
python: ▶ Bash {"command": "git status"}
rust:   ▶ Bash {"command":"git status"}
```

**Key ordering** — Python preserves insertion order; Rust's `serde_json::Map`
is a `BTreeMap` (no `preserve_order` feature), so keys come out sorted:

```
python: ▶ webfetch {"url": "https://www.githubstatus.com/api/v2/summary.json", "format": "text"}
rust:   ▶ webfetch {"format":"text","url":"https://www.githubstatus.com/api/v2/summary.json"}
```

Because args are clipped at `ARG_CLIP` (120 chars, same on both sides), the
tighter Rust encoding fits one or two more characters before the `…`:

```
python: … tests/live_notify_check.rs …
rust:   … tests/live_notify_check.rs &…
```

Same underlying value, different serialization. Not treated as a signoff
blocker.

---

## ✅ Accepted deviations (documented / intentional)

**Cost column `0.0000` vs `-`.** Documented at `src/roster.rs:42-45`: Python
distinguishes "no cost data" (`-`) from a genuine `$0.0000`; `SessionMeta::cost`
has already collapsed the two to `0.0`. Visible on every Claude row above.

**Totals footer.** `2 live · 7 done · Σ $0.62 · 120,808,931 in` is a Rust
addition (`roster.rs` `totals_line`), documented in `README.md` ("fleet totals
in the footer") and carrying the attention-state counts from #39. Python has no
equivalent line.

**No terminal wrapping in `render`, whitespace collapsed.** Python wraps with
`textwrap.wrap` to `terminal_size - 2` (`hermon.py:89`, `:115`, `:214`). Rust
emits one logical line per source line with internal whitespace collapsed and
leaves wrapping to the pane widget — documented at `src/render/claude.rs:7-10`,
`src/render/mod.rs:5`, `src/render/opencode.rs:140-141`. Every render diff
above is dominated by this; the underlying text is identical (proved by the
whitespace-stripped hashes).

**Render banner.** Python prints `hermon · tailing <path>` (`hermon.py:270`)
and `hermon · hermes session <id> (<db>)` (`hermon.py:778`). Rust is addressed
by roster key (`hermon render C:2b32b1`) rather than by file path or raw
session id, so there is no path to echo. All diffs above drop Python's first
line before comparing.

---

## Method notes (for anyone re-running this before the tag disappears from view)

- `timeout(1)` is not on macOS. Use `perl -e 'alarm N; exec @ARGV' <cmd>`.
- **Run Python as `python3 -u`.** Otherwise `hermon.py`'s stdout is
  block-buffered into a pipe and the buffer is lost when the render is killed,
  so the capture looks empty.
- **Set `COLUMNS=400` on the Python side.** Its wrapping is width-dependent;
  a wide terminal shrinks item 5 to near-nothing and makes real differences
  visible instead of drowned.
- Python `render --hermes` takes the **full** session id
  (`20260828_141000_30cf4c`), not the 6-char roster key. Passing the roster key
  matches no rows and the process polls silently forever, which reads as a hang.
- Replay depth must be pinned on both sides: Rust `--replay-lines` ↔ Python
  `--replay-msgs` (Hermes) / `--replay-parts` (OpenCode); `--replay-bytes` on
  both for Claude.
- For live sessions only the replay prefix is deterministic; the tail diverges
  by capture timing. Compare prefixes, or use a finished session.

---

## ⚠️ Required from a human — BEFORE the deletion commit lands

- [ ] **`git tag python-final`** on the last commit that still contains
      `hermon.py` — i.e. `e1a0428`, the parent of the deletion commit — and
      push it:

      ```bash
      git tag -a python-final e1a0428 -m "Last commit containing the Python implementation"
      git push origin python-final
      ```

      **This session created no tag** (no commits were made). The
      release-notes section added by this PR
      (`packaging/RELEASE_NOTES.md` § 7) tells readers to run
      `git show python-final:hermon.py`, and `src/`/`tests/` comments cite
      `hermon.py` line numbers — all of which dangle until the tag exists.

## Still unverified — human soak items from the issue

Out of reach for a non-interactive session; both were listed in the issue as
part of signoff and neither is covered by the evidence above:

- [ ] **Busy-fleet `ls` run.** The capture above covers 9 sessions on a quiet
      machine. #66 was precisely a bug that got worse with fleet size, so a
      genuinely busy fleet is still the stronger test.
- [ ] **Day-long `hermon watch` soak with notifications on.** Not attempted.
      Should cover pane lifecycle (linger, eviction, resurrect), notification
      delivery and cooldowns, and the mute key.

If either turns up a divergence, it is a new bug issue against the Rust side —
`python-final` keeps the oracle one checkout away.

---

## What this PR changes

**Deleted** (19 tracked files):

- `hermon.py`
- `tests/test_hermes.py`, `tests/test_lock.py`, `tests/test_opencode.py`,
  `tests/test_render.py`
- 14 tracked `.pyc` files under `__pycache__/` and `tests/__pycache__/`
  (these were committed, not ignored — they dirtied `git status` on every
  Python run)

**CI** — the `Python tests` step (`python3 -m unittest discover -s tests`)
removed from both `.github/workflows/ci.yml` and
`.github/workflows/release.yml`. Both files re-parsed clean afterwards; the
remaining steps are Build / Test / Clippy / Format (+ tag-version check and
release build on `release.yml`).

Note: `release.yml`'s "Check tag matches crate version" step still shells out
to `python3 -c` to read `cargo metadata` JSON. That is an unrelated one-liner,
nothing to do with `hermon.py`, and was left alone.

**Docs**

- `README.md`: the `## Python version` section is gone. Its content moved to
  `packaging/RELEASE_NOTES.md` § 7 "Predecessor: the Python implementation",
  which points at the `python-final` tag; the two in-text references
  (`tmux` mechanism, source-integration touch-point count) and the
  `--fresh-window` table row now read as history rather than as pointers to a
  live file.
- `packaging/RELEASE_NOTES.md`: new § 7 as above; the release-workflow
  description no longer claims the tag build runs "the legacy Python tests".
- `docs/roadmap.md`: **M7 closed out** ✅. The dangling `[hermon.py](../hermon.py)`
  link in the intro is replaced by a pointer to the predecessor section, and
  the sequencing notes rewritten.
- Also in `docs/roadmap.md`: **M6 marked done.** It was still 🚧 with
  "#44 still open", but #44 merged as `253c88d` ("Notification delivery, mute
  key and CLI flags"). This is adjacent to #47's scope, but the sequencing note
  ("M6 and M7 are the only milestones with open work") had to be rewritten for
  M7 anyway and would otherwise have been left self-contradictory. Flagging it
  explicitly in case you'd rather split it out.

**`.gitignore` — no change needed.** It contains only `target/`; there were
never any `__pycache__` lines to drop. The `.pyc` files were tracked, and are
removed by the deletion above.

**Source comments left as-is.** `src/render/*.rs`, `src/source/*.rs` and
`tests/opencode_source.rs` cite `hermon.py:NNN` and `tests/test_*.py::case`
as provenance for the behavior they port. The issue scopes its grep criterion
to docs, and these are genuine history worth keeping; § 7 of the release notes
records that they resolve against `python-final`.

---

## Acceptance criteria

| Criterion | Status |
|---|---|
| Parity evidence in the PR body (outputs pasted) | ✅ this document |
| `python-final` tag exists before the deletion commit | ⚠️ **human step, documented above — not done by this session** |
| CI green with the Python step removed | ✅ locally; both workflows parse, all four Rust gates pass |
| `git grep -li "hermon.py"` in docs → only changelog/history mentions | ✅ see below |

```
$ git grep -li "hermon\.py" -- README.md docs/ packaging/ .github/
docs/roadmap.md
packaging/RELEASE_NOTES.md
```

Both are history: `docs/roadmap.md` records what M1/M7 were and that #47
deleted it; `packaging/RELEASE_NOTES.md` is the § 7 predecessor note. `README.md`
and both workflows are clean.

## Gate results

| Gate | Result |
|------|--------|
| `cargo build --all-targets` | ✅ clean |
| `cargo test` | ✅ **414 passed, 0 failed, 3 ignored** (the `live_notify_check.rs` manual-only ones) |
| `cargo clippy --all-targets -- -D warnings` | ✅ 0 warnings |
| `cargo fmt --check` | ✅ clean |
| `python3 hermon.py` render parity, pre-deletion | ✅ captured above, then deleted |

Workflow YAML validated by parsing both files with Ruby's `psych` (`yq` and
`actionlint` are not installed on this machine).
