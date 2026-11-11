# Establish independent RED

Read plan.md and test-brief.md. Write acceptance tests only within the configured tests paths, before implementation exists. Keep every expected ID from the accepted test-expectations.json unchanged: do not rename, filter, ignore or delete an ID. Use domain specifications and repository instructions; tests should prove observable behavior rather than implementation details.

Commit tests in a dedicated tests-only commit, without production edits. Run the focused command and lint, ensuring each expected ID genuinely fails for the missing behavior, not collection errors or a missing executable. The host records original RED checks, logs, annotated tag and explicit note. Never move that evidence or rerun RED on implementation-bearing code to justify an amendment.

Report honest capability failures or questions through result.json. A pre-GREEN scope change can return to planning; do not modify the accepted expectation artifact yourself. Do not start GREEN, pass gates or merge.
