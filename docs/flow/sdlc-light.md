# sdlc-light: independent RED, early CI, reviewed test amendments

Choose `workflows/sdlc-light/workflow.yaml` for new behavior needing an independent failing baseline and early GitHub Actions feedback, while allowing explicitly reviewed test corrections. Choose `sdlc` instead when Owner GO, frozen independent tests and a local `project_check` are required. Choose `sdlc-characterization` for test-only characterization of existing behavior. Existing strict/characterization packs and running snapshots are unchanged; CI-gate adoption for those packs needs separate acceptance work.

## Intake and project configuration

From the Hermon checkout, with an existing project file and authorized tracker ticket:

```sh
hermon doctor workflows/sdlc-light/workflow.yaml --project /path/to/project.json --ticket TEAM-123
hermon run workflows/sdlc-light/workflow.yaml TEAM-123 --project /path/to/project.json --confirm
hermon hub --once
hermon status TEAM-123
```

Use the existing five bot/profile names: `sdlc-planner`, `sdlc-test-writer`, `sdlc-implementer`, `sdlc-reviewer`, `sdlc-docs`. Configure real completion protocols and known model families. The reviewer must have a different runtime family from the implementer, not merely another alias. Intake snapshots the five role files and templates. No light profile-skills, red_fix role or GO template is shipped; the unused `go_request.md` still resolves from the compiled template fallback.

Configure the project with dedicated, disjoint path ownership and a focused test command. The following is a fragment, not a complete project file:

```json
{
  "paths": {"tests": ["tests/**"], "production": ["src/**"]},
  "commands": {
    "lint": {"argv": ["cargo", "clippy", "--locked", "--", "-D", "warnings"], "timeout_seconds": 120},
    "test": {"argv": ["cargo", "test", "--locked", "--test", "acceptance", "--", "--color", "never"], "timeout_seconds": 120, "report": "Libtest"}
  },
  "code_host": {
    "repo": "owner/repository",
    "remote": "origin",
    "ci": {"selected_checks": ["exact-job-name-from-authorized-check-runs"], "poll_interval_seconds": 30, "no_checks_timeout_seconds": 600, "timeout_seconds": 3600}
  }
}
```

For Vitest, an alternative focused command is `{"argv":["npx","vitest","run","tests/acceptance.test.ts","--reporter=json"],"timeout_seconds":120,"report":"VitestJson"}`. Configure installed executables in the project's complete environment, including PATH; Hermon does not inherit arbitrary host environment into workers. Do not put credentials in task files, logs or committed examples.

`tests_pass` accepts VitestJson or ordinary pretty Libtest (also the implicit default), never counts-only Json. Every planned ID must execute and pass with exit 0 and reconciled summaries. Libtest needs exact `test <ID> ... ok` lines with globally unique IDs; quiet dots, summary-only output, ANSI-wrapped output, missing/renamed/ignored IDs, duplicate expected IDs and failed targets cannot establish named passes. Vitest needs the complete JSON reporter document and named assertions. The planner supplies a nonempty `new_tests` array and empty `baseline_failures` in accepted `test-expectations.json`; a worker-written or newer unaccepted planner directory cannot redefine that authority. Select a baseline-clean focused scope. Historical failures outside it belong to CI, not a local waiver.

Doctor probes only commands used by workflows with `github_ci`; an unused local `project_check` is not invoked. It checks light path prerequisites and the passing reporter. Before a live pilot, obtain exact selected job names from authorized GitHub check-runs output, and verify those jobs run on drafts and report the PR head SHA. The placeholder above is not a verified job name.

## Lifecycle and ownership

The bounded graph is plan → red → green → draft_pr → ci → review → ready → merge → docs. Planning may be visited twice, GREEN four times and review two rounds; check retry is distinct from a visit. RED can route a pre-GREEN scope change to planning. After accepted GREEN, scope changes require a newly planned run, never a replacement RED on implementation-bearing code.

The host accepts independent tests-only RED after lint and genuine expected assertion failures, then pins its annotated tag object, commit, run/base/branch, path sets, canonical expectation digest and note digest outside the worktree. It does not rewrite RED or rerun tests_fail on the implementation to bless corrections. `red_guard:red` is explicit opt-in; strict `frozen_unchanged` remains strict.

Every post-RED commit must be linear (no merge commits). A changed path must belong to exactly one configured set. Each commit is entirely test-only or entirely production-only; mixing then reverting still fails. Empty commits are harmless. Inline unit tests do not make an entire production file an amendable test path. Do not allow workflow, build-security or hidden fixture paths by overly broad production globs.

After focused GREEN checks, `open_draft_pr` pushes only the accepted branch SHA and opens or updates the same draft. Its managed body contains the complete accepted plan, exact candidate, RED/GREEN references, Review pending and amendment warning. Owner text outside the delimited managed region is preserved. Tags and notes remain local; they are never pushed. Only schema-2 markers identify this lifecycle; legacy PR markers cannot downgrade it to schema-1 behavior.

`github_ci` waits silently on the exact pushed head. Policies are intake-pinned in `context/ci-policy.json`. Check selection uses exact names and the highest check-run ID per name across all pages (`filter=all`); only GitHub Actions app runs with matching head SHA and success authorize passage. Missing jobs wait until the missing-check deadline, pending jobs until the overall deadline; skipped, neutral, cancelled and failed selected jobs do not count as success. Failures route back to GREEN with job names and links, then fast-forward the same PR without force. CI does not create an owner approval wait or a pending-CI desktop alert.

## Amendment review schema

The workflow opts in explicitly:

```yaml
review:
  bot: sdlc-reviewer
  inputs: [plan.md, "diff:red..green", amendments]
  require_different_family: green
  review_amendments: {red: red, green: green}
  max_rounds: 2
  on_verdict: {changes_needed: green}
  next: ready
```

Whenever an `amendments` input is requested, the host writes `amendments.diff` and `amendments.json` beside that attempt's task.md and includes the SHA-256 digest in the task. The deterministic diff is the complete original-RED-to-candidate delta restricted to configured tests paths: no renames, external diff or textconv, with binary data, full object IDs and a 1 MiB limit. The compact receipt binds original RED proof, red_commit, candidate_sha, paths, diff_sha256 and empty. `candidate.diff` separately contains the verified RED-to-GREEN full delta. There is no per-commit patch archive; history classification still covers net-zero mixed edits.

The reviewer must not edit the candidate. After inspecting the exact candidate and test semantics, a nonempty accepted amendment requires:

```json
{"schema":1,"status":"done","summary":"Reviewed implementation and test corrections","artifacts":[],"block":null,"verdict":"clean","questions":[],"accepted_amendments":"<exact host diff SHA-256>"}
```

Replace the placeholder with the actual digest from task.md. A generic clean, wrong/stale digest, free text or worker-authored diff cannot authorize ready. Missing/wrong acceptance is an unknown semantic check failure: one retry, then block. Empty net diffs need no affirmative acceptance (the matching digest is allowed). `changes_needed` routes to GREEN within the review bound. An acceptance field is rejected on non-opted-in steps.

GREEN binds the diff receipt in its evidence note, revalidated against the pinned RED and recomputed diff on recovery and review. The run's accepted candidate and independent review receipt carry the same amendment proof. `ready_pr` needs current candidate identity, independent clean review and fresh selected CI success; the durable schema-2 ready receipt carries accepted/empty plus digest, not not_applicable for a light run. Missing required proof is not an exemption.

## Restart, failures and owner merge

Use status and the run's event journal to inspect waits, blocks and exact candidate identity. Fix capabilities honestly, then use the CLI's explicit retry operation with a reason; do not edit run.json, tags, notes or receipt files to advance. Git proof pins and check logs are local recovery authority: preserve the run store and repository together. Evidence notes remain bounded; full diffs live outside the worktree. Overflow and inconsistent/missing proof fail closed.

Publication and readiness mutations are followed by exact-target readback, including after a lost response. Restart the hub normally; a PR already marked ready for the persisted exact authorization is reconciled without another PR. A worker's summary or artifacts cannot fabricate checks. CI gates cannot be manually approved or overridden; draft and ready lifecycle actions cannot be overridden. Overriding another step never manufactures the evidence that downstream authorization requires.

Only the owner merges in GitHub; Hermon never invokes `gh pr merge`. Schema-2 owner_merge checks the finalized ready receipt, exact merged PR head, head-side parent for a two-parent merge and merge-commit ancestry on the configured base. A later external push cannot inherit an earlier review or authorize Docs, even if its commit reaches base. Successful verified merge supplies `context/merge.json` to Docs and publishes one final outcome; merge is not a claim of deployment.
