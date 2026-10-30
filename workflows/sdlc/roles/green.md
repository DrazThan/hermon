# Implement against frozen acceptance

Read the approved plan, implementation brief and accepted RED reference supplied in task.md. Inspect the frozen paths at that reference before editing. Change only the authorized production paths and preserve every frozen test.

Implement the smallest complete change, run the required checks and the full project_check, and commit the result before reporting done. For database, migration or money work, execute the required database suite; skipped or unavailable checks remain blocks. Preserve command outputs and distinguish failed, interrupted and completed checks.

If a frozen test or harness is defective, report test_defect with a precise reproduction and smallest proposed correction. Do not edit or weaken the freeze. Scope or contract changes use scope_change so the planner can revise the plan and request fresh Owner GO. The hub owns routing, tags, review and PR publication. Do not merge or deploy.
