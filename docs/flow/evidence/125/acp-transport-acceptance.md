# Public ACP transport acceptance freeze

The owner approved resolving the Hermes prerequisites before an owner-selected live
pilot. These independent tests define the new opt-in public ACP adapter. The
legacy `Hermes` binding must keep its existing cancellation gate.

`crates/hermon-flow/tests/hermes_acp.rs` exercises the public ProcessAgent API
with a fake `hermes -p sdlc-implementer acp` executable. BotConfig is deserialized
at runtime so missing support produces behavioral RED instead of a future-enum
compile error. The fixture never contacts Hermes, a gateway or a provider.

The six tests require:

- Start with the exact profile argv, initialize ACP v1, open the configured
  worktree with its profile context, set accept_edits for that session, and
  record the actual ACP/current Hermes session IDs, expected model and verified
  family after end_turn.
- On deadline, send session/cancel for the exact target ACP ID, retain timeout
  intent, and wait for its prompt terminal response before confirming settlement.
  The target heartbeat stops while an unrelated concurrent session keeps running.
- A late end_turn cannot undo a recorded cancellation.
- Losing the ACP viewer before a terminal response leaves the gateway outcome
  uncertain. It cannot release the concurrency slot or claim confirmed settlement.
- After an owner question, reopen the recorded session with explicit session/load
  and deliver the exact answer, including after constructing a new host adapter.
- Deny a dangerous permission request by default. The fixture never executes the
  requested command; it records the client's response.

The shared fake is `tests/fixtures/hermes-acp.py`. It implements newline JSON-RPC,
finite prompts and cancel responses, metadata, permission requests, heartbeats
and isolated SQLite usage records. Its done, hold, late-end, question and
permission handshakes were smoke-tested independently. Provenance tests have a
separate freeze and reuse this fixture without changing it.

`acp-transport-red.txt` records six compiled tests failing on the unsupported
HermesAcp configuration. `acp-transport-clippy.txt` records test-only Clippy with
warnings denied. `acp-transport-freeze.sha256` freezes the test, fixture and this
contract before the adapter implementation. The live cancellation probe under
the private operator record is separate evidence; this fake suite does not
claim a live pilot or waive any owner gate.
