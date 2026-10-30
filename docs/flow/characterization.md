# Test-only characterization

`workflows/sdlc-characterization/workflow.yaml` verifies existing behavior with
the same five SDLC profiles. Select this pack at intake for a new run. It does
not change `workflows/sdlc`, relax `tests_fail`, or replace an existing run's
immutable workflow snapshot.

The path is Plan → Owner GO → Characterize → Verify → Review → PR → Owner merge
→ Docs. Plan executes the baseline and independently names every required new
passing assertion in `test-expectations.json`:

```json
{"new_tests":["reports populated results preserve pagination"],"baseline_failures":[]}
```

The historical `new_tests` field identifies the required passing assertions in
this pack. It identifies expected failures in the original RED pack. Required
IDs must match Vitest `assertionResults[].fullName` exactly and be unique across
files. The host reads the accepted planner attempt, never a newer unaccepted
directory or a test worker's report. Changed acceptance requires revised Plan
and fresh Owner GO.

Characterize and its bounded correction step change only configured test paths,
pass lint and satisfy `tests_pass`. The characterized tag freezes the cumulative
test delta. Verify preserves that freeze, enforces test-only paths, reruns
`tests_pass` and executes the full `project_check`. Verification can retain the
same commit. A defective frozen test routes through `characterize_fix`, at most
twice, retaining prior evidence. Production defects and necessary scope changes
return through Plan and a fresh Owner GO; this pack does not authorize their
repair.

Review uses a different known model family from Verify and has at most two
rounds. Review must inspect the **complete intake-base-to-candidate diff**:
the characterized-to-verified diff can be empty. PR publication and verified
owner merge retain their existing gates. Docs follows verified merge.

## Named passing evidence

`tests_pass` runs `commands.test` and currently requires `report: VitestJson`.
Doctor rejects a missing test command or an incompatible report format before
intake allocates a run. The check requires exit zero, explicit report success,
zero failed assertions/suites, consistent counts, no runtime/collection errors,
nonempty independent expectations and an empty baseline failure set.

Every required ID must occur exactly once with status `passed`. Missing,
duplicate, skipped, pending, todo or failed required assertions block. Unrelated
baseline skips may remain disclosed; they do not satisfy required IDs. Duplicate
unrelated passing names are allowed because existing projects may reuse suite
names across files. Json and Libtest counts cannot establish this gate.

Evidence notes persist `required_passed_tests`, the required subset only. Full
reports stay in hashed command logs; thousands of baseline IDs do not inflate
the bounded git annotation. The original failure-report schema and strict RED
acceptance remain intact.

Keep live issue context, session receipts, credentials and operator paths in
ignored local evidence directories. Public examples and tests use synthetic
identifiers. See [publication policy](evidence/125/publication-policy.md).
