# Slice 6 independent acceptance freeze

The independent test-writer authored `crates/hermon-flow/tests/tracker.rs` and
`crates/hermon/tests/templates_preview.rs` from issue #121 and plan #114 sections
5.1 and 9 before production implementation. `freeze.sha256` freezes these tests.
Public APIs were introduced as behavior-free stubs to obtain executable RED.

`cargo test -p hermon-flow --test tracker --locked` compiled and ran **10 tests,
0 passed, 10 failed** (`red.txt`). Failures are at the unimplemented adapter or
renderer behavior, never compile errors. Local fake Linear TCP servers require
host access because the sandbox refuses loopback socket binding. An initial
sandbox attempt was discarded as invalid RED; the recorded run has host access.
`cargo test -p hermon --test templates_preview --locked` compiled and ran **1
test, 0 passed, 1 failed** at the CLI preview stub (`red-cli.txt`). Matching tracker
Clippy with warnings denied passes (`red-clippy.txt`).

Accepted behavior:

- All seven comment kinds render built-ins and post through HTTP GraphQL using
  one configured central bearer token. Mutations bind the configured issue and
  supply no actor override. Question and GO templates include the trusted owner
  mention. The run/request hidden marker is appended after any selected template.
- Untrusted values cannot add literal mentions, Linear profile links, markdown
  formatting or hidden markers. Values are substituted once; an untrusted
  `owner_mention` field cannot override configured owner notification. Invalid
  marker IDs and unknown templates fail closed.
- Posting the same request reuses its thread across adapter restart. Credentials
  do not appear in durable files.
- Exact body `GO`, from a configured owner ID, under the correct request parent,
  passes without a coordinator. Whitespace, casing, markup and extra text do not.
  A bot, root comment, other thread or prior gate visit cannot approve a gate.
- Polling persists an inclusive createdAt watermark and accepted reply latch.
  The fake models newest-first pages: a terminal Relay `after` cursor cannot
  discover later inserts. Restart polling retains the watermark, restarts at the
  head, preserves equal timestamp eligibility and carries the maximum timestamp
  across pages. A later inserted owner reply remains discoverable.
- Public reads return issue metadata, label search results and parsed threaded
  comments with an opaque continuation cursor.
- A real deterministic hub publishes a bot's question, blocks, survives a run and
  tracker restart, ignores unauthorized/unrelated replies, applies an owner answer
  once through the agent resume hook with the prior handle, and posts an answer
  acknowledgement. The fake agent records the exact prior handle and answer.
- The real `hermon templates preview <workflow> <template>` command resolves pack
  paths relative to the workflow, renders samples plus a marker, honors a project
  override, and rejects an unknown template.

All peers and issues are synthetic. No live Linear comment is posted; issue #121
reserves that live check for explicit owner approval. The core freeze may not be
edited by the implementer; genuine defects return to the independent test-writer
for documented superseding freeze.

## Supplemental process resume freeze

`tests/tracker_agent.rs` is frozen separately in `supplemental-freeze.sha256`.
It launches only a synthetic Python worker through the real detached ProcessAgent.
The independent RED is **2 tests, 0 passed, 2 failed** (`supplemental-red.txt`):
after reopening the adapter, the default answer hook launches a fresh process
without appending the owner answer. Matching Clippy passes
(`supplemental-red-clippy.txt`). Host access allows owned process settlement
observations; no external agent or user process is involved.

The success contract requires reopening durable process receipts, resuming with
the explicit prior session when supported, adding the literal owner answer to
the new task and its stdin, and retaining the host-created continuation task.
When resume is unsupported, a fresh session must also receive the prior task
context. Reconciliation repeats neither task mutation nor owner answer append.

## Superseding freeze v2: macOS accepted-socket fixture correction

Initial production execution reached a platform defect in the fake HTTP server:
its listener is nonblocking, and macOS can propagate that flag to accepted
sockets. An immediate read raced client transmission and returned `WouldBlock`
before any behavioral assertion (`initial-green.txt`). The independent
test-writer confirmed this fixture problem and added exactly one setup call,
`stream.set_nonblocking(false)`, before reading the accepted stream. Existing
read deadlines and Content-Length assertions remain unchanged; no acceptance
assertion was weakened.

`freeze.v1.sha256` preserves the original manifest. The active `freeze.sha256`
supersedes it with the corrected tracker test; the CLI test is byte-identical.
The corrected test was replayed against the original behavior-free sources from
`16c0f27`, exported by `git archive` into
`/private/tmp/hermon-121-corrected-red`. It compiled and ran **10 tests, 0 passed,
10 failed** at the stub behaviors (`red-v2.txt`). Live production files were not
reverted or modified for this replay. CLI and supplemental process RED remain
valid and unchanged.

## Supplemental independent review regressions

`tests/tracker_recovery.rs` is independently authored and frozen in
`review-freeze.sha256`. Its first implementation replay is **4 tests, 3 passed,
1 failed** (`review-red.txt`), with matching Clippy passing (`review-clippy.txt`).

The failing regression covers an owner template containing a copied old hidden
marker. The acceptance requirement says the current marker survives any template;
rejecting that template at render prevented the comment from being delivered.
The corrected behavior preserves the template text, neutralizes its copied marker,
and appends the one authoritative current marker.

The three passing regressions verify restart recovery after a mutation is
accepted but its HTTP response is lost (exactly one mutation, then read-by-ID),
GraphQL error/success:false/missing-ID responses never latching success, credentials
never entering errors, immutable request body binding, issue/owner state binding,
and a nullable comment author never authorizing GO. These use a separate local
HTTP peer that preserves the client-supplied UUID and can drop a response after
recording the accepted mutation. No live Linear issue is involved.

## Supplemental unsupported-agent answer contract

Independent review of the public Agent trait found that its default answer
hook silently called `start` without carrying the owner answer. Although the
real ProcessAgent overrides that hook correctly, another adapter could lose
the answer while appearing to resume successfully.

`tests/tracker_default_agent.rs` is independently authored and frozen in
`default-agent-freeze.sha256`. Its behavioral RED is **1 compiled test, 0 passed,
1 failed** (`default-agent-red.txt`) because the default hook returns a new
context-free session. Matching Clippy passes (`default-agent-clippy.txt`).
An adapter without explicit owner-answer support must return a nonempty
`capability` block and must not launch a replacement. Supported adapters may
resume the prior session or start fresh with complete answer/context delivery.
