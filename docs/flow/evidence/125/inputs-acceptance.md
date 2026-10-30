# Accepted artifact routing regression freeze

Independent tests in `crates/hermon/tests/flow_inputs.rs` establish two host
routing defects discovered during issue 125 integration. They run the real CLI,
intake and hub with isolated git repositories, local fake tracker/gh and finite
fake workers. Existing frozen acceptance files are unchanged.

1. A planner produces `plan.md`, is accepted by the hub, and transitions directly
   to a consumer declaring that artifact as input. The consumer must start within
   the same reconciliation, with the accepted artifact in its task, without a
   manual retry or another owner action. This exposes the immutable pre-advance
   record incorrectly hiding the just-accepted producer event.
2. A genuine Owner GO gate separates planner acceptance from consumer dispatch.
   After acceptance, an unrelated higher-numbered attempt directory containing a
   conflicting artifact is introduced. Owner approval must dispatch the consumer
   using the accepted planner attempt, never the largest directory number.

`inputs-red.txt` records both intended behavioral failures: the direct chain
blocks on `missing accepted input plan.md`, and the gated chain delivers the
unaccepted directory's text. These are compiled tests, not API/build failures.
The separate manifest freezes these tests before the host input fix.
