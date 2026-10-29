# Independent Slice 4 validation

After the final production fixes, the independent test writer ran:

`cargo test -p hermon-flow --test agent --test agent_regressions --test agent_launch_failure --locked`

Result: **17 passed, 0 failed**, recorded in `final-green.txt`. The original
14-test suite and all three supplemental regressions pass. All inputs match
their original, supplemental and launch-failure SHA-256 manifests. No frozen
acceptance test or fixture needed correction or weakening.

The matching Clippy command with `-- -D warnings` passes (`final-clippy.txt`).
These checks used the isolated toolchain in `/private/tmp/hermon-116-rust/` on
macOS on 2026-10-29. Fixture-process tests used host access because the restricted
sandbox does not permit the `ps` observations necessary for group-settlement
verification. They ran only synthetic agents and test-owned process groups.

Independent production review found no remaining material defect in the tested
scope. The worker retains process-group ownership until all group members have
settled, preserves deadline/cancellation outcomes despite a later zero exit,
separates process success from protocol completion and result validation, and
persists optional session identity. Lifetime locks avoid trusting reused PIDs;
dead supervisors retain unresolved handles and block duplicate work. The
supplemental RED fixes discard stale completion evidence, resolve relative
executables in the worktree and leave known pre-spawn failures retryable.

Actual Hermes launch remains capability-gated by the Slice 0 shared-gateway
finding. Tests verify exact accepted argv and stderr session parsing, without
claiming that a CLI viewer's death establishes gateway settlement. Host-level
reconciliation of uncertain launches, tracker/code-host integration and live
vendor-agent operation are outside this fixture acceptance scope. The integrating
implementer separately runs workspace-wide checks and CI.
