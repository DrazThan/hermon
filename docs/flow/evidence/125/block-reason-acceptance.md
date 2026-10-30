# Independent correction-context regression for #125

A synthetic CLI run starts GREEN, whose valid result reports a `test_defect`
with a precise reason. The host automatically routes to `red_fix`, whose
`block_reason` input must contain that exact reason. The bot is otherwise a
fixture and correction completes to an owner gate.

Behavioral RED was observed before a product fix: the pipeline routed and
finished correction, but the generated task contained an empty
`## Input: block_reason` section. See `block-reason-red.txt`.

Command: `cargo test -p hermon --test flow_block_reason --locked`.
The test uses a local loopback tracker fixture and temporary git worktree only.
No live vendor sessions, tracker writes or bot-profile edits occur.
The new test copies shared fixture setup without changing any frozen source.
