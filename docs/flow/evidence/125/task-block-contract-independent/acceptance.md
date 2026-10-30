# Worker-facing block-kind contract

The independent test runs the real host with a synthetic queued run and a finite
fake worker. The worker copies the task it actually receives before completing.
That text must match the generated task and explicitly include owner_question,
test_defect, capability, transient, scope_change and unknown.

The fixture has no role or ticket text that could supply those names. It uses
public synthetic identifiers, no live tracker and no vendor process. It does not
change result validation or any existing frozen test. The test is frozen before
the generator changes. Raw logs remain in ignored local storage; public evidence
uses neutral checkout and temporary-path labels.
