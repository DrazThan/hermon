# Slice 8 independent acceptance

The independent test writer authored `crates/hermon/tests/flow_cli.rs` before
production implementation. The file is frozen by `freeze.sha256`.

The suite invokes the real `hermon` binary, synthetic executable bots and
project checks under an explicit hub environment, a fake `gh`, isolated git
repositories/worktrees, and an in-process loopback Linear GraphQL server.
It uses no vendor credentials, live tracker writes or production repositories.

Acceptance covers:

- Intake refuses failed doctor probes before creating a run, branch or
  worktree, or posting a started comment.
- Missing workflow bots and commands absent from the configured PATH produce
  named errors. Doctor probes every required bot, tracker authentication,
  `gh auth status`, and project commands using the complete configured
  environment. Project-command/auth failures and bot exit zero without a
  completion result fail doctor.
- Successful intake snapshots the workflow source, SHA-256 workflow/template
  hashes, selected base, branch and worktree; exports issue context and posts
  started through the central tracker. Credentials do not enter the run or
  event state. Repeated explicit run IDs cannot replace an existing run.
- Status emits versioned JSON containing durable run state for one or all runs;
  an unknown run fails without creating a directory.
- Pause, resume, retry, override with a reason, approve, answer and cancel each
  change persisted state and append versioned, sequenced events. CLI replies
  and approvals are mirrored to the tracker as entered in Hermon.
- Wrong/current-step mismatches, empty override reasons, answers without an
  owner question and approvals without a gate reject without mutation. A CLI
  approval cannot bypass the owner-merge ancestry check. Cancelled runs cannot
  be resumed, retried or approved.

The controls fixture obtains a genuine intake record, then seeds hub states
through the already-public serialized `hub::Run` schema to test controls
without depending on the later menubar hosting slice.

## Behavioral RED

On 2026-10-29, `cargo test -p hermon --test flow_cli --locked` compiled
successfully and exited 101: **0 passed, 10 failed**. The existing binary
rejected the new `doctor` and `run` subcommands. These are behavioral CLI
failures; there are no compiler errors or artificial failing stubs.
Exact output is in `red.txt`.

The suite was formatted with the installed Rust toolchain before this freeze.
Verify from repository root using `shasum -a 256 -c
docs/flow/evidence/123/freeze.sha256`. Any acceptance correction must return to
the independent writer and produce a documented superseding freeze.
