# Independent characterization acceptance

This packet was written from the requested contract and public test interfaces
without reading the characterization implementation. The new files and this
contract are frozen before execution. The same files will run first against
pre-feature commit 0852a5c in an isolated source archive, then against the current
branch. Existing tests and production files are unchanged.

The GitChecks tests require every independently expected ID to occur once and
pass. Missing, skipped, pending, todo, failed and duplicate expected IDs fail.
Counts-only or incompatible formats, inconsistent totals, suite/runtime errors,
nonzero or signal exits, empty expectations and declared baseline failures fail.
Successful evidence retains only the required passing subset in
commands[0].required_passed_tests, and stays below the existing note-size bound.
The tests also check test-path ownership, the accepted freeze and strict RED's
continued requirement for actual expected failures.

The synthetic host tests run real intake, detached fake workers and checks with
a loopback tracker. They prove tests_pass consumes the accepted planner artifact
without manual state edits, ignores a newer unaccepted planner directory and
rejects unrelated passing names in place of a planned ID. The helper has no live
vendor, tracker or project context and uses only public fixture identifiers.

The shipped-pack tests require a separate sdlc-characterization workflow with
Owner GO, test-only characterization and verification, frozen tests, bounded test
corrections and review, a reviewer from a different family, PR publication,
owner merge and Docs. The original sdlc workflow must retain tests_fail and its
RED freeze. Role guidance and validated owner-mention templates must be usable.

Raw execution logs stay in ignored local scratch storage. Public evidence will
replace operator and temporary checkout paths with neutral labels while keeping
commands, test names, failures, counts and exit codes. All fixture and packet
commits use the public DrazThan noreply identity. No live issue, session or
operator metadata belongs in this packet.
