# Bounded continuation after an interim ACP turn

These independent tests require an end_turn without the assigned result file to
continue the same owned attempt and ACP session. A reminder may only finish the
current step's existing file contract. It is not an owner answer, a gate approval,
a new task or permission to relaunch work.

The packet covers success after one interim turn, and exhaustion after exactly
three follow-ups (four turns total). Confirmed end_turn exhaustion must return a
settled transport outcome with an explicit capability-block result, so the hub
holds and alerts instead of treating it as a transient process failure. It must
not claim a successful step.

An existing invalid result file stops continuation immediately and its bytes
remain available to strict result validation. Cancellation during a follow-up
must target the same session. A delayed first turn followed by a held prompt
must still use the original three-second overall launch deadline. Cancellation
and timeout keep their failed outcome flags.

The finite public fixture derives interim behavior from the unchanged frozen ACP
fake. It has no real background job, provider, session, tracker or owner context.
The tests require one launch, one session and one attempt, with no model change
or owner-answer receipt. Raw logs stay in ignored local storage; public evidence
uses neutral operator and temporary-path labels. The test, wrapper and this
contract are frozen before the continuation implementation.
