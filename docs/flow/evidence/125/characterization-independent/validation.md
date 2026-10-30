# Independent characterization results

The same frozen packet ran first against pre-feature commit
0852a5c in an isolated source archive, then against current commit 6009e18.
No production code was read until the inputs were frozen. No test changed after
that freeze.

The baseline produced 11 passes and six behavioral failures. The passing-check
adapter rejected the unknown tests_pass check, the new pack was absent and
intake rejected tests_pass before creating a run. All tests compiled; the
failures were missing behavior rather than build errors.

The current branch passed all 17 tests:

| Suite | Baseline | Current |
|---|---|---|
| Named passing checks | 10 passed, 2 failed | 12 passed |
| Separate pack and strict RED | 1 passed, 1 failed | 2 passed |
| Accepted planner IDs through intake/host | 3 failed | 3 passed |

Each suite ran with `cargo test`, `--locked` and `--offline`. `results.json`
records the exact arguments, commit IDs and exit codes. `red-*.txt` and
`green-*.txt` preserve test names, failure reasons and counts. Operator, scratch
checkout and temporary paths were replaced with neutral labels; raw logs remain
in ignored local storage. Only synthetic public fixture identifiers appear here.

After execution I reviewed the changes to the check, report parser, workflow
loader, doctor and host adapter. Required IDs are checked against named passing
assertions and rejected when ambiguous. Summary counts and suite/runtime errors
remain checked. The evidence note keeps only the required subset. The host reads
the accepted planner artifact for tests_pass, and the separate pack preserves
owner gates, test ownership, frozen tests and bounded independent review.
The original strict RED path remains separate. No material finding was identified
within this acceptance contract, and no production fix was needed.

This independent packet is separate from the earlier 14 feature tests and the
prior full-workspace result of 1021 passed with three ignored. The root agent
also ran formatting and strict workspace all-target Clippy with this packet
present; those are separate checks, not additional independent test cases.
No live issue, operator context, session, tracker or vendor store was read or
changed by this packet. All new fixture repositories use the public noreply
identity.
