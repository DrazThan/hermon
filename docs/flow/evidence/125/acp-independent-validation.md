# Independent ACP validation

All 22 new frozen ACP tests passed against the final implementation: 19 transport,
provenance, request-ID and permission tests, plus three CLI doctor tests. All five
active ACP freeze manifests match. The original canonical-path test and v1
manifest remain archived with the documented independent correction.

Executed commands:

```sh
cargo test -p hermon-flow --test hermes_acp --test acp_provenance \
  --test acp_rpc_collision --test acp_permission_scope --locked --offline
cargo test -p hermon --test hermes_acp_doctor --locked --offline
```

Results are in `acp-independent-flow-green.txt`,
`acp-independent-doctor-green.txt` and `acp-independent-freeze-check.txt`.
The tests used isolated executables, temporary SQLite stores and local fixtures.
They made no live model, tracker or GitHub calls. Workspace checks are recorded
separately by the implementing agent.

I reviewed the final transport, probe and ProcessAgent integration. Cancellation
intent is persisted before the exact-session cancel notification. Gateway
settlement comes from the prompt's terminal response; viewer loss leaves an
unfinished receipt and retains concurrency. A late end_turn cannot undo cancel
or timeout. Resume explicitly loads the recorded ACP session and carries the
owner answer.

Version/current-model checks precede prompt submission. Main usage is read-only,
requires positive task-empty calls and rejects unknown/mixed families. Only
compression-parent lineage is followed, with cycle and depth limits. The receipt
retains actual session identities and contributing usage. The legacy Hermes
binding keeps its original supervision gate.

Incoming server requests are handled separately from outbound response IDs.
Permission approval is limited to allow_once for write_file in the exact owned
attempt directory. The session ID, absolute path, canonical parent, existing
file type and protected basenames are checked. Other tools, dangerous commands,
outside paths, symlink escapes, hub state/control files and allow_always remain
denied. Doctor uses this same transport and validates the real assigned result
and usage without allocating a run or worktree.

No remaining material issue was identified in this adapter review. The live
five-profile and scoped active-tool cancellation probes are separate evidence.
These synthetic results establish the adapter contracts tested here. The live
workflow, owner gates, PR, owner merge and Docs still need their own run evidence.
