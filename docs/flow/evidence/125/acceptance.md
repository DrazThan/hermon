# Issue 125 independent acceptance freeze

The independent test writer authored these tests before the pack/runtime
implementation. The scope is the shipped SDLC workflow pack, project configuration,
and role guidance reaching real queued workers through the CLI and host.

## Automated acceptance

- The shipped workflow preserves Owner GO, RED, frozen GREEN, different-family
  bounded review, PR, owner-only merge and Docs. Test-defect correction is bounded,
  and scope changes return through a fresh plan/GO. The planner also publishes
  an independently prepared `test-expectations.json` artifact for RED checks.
- Every bot step has substantive `roles/<step>.md` guidance. All seven shipped
  pack templates load through the actual template resolver and render with the
  hub marker; question and GO templates include the owner mention.
- The RummyRune project loads under the actual strict runtime schema. Its
  repository path alone is redirected to a temporary directory for portable CI.
  It binds the five named Hermes profiles, targets qa and DrazThan/RummiRune,
  uses literal pnpm lint/check argv and the repository's actual Vitest invocation
  (there is no pnpm test script), and allows the full check more than 20 minutes.
- A real CLI intake snapshots convention-based role guidance and its SHA-256 in
  the run's `context/roles.json` manifest. Editing the pack afterward cannot alter the actual dispatched task.
  Existing unreadable role entries fail before allocating a run/branch. Tampering
  with a frozen role snapshot prevents dispatch and identifies the role error.
- Existing packs without role files remain covered by the existing intake/host
  suites. These tests do not mandate roles for every generic workflow.

`crates/hermon-flow/tests/sdlc_pack.rs` covers pack configuration. The independent
`crates/hermon/tests/flow_roles.rs` fixture uses only isolated git repositories,
local HTTP, fake bots and fake gh; it never touches vendor sessions or live tickets.
Vitest report parsing and accepted-plan expectations have a separate independent
freeze because they are prerequisite runtime gaps discovered during inspection.

## Manual/repository review acceptance

README positioning and Non-goals must distinguish read-only monitoring from the
opt-in hub's owned process control, and explain the new workflow commands. The
roadmap must add M11 without claiming the live pilot has passed. A migration/pilot
runbook must preserve sdlc-reliability history, record each owner-approved profile
revision and the controller pause, and describe a selected RUM ticket's gates,
RED/GREEN/review/PR and post-owner-merge Docs evidence. Unsupported Hermes scoped
cancellation remains a capability block, never a silently substituted agent.

No synthetic fixture result establishes the live pilot. The owner chooses the
small RUM ticket. Live tracker activity, profile edits, cron pause, owner GO and
owner merge need their actual recorded authorization/state. The repository PR
must identify any outstanding live acceptance rather than closing it by assertion.

## RED evidence

`pack-red.txt` records three behavioral failures: missing project, missing pack
comment templates and missing planner expectation output. `roles-red.txt` records
missing intake snapshots and ignored unreadable role entries. An initial
sandbox-only localhost bind failure is retained as `roles-sandbox.txt`; it is not
behavioral RED. The role suite was rerun with localhost fixture access and failed
for the intended missing behavior.

The SHA-256 manifest `freeze.sha256` freezes the tests and this document. Product
implementation may proceed after this freeze; any test correction must be
independently explained and recorded with a new manifest.
