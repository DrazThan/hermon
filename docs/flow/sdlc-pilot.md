# SDLC pack, migration and guided pilot

The shipped pack is `workflows/sdlc/workflow.yaml`. The RummyRune project file is
`projects/rummyrune/project.yaml`. It is an owner-local configuration, with
explicit paths, qa as the default base, an oztal000 branch prefix, the five
named Hermes profiles, the RUM owner identity and the central Linear token's
**environment-variable name**. It contains no credential. Use `--base staging`
when the owner selects staging for a refactor; base is a string in the current
schema, not the mapping from the original proposal.

The explicit ACP example is `projects/rummyrune/pilot.yaml`. It preserves
the five role names and contains placeholders for operator paths, owner
identity and the repository. Prepare the actual configuration locally.
Keep live issue context, session receipts and authentication state outside
the public repository. Fixture tests and capability checks do not establish
an owner approval, live workflow, merge or Docs success.

## Pack contracts

Plan produces `plan.md`, `test-brief.md`, `impl-brief.md` and
`test-expectations.json` beside its attempt's `result.json`. The plan names exact
new acceptance test IDs and verified baseline failures, for example:

```json
{"new_tests":["rules.test.ts acceptance rejects invalid moves"],"baseline_failures":[]}
```

For Vitest, IDs must match `assertionResults[].fullName` from the JSON reporter,
including its file/suite prefixes. The independent planner specifies these IDs
before the independent test writer writes RED. The host reads the accepted
planner attempt, not a RED result or the highest numbered directory. A changed
acceptance contract returns through Plan and fresh Owner GO. No owner edits to
run folders or expectation files are part of the shipped SDLC path.

RED and red_fix modify only the configured test paths, pass lint and establish
all independently expected new assertion failures. GREEN preserves the RED
freeze and passes `pnpm check`. Review uses a different known model family,
returns `clean` or `changes_needed`, and is bounded to two rounds. Test correction
is bounded to two attempts and supersedes the RED freeze with history retained.
PR publication uses the intake base; only the owner's verified merge opens Docs.
Correction tasks receive the exact reported defect reason. A superseding RED
checks edits against its recorded attempt-start commit and freezes accumulated
test paths; earlier GREEN production changes are excluded from that freeze.

RummyRune has no root `pnpm test` script. Its configured RED command is
`pnpm exec vitest run --workspace tests/vitest.workspace.ts --no-file-parallelism --reporter=json`.
`commands.test.report: VitestJson` validates the complete JSON report and its
individual assertion counts and full names. Suite/collection/runtime errors,
missing or malformed reports, mismatched counts, a successful exit and unrelated
or baseline-only failures cannot establish RED. Pending, skipped and todo tests remain
ignored evidence. `Libtest` remains the default for older project files;
`Json` supports the existing normalized report contract. Reports must be clean
JSON on captured output and fit the 16 MiB parsing bound.

The ACP pilot configuration separates raw server logs from the JSON report.
It runs the same installed Vitest CLI through Node inside `pnpm exec /bin/sh`, duplicates the report stream
to descriptor 3, redirects normal stdout to stderr, and selects
`--outputFile=/dev/fd/3`. This preserves the complete report and actual exit
status while keeping direct `process.stdout.write` diagnostics out of the parser.
Pass/fail fixtures and a real local-Postgres test verified this configuration;
see [report-stream evidence](evidence/hermes-acp/report-stream.md).

Every bot step has a short `roles/<step>.md` prompt. Intake snapshots readable
role files and their SHA-256 hashes into `context/roles/` and
`context/roles.json` before publishing the run. Existing unreadable, empty,
out-of-pack or oversized files fail intake before worktree allocation. The host
checks the snapshot hash before writing the task. Later source edits affect only
new runs; older packs without role files keep their existing behavior.

The Owner GO comment includes the accepted published plan body (at most 32 KiB),
so the owner can review it in the request thread. Oversized or unreadable plans
block rather than being silently truncated. Verified merge SHA and PR identity
are retained in `context/merge.json`, supplied to Docs and included in the
central final outcome. Outcome delivery retries after restart without duplicate
comments.

All seven comment templates ship under `workflows/sdlc/templates/`. Project
`templates` overrides individual pack files; missing files fall through to the
compiled defaults. Preview with:

```sh
hermon templates preview workflows/sdlc question.md
hermon templates preview workflows/sdlc go_request.md
```

The hub appends its reply marker and supplies the owner mention. Workers report
questions, blocks and results through task.md/result.json; the hub owns tracker
comments, routing, checks, tags and PR publication. Optional per-step comments
retain their existing delivery policy.

## Approved local migration

On 2026-10-29 the owner approved all five profile revisions and the pause of
`SDLC public controller`, job `dcc9fbf66967`. The domain-only sources are
versioned under `workflows/sdlc/profile-skills/<profile>/SKILL.md` and were applied
only to the matching main skill in these profiles:

- sdlc-planner
- sdlc-test-writer
- sdlc-implementer
- sdlc-reviewer
- sdlc-docs

They retain specialist guidance and remove board coordination, release receipts,
checkpoint inventories, publication-authority and tracker helper protocols.
Workflow role prompts now carry the step instructions. Unrelated domain skills,
the orchestrator profile and the preserved plan-review skill were not migrated.

Original files are backed up under
`~/.hermes/backups/hermon-125-2026-10-29/<profile>/SKILL.md`.
The [migration receipt](evidence/125/profile-migration.json) records each original
and installed hash, the exact paused job readback, and matching before/after
hashes for the installed `sdlc-reliability` files (excluding Python caches).
The plugin remains installed and unmodified for read-only historical inspection.
Do not replay, adopt or rewrite its old chains into Hermon. The pause prevents
future scheduling; it does not establish settlement of an already running job.
Historical work must be reconciled before selecting a conflicting pilot ticket.
Restoring a profile uses its exact backup; resuming the controller is a separate
owner decision, not an automatic rollback step.

## Preflight for the guided pilot

Before intake, inspect the owner-selected small RUM ticket, its comments and
relevant Notion Engineering KB Design Specs and Decision Log. Follow the target
repository's AGENTS.md and personal overlay. Confirm one scope, the intended
base and required capabilities. No pilot is selected by guessing a ticket ID.

Legacy `Hermes` bindings retain the shared-gateway cancellation gate. The opt-in
`HermesAcp` binding uses public ACP v1 with a pinned version and current model,
exact-session cancellation and read-only main-call usage verification. Viewer
death leaves an unfinished receipt and retains capacity. It never establishes
settlement. The adapter preserves the selected profile's skills and MCP context.
GREEN and Review use independently verified OpenAI and Anthropic families.
See [ACP configuration and supervision](hermes-acp.md),
[process supervision](process-supervision.md) and the
[live contract probes](evidence/hermes-acp/live-contract.md).

Configure the complete child environment, dependencies, git/gh access and bot
credentials locally. GUI/launchd does not load shell startup files. Supply the
central app token to the host through `HERMON_LINEAR_TOKEN`; never commit it or
put it in task files. Verify the central app can read/write the selected issue,
the configured owner ID is correct, and GitHub can push the run branch and open
a PR. `doctor` executes project checks and bot probes; allow the configured
60-minute full-project check budget. It must pass before intake allocates a run.

```sh
hermon doctor workflows/sdlc --project projects/rummyrune/pilot.yaml --ticket "$TICKET"
hermon run workflows/sdlc "$TICKET" --project projects/rummyrune/pilot.yaml \
  --base staging --confirm --run-id "$RUN_ID"
hermon hub
# Or use hermon menubar with the same data directory.
hermon status "$RUN_ID" --json
```

These examples require owner-local paths and credentials before execution.
With a custom `--data-dir`, pass it consistently to intake, hosting, views and
controls. Only one host may own the directory. Existing monitoring stays usable
with no queued runs.

## Live acceptance record

Record the selected issue, approved base, run ID, configured model families,
preflight results and start time. Preserve run state/events, attempt logs and
small git tag notes as evidence. Keep tokens out of any exported record.

1. Plan reaches Owner GO with its published artifacts and an owner mention.
   Only an exact GO by the configured owner in the request thread passes.
2. Without owner nudges or run-folder edits, RED runs independent tests, GREEN
   passes frozen tests/project_check, Review verifies the candidate and the hub
   pushes the branch and opens a PR against the selected base.
3. Record each block's first observation, route or owner alert and elapsed time.
   Every block must route or alert within `stall_alert_minutes` (30 minutes).
   Capture the tracker comment/event and desktop notification when applicable;
   a muted banner does not establish tracker delivery. Capability failures stay
   visible until fixed and explicitly retried.
4. Owner actions before PR are limited to answering questions and GO. A manual
   retry, override, extra release or state edit needed to advance is a pilot
   finding, not a passing no-nudge run. Inspect pipeline views with TUI `v` or
   GUI Pipelines and confirm the recorded running session opens correctly.
5. The owner merges the PR. The hub independently verifies MERGED and ancestry,
   runs Docs, and publishes the outcome with the merge commit and actual docs
   result. A fixture merge or an unmerged PR cannot satisfy this acceptance.

Useful controls are `answer`, `approve`, `retry`, `override --reason`, `pause`,
`resume` and `cancel`; see [intake and controls](intake-controls.md).
Cancellation only targets owned processes; never stop a shared gateway to make
an unknown outcome look settled. The milestone remains open until this live
record and the owner's merge/Docs outcome exist.
