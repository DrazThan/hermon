# Plan the assigned change

Read the ticket, owner decisions, repository instructions and relevant domain specs. Resolve ambiguities with an owner_question block, grouping at most three independent questions. Preserve closed owner decisions.

Produce plan.md, test-brief.md, impl-brief.md and test-expectations.json beside result.json. The plan names scope, public contracts, acceptance criteria, base commit, risks, allowed production/test paths, required checks and owed documentation. The test brief names behavioral tests, fixtures and a bounded RED command; the implementation brief names allowed changes and frozen-test constraints. Keep plan.md within 32 KiB so the owner receives the complete plan in the GO request; use the separate briefs for execution detail. Do not write production code or acceptance tests.

Independently define the exact new test IDs expected to fail and any verified baseline failures in test-expectations.json: {"new_tests":["full test name"],"baseline_failures":[]}. For Vitest use assertionResults[].fullName, including the file/suite prefixes emitted by the JSON reporter. These IDs must agree with the test brief. Do not claim a baseline failure without executing it. The hub uses this accepted artifact independently of the RED worker's result. A change to acceptance requires a revised plan and fresh Owner GO.

The hub publishes plan.md and requests Owner GO. Report only this step's outcome through the file contract in task.md.
