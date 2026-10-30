# Central-app publication acceptance freeze

Independent executable-level tests in `crates/hermon/tests/flow_publication.rs`
cover the owner-visible outputs required by issue 125. They use local tracker
fixtures, fake workers and isolated git state. They do not post live comments.

- An actual planner produces and declares `publish: [plan.md]`. On acceptance,
  the host reaches Owner GO and posts a marked GO request containing the accepted
  plan's readable text. The owner must be able to review the plan without a manual
  filesystem lookup, run edit or extra nudge. Reconciliation does not duplicate
  the plan/GO comment.
- An actual Docs worker completes successfully. The host renders the configured
  final `outcome.md` through the central tracker app, includes the accepted Docs
  summary, retains its run marker and posts exactly once across host restarts.
  The test uses a docs-only workflow and makes no claim to verify a live PR merge.

`publication-red.txt` records intended behavioral failures: the current GO
request has empty artifact context, and no outcome comment is posted after Docs
reaches Done. Both tests compile and run with localhost fixture access.

A verified merge SHA remains a real code-host concern: do not invent one from
an unmerged PR or infer it from a docs fixture. Full live PR/merge/Docs acceptance
remains pending the owner-guided pilot.
