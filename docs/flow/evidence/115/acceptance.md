# Frozen acceptance checklist — Hermon #115 (Slice 0)

Frozen before evidence implementation, 2026-10-29, by an independent test writer.
Authority: issue #115 and the Slice 0 / section 9 requirements in
`.hermes/plans/2026-10-29_123933-bot-pipeline-orchestrator.md` (overall plan #114).
Target: `docs/flow/agent-contracts.md`; docs-only, no product-code changes.

Assessment rule: each item below must be accounted for explicitly. A blocked
probe may produce an honest, reviewable evidence document but does not establish
the missing capability. A capability is verified only by an observed probe;
source inference and unexecuted example commands must never be called observed.

- [ ] **A1 — Provenance and reproduction.** Record probe date/platform, resolved
  executables and exact installed versions for Hermes, Codex and Claude. Include
  Hermes source revision if inspected. Every claimed live observation has an
  exact command, relevant task/fixture contents and working-directory/input
  conditions, stdout/stderr evidence (or an exact labelled excerpt with a durable
  artifact reference), and actual process return code. Redact credentials without
  removing evidence needed to interpret the result. Do not substitute generic
  reference documentation or an arbitrary current release for the installed CLI.
- [ ] **A2 — Hermes headless file contract.** Show the installed equivalent of
  `hermes -p <profile> chat -Q --query-file <task.md>` with closed/noninteractive
  stdin. Record whether it completes without a terminal or interactive prompt,
  reads the task file, and executes in the specified work directory. Identify the
  profile used; show evidence that its skills and MCP configuration are available
  to that invocation. Distinguish config discovery from actual skill/MCP use;
  describe a harmless probe or explicitly mark actual use blocked/unverified.
- [ ] **A3 — Session capture and continuation.** For Hermes, record the exact
  parseable output field/line and example session ID from a live invocation,
  including stdout versus stderr and chosen output mode. Attempt
  `--resume <session>` combined with `-Q --query-file <new-task.md>`; demonstrate
  continuity using information from the first task that is absent from the next
  task, plus session identity. If output lacks an ID, document the limitation and
  any alternative lookup as a separate, qualified mechanism, not CLI ID output.
- [ ] **A4 — Codex and Claude equivalents.** Independently account for A2/A3's
  semantics for `codex exec` and `claude -p`: exact installed argv and file-input
  method, headless execution, work directory, identity/configuration scope,
  available skills/MCP, machine-readable session-ID capture, and exact resume
  invocation with continuity evidence. Explain unsupported profile concepts or
  input differences instead of assuming Hermes flags are portable. State when a
  shell wrapper is required rather than an argv-only command template.
- [ ] **A5 — Exit outcome matrix.** For each of the three CLIs, include distinct
  rows for success, a genuine model/provider error, SIGINT, and a supervisor
  timeout. Each row is labelled observed, source-inferred, or blocked, with the
  command/fixture, relevant output, and actual return code or explicit absence of
  measurement. Do not classify authentication/configuration/argument errors as
  model errors. For SIGINT, state which owned PID/process group received it and
  distinguish a signal-terminated child from a shell's `128 + signal` convention.
  For timeout, give the supervisor deadline and signal/escalation policy and
  distinguish its own timeout status from the child's status; do not present a
  wrapper's exit code as a native agent timeout contract.
- [ ] **A6 — Evidence honesty and missing capabilities.** Clearly distinguish
  observed behavior, installed-source inference (with version/path/line or other
  precise reference), and blocked/unexecuted cases everywhere, including summary
  tables. Name blockers and the minimum follow-up probe. A probe killed before
  its first model response establishes only startup/termination facts; an empty
  output file, a run ID, or source inspection alone proves no successful task,
  skill/MCP execution, resume continuity, or model-error behavior. Capture material
  limitations without extrapolating from one CLI or output mode to another.
- [ ] **A7 — Section 9 decision and scope.** Include an explicit section 9 adapter
  disposition, and update the local plan's section 9 before Slice 3 wherever
  observed gaps or unresolved contract assumptions require it. Cover the exact
  supported Hermes start/resume argv, optional/missing session IDs and capture
  mode, nonzero/zero error caveats, supervisor-owned timeouts, and whether command
  templates need shell/stdin support. State the fresh-session-with-prior-context
  fallback if resume or ID capture is unavailable/unverified. Leave unverified
  integrations gated; do not retain an unconditional claim contradicted by the
  spike. If no edit is needed, justify that conclusion against observed evidence.
- [ ] **A8 — Deliverable and workflow.** The final result is a short, navigable
  evidence document at the target path, with all three agents and all required
  outcomes accounted for and links/references sufficient to reproduce probes.
  Only documentation/evidence changes are in scope. Freeze this checklist and
  its hash before implementation; independently validate the final document
  against these unchanged criteria. Report capability completeness separately
  from document/checklist completeness, listing any remaining blocked probes.
