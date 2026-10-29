# Independent Slice 5 validation

After production fixes in `1159d41`, the independent test writer ran:

`cargo test -p hermon-flow --test checks --test checks_regressions --locked`

Result: **28 passed, 0 failed**: 17 corrected acceptance tests and 11 separately
frozen supplemental regressions. `final-green.txt` records this independent
replay. Both active SHA-256 manifests (`freeze.sha256` and
`supplemental-freeze.sha256`) match their inputs. The two genuine original
fixture defects and superseding freeze are documented in `acceptance.md`; the
original manifest, RED and first implementation's 15/17 result remain available.
No acceptance assertion was weakened to accommodate production behavior.

The matching Clippy command with `-- -D warnings` passes (`final-clippy.txt`).
These checks used the isolated Rust/Cargo toolchain under
`/private/tmp/hermon-116-rust/` on macOS on 2026-10-29. Process checks used host
access because the sandbox cannot make the `ps` observations needed to verify
owned group settlement. Tests run synthetic local git repositories and commands;
they contact no vendor agent or external service and signal only test-owned
process groups and their explicit sentinel.

Independent production review found no remaining material defect in this tested
scope. New evidence requires the same successful step/attempt context, clean
worktree, bound branch and HEAD, and verifies the current bytes of referenced
logs before publication. The frozen authority checks its requested evidence kind,
branch, ticket/base, target commit, full base-to-RED path delta, ancestry and
agreement between the annotation and explicit git note. Existing evidence keys
remain immutable and recover a missing explicit note without moving the tag.
Libtest parsing now streams all targets, retaining bounded lines and failure
identifiers; JSON parsing uses the bounded final report line. Neither silently
substitutes exit status for identified new failing tests.

The real hub integration test reaches done and records real evidence through
its `Git` adapter. Command fixtures cover every built-in, direct argv, configured
environment and relative executable resolution, dirty mutation refusal, timeout
and descendant settlement with an unrelated process left running. Evidence
fixtures cover under-limit annotation size, stdout/stderr log retention and
SHA-256, test counts, superseding RED, receipt revocation and crash recovery.

Project config loading, intake and user-facing hub hosting remain later slices.
The host must provide correct independent new-test and baseline-failure IDs,
hold its run lock, prevent competing worktree edits and retain referenced logs.
Evidence publication to a remote must include annotated tags and
`refs/notes/hermon` explicitly. Workspace-wide validation and cross-platform CI
are handled by the integrating implementer.
