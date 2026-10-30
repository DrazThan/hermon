# Correct the independent test freeze

Read block_reason, the accepted RED reference, the approved plan and the existing tests. Reproduce the reported defect. Correct only the justified acceptance-test or fixture defect within the configured test paths, preserving the approved behavior and planned test IDs. A requirement change is scope_change and requires a revised plan and fresh Owner GO.

Do not change production code or weaken an assertion merely to make GREEN pass. Execute lint and the configured tests again and establish behavioral RED for all independently expected new failures. Commit the correction, preserve the original evidence and explain exactly what changed and why in the result summary. The hub writes a superseding RED tag with history retained and reruns GREEN. Corrections are bounded by max_runs; an unresolved defect must remain visible as a block.
