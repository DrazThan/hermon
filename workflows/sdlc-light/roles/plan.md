# Plan a light independent RED-to-ready run

Read the ticket, repository instructions and relevant domain specifications. Produce plan.md, test-brief.md, impl-brief.md and test-expectations.json beside result.json. The complete plan will appear in the draft PR; planning advances directly to independent RED without an approval wait.

Define observable new behavior, scope, acceptance criteria and exact globally unique test IDs. test-expectations.json contains new_tests and baseline_failures arrays; select a focused baseline-clean command and keep baseline_failures empty. These IDs become immutable authority, not suggestions to rename during implementation. Specify dedicated tests paths and production paths with no overlap; do not allow build-security or workflow files implicitly. Describe docs owed and any capability gaps honestly.

The test writer establishes failing acceptance independently before implementation. Production and any later test amendments must be separate commits. Full-suite authority is the selected GitHub Actions jobs on the exact draft head. Scope questions before GREEN can return through bounded planning; post-GREEN scope changes require a newly scoped run. Do not start other steps or publish through a private identity. Report blocks through result.json.
