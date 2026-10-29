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
