# Slice 4 independent acceptance freeze

The independent test writer authored `crates/hermon-flow/tests/agent.rs` and
`tests/fixtures/agent.py` from issue #119 and the Slice 0 contract before process
supervision was implemented. `freeze.sha256` freezes both files. Production
implementation must preserve those inputs; a genuine test defect returns to the
test writer for a documented correction and superseding freeze.

Behavioral RED: `cargo test -p hermon-flow --test agent --locked` compiled and ran
14 tests: **1 passed, 13 failed** against behavior-free public API stubs. The
existing Hermes capability-gate stub correctly rejects unsupported supervision;
all launch, reconciliation, parser and argv behaviors fail. The retained
`red.txt` contains the full results. Clippy with warnings denied passes
(`red-clippy.txt`). Checks used the isolated toolchain under
`/private/tmp/hermon-116-rust/` on macOS on 2026-10-29.

The Python fixture was also smoke-tested directly before freeze: its descendant
updates a heartbeat, ignores SIGTERM, and its parent handles SIGTERM by writing a
valid result and exiting zero. The smoke runner killed the entire owned group
afterward. This confirms the timeout tests exercise an active descendant and a
misleading zero exit rather than fixture syntax failure.

## Accepted behavior

- The six issue-named tests cover valid done, typed block through the real hub,
  malformed/missing results through the real hub, timeout of the owned group
  while an unrelated process survives, hub restart reattachment without duplicate
  launch, and exact Slice 0 Hermes start/resume argv.
- Attempt storage is `root/<run>/steps/<step>/<number>`, with a hub-written
  `task.md`, agent-written `result.json`, durable process metadata and separate
  stdout/stderr logs. The adapter records the detached supervisor PID, owned
  child process group and optional opaque session ID.
- Starts bind idempotently to `(run_id, step, number)`. Dropping/recreating the
  adapter leaves its worker running; the same start reuses the existing handle.
  The detached worker enforces its deadline while the hub is absent.
- Timeout and cancellation are durable before signalling. A SIGTERM handler's
  valid result and exit zero cannot turn either into accepted completion.
  Settlement includes the descendant that ignores SIGTERM.
- Direct argv substitution preserves spaces and shell metacharacters as literal
  arguments. Configured task stdin receives the exact task bytes. Resume uses
  explicit start/resume templates; absent resume support falls back to start
  with the host responsible for contextualizing the new task.
- Codex completion requires `turn.completed`, with session capture from
  `thread.started.thread_id`. Claude requires a non-error completed result;
  `subtype: success` with `is_error: true` is insufficient. Both session IDs
  remain observable after adapter restart. A nonzero process exit rejects a
  valid step result.
- A dead supervisor without an exit receipt is a transient observation failure,
  preserves its existing handle and does not authorize duplicate work. The hub's
  existing unresolved-outcome policy still prevents resend.
- Hermes uses the exact `hermes -p PROFILE chat -Q --query-file TASK` command,
  optionally followed by `--resume ID`. Its ID parser takes the last exact
  `session_id: OPAQUE` stderr trailer, including CRLF, without guessing UUIDs.
  Actual Hermes launch remains a capability block until a stable scoped
  cancellation contract has been independently verified. The tests launch no
  vendor CLI, contact no model and do not stop shared gateways.

The scope is process adapters and their hub integration seam. Tracker/code-host
adapters and user-facing hub hosting are later slices.

## Supplemental review regressions

Production review identified two additional defects after the original 14 tests
passed: a valid `result.json` that predated a fresh launch could be accepted when
the new command wrote no result, and a `./relative-agent` executable resolved
against the hub's current directory instead of its configured worktree.

The independent test writer added `tests/agent_regressions.rs` without changing
the original freeze. `supplemental-red.txt` records **0 passed, 2 failed** against
the then-current implementation, reproducing both behaviors before their fixes.
`supplemental-freeze.sha256` separately freezes these two tests. A fresh launch
must discard pre-existing completion output before starting its command, and
relative executable paths must resolve in the command's configured worktree.

Review also required observing that the entire owned group had settled after
SIGKILL before publishing a finished receipt. Merely queuing a signal and reaping
the group leader cannot establish that all descendant tools have stopped.

A final narrow regression covers a known failure before spawning: if `worker.log`
cannot be opened, the adapter must not retain a launch reservation or consume
bot capacity. After the filesystem is repaired, the same request can launch.
`tests/agent_launch_failure.rs` was independently authored and frozen in
`launch-failure-freeze.sha256`; `launch-failure-red.txt` records **0 passed,
1 failed** before the spawn-stdio ordering fix. The preceding freezes remain
unchanged.
