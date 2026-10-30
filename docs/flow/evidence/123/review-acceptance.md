# Independent Slice 8 implementation review

The independent writer reviewed the initial GREEN implementation and added
`crates/hermon/tests/flow_cli_regressions.rs` without editing the original
frozen acceptance suite. The supplemental suite is frozen separately by
`review-freeze.sha256`.

Behavioral RED against the initial GREEN implementation compiled successfully
and exited 101: **4 passed, 4 failed**. Exact output is in `review-red.txt`.
The four material failures were:

- Doctor passes when the workflow declares `project_check` but the project
  omits that required command, so intake accepts a missing capability.
- Doctor checks global `gh` authentication without checking access to the
  configured repository. An authenticated account without repository access
  is accepted.
- Status replays a committed pause from the append-only journal after a lost
  snapshot write, but fails to persist the recovered snapshot to `run.json`.
- A cancelled run with an unsettled owned worker stops polling forever. The
  worker's later settlement is never reconciled because `advance` returns
  immediately for cancelled runs. The regression requires clearing the old
  handle while retaining cancellation and rejecting late success, evidence,
  and successor dispatch.

The passing review cases establish that pause prevents new dispatch, a torn
event tail preserves the prior committed control, a failed tracker mirror
retains the local answer and later retries without reapplying it, and a foreign
attempt handle cannot be signalled or receive a cancellation file. Cancellation
intent remains durable when worker settlement fails.

Review also identified the intake publication window between renaming the
staging directory and holding its run lock. A hosting process could observe or
modify the published run during started-comment delivery. This is reported as
a locking defect; it is not represented by a timing-dependent test.

The original acceptance freeze remains unchanged. No production files were
edited by the independent test writer.
