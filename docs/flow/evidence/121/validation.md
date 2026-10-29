# Independent Slice 6 validation

After implementation, the copied-template-marker fix and the fail-closed
default-agent hook fix, the independent test
writer replayed all frozen acceptance and supplemental regressions:

`cargo test -p hermon-flow --test tracker --test tracker_agent --test tracker_recovery --test tracker_default_agent --locked`

Result: **17 passed, 0 failed**: 10 tracker/template/hub tests, 2 real process
resume tests, 4 supplemental review regressions and 1 default-agent contract
regression. Complete output is retained
in `independent-final-green.txt`. Matching Clippy with warnings denied passes
(`independent-final-clippy.txt`). The real CLI preview acceptance independently
passes **1 test, 0 failures** (`independent-cli-green.txt`).

All four active SHA-256 manifests verify. The original core freeze, the genuine
macOS accepted-socket fixture defect and superseding freeze are documented in
`acceptance.md`; the original RED and corrected-stub replay remain retained.
The supplemental copied-marker regression and unsupported-agent answer hook
regression both failed before their respective fixes and pass now. No assertion was weakened to accept production.

The fake HTTP peers use synthetic app credentials, issue/comment IDs, timestamps
and owners. The process fixtures launch only owned synthetic Python workers.
Host access was used for local loopback binding and process settlement, which
the restricted sandbox cannot observe. No live Linear comment or vendor agent
was invoked. Actual Linear app-actor readback and survival of the hidden marker
through its renderer remain unverified live checks.

Independent review found no remaining material defect in this slice's tested
scope. Requests bind to configured issue/owners and immutable comment bodies;
ambiguous mutation recovery reads the reserved ID rather than duplicating a
comment. GraphQL failures, unsuccessful mutations and missing IDs do not latch
success, and server error strings cannot expose the configured token. A null
author, bot, unrelated thread, old gate visit or nonliteral GO cannot approve a
gate. Reply progress and authorized matching results persist together. Inclusive
time watermarks preserve ties and find newer inserts after newest-first paging
and restart. The real process adapter resumes a recorded explicit session or
carries prior task context into a fresh session, with an owner answer applied
once to the new task and task-file stdin. An adapter without explicit owner-answer
delivery support now refuses with a capability block before launching a
context-free replacement.

The hosting layer remains responsible for configuring an app-actor token,
renewing it, selecting the intake-frozen templates, supplying the trusted owner
mention and owner IDs, and serializing state access under the run lock. Full
workspace/build/release and cross-platform CI are validated by the integrating
implementer.
