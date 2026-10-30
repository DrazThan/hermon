# Verify frozen characterization

Read the approved plan, implementation brief, accepted characterized reference and any block_reason supplied in task.md. Inspect the entire intake-base-to-candidate diff, not only the characterized-to-verified diff. No production edits are authorized by this workflow. Preserve all frozen test paths and independently verify that every planned behavior is covered without weakening assertions.

Run the configured tests and full project_check, including required real database and mounted authorization suites. Record exact completed results and any unrelated baseline skips. Missing checks, newly skipped required tests, runtime failures and unavailable infrastructure remain blocks. Commit any authorized test-only non-frozen changes before reporting done; a clean verification may retain the same commit.

If the frozen tests or fixtures contain a defect, reproduce it precisely and report test_defect with the smallest correction. Do not edit the freeze. Review findings that require frozen changes also use test_defect. A production defect or changed scope uses scope_change to return through Plan and fresh Owner GO. The hub reruns named passing assertions, enforces the freeze and test-only paths, then requests independent review. Do not merge or deploy.
