# Explicit failed ACP admissions

The pinned Hermes ACP adapter reports a canonically failed admitted turn as a
JSON-RPC error: code -32603, message `Internal error`, data.details
`admitted_turn_failed`. This response is distinct from ambiguous transport loss.
Only a response to the currently awaited prompt may establish this failed
terminal outcome. It cannot accept a result, request another prompt, or pass a
gate. Generic errors, wrong codes/details/IDs, notifications and colliding
server requests remain unsettled.

A narrowly scoped `reconcile RUN STEP` control may recover an older dead worker
from that same recorded proof. It must hold the run and worker locks, verify the
owned attempt and launch/session/model/version binding, and consume a complete,
bounded, regular non-symlink ACP trace with the exact initialization/open/mode
and sequential prompt responses. The final frame must be the failed admission
for the expected prompt. Broken, missing, foreign, trailing or ambiguous traces
and live workers are refused without modifying the receipt. Reconciliation
starts no transport, session, prompt or check. It preserves the trace, launch,
result file and worktree, records failure only, and leaves the blocked run for
normal host observation and an explicit supported retry.

These five TDD tests and their synthetic wrapper are frozen before production
changes. They derive requests from the unchanged ACP fixture. Initial fixture
execution exposed a read-before-process-receipt race in the new test helper;
that helper was corrected before freezing. The retained compiled behavioral RED
is 3 passed and 2 failed: known failed admissions remain unsettled and the
reconciliation command is absent. Earlier frozen packets are unchanged. Raw
live evidence is retained privately; exported logs normalize operator paths.
