# Initial fixture correction

The first draft reached task generation but failed because its inline shell
command did not produce the copied worker-task file. That run is a fixture
failure, not behavioral RED for the requested enum guidance.

The independent writer moved the same finite script into a standalone executable.
The worker now copies the exact generated task and settles before assertions run.
The test then reaches the intended failure on the missing owner_question name.
No production code, enum validation or existing frozen packet changed.

The initial draft and its original manifest remain in initial-fixture/test.rs
and freeze.v1.sha256. The active freeze records the corrected fixture and this
explanation before any generator change. The sanitized initial failure is kept
separately from the behavioral RED log.
