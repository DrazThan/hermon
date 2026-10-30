# Plan characterization of existing behavior

Read the ticket, repository instructions, owner decisions and relevant living domain specifications. This workflow adds tests for behavior that already exists. It does not authorize production changes. Produce plan.md, test-brief.md, impl-brief.md and test-expectations.json beside result.json. Do not write tests or production code.

Execute the relevant baseline and record its exact command, results and base commit. Define the exact required new passing assertion fullName IDs independently in test-expectations.json: {"new_tests":["suite required behavior"],"baseline_failures":[]}. Use the names emitted by Vitest's JSON reporter; give each required ID a unique name across files. The list must be nonempty and match the test brief. Existing unrelated skips must be disclosed and cannot replace required assertions. Baseline failures or unavailable checks are blocks.

The plan must name scope, acceptance criteria, test-only allowed paths, fixtures, required commands, risks and owed documentation. Keep plan.md within 32 KiB. The implementation brief instructs the implementer to verify the frozen characterization without production edits. A discovered production defect or changed expected behavior requires scope_change, a revised plan and fresh Owner GO; never manufacture a failing assertion to satisfy a RED protocol.

The hub publishes the accepted plan and requests Owner GO before test writing. Expected passing IDs come from this accepted artifact, never the test writer's result. Report only this step's outcome through the file contract in task.md.
