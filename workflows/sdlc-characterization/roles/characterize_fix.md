# Correct frozen characterization

Read the approved plan, test brief, accepted characterized reference and block_reason. Reproduce the reported test or fixture defect. Change only the smallest justified test-only correction, preserving the approved behaviors and exact required assertion IDs. Do not change production code or weaken an assertion to obtain a pass.

Execute lint and the configured tests; all required assertions must execute once and pass. Commit the correction and explain its cause and exact changes. The hub retains the original evidence, writes a superseding characterized tag and reruns verification. Corrections are bounded by max_runs. An unresolved defect remains a block; a change to behavior or scope uses scope_change and requires a revised plan and fresh Owner GO.
