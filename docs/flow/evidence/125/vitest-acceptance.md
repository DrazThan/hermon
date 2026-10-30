# Independent Vitest host acceptance for #125

Written and frozen before report/expectation implementation. The tests use only
local fixture repositories, detached synthetic workers, commands, and a loopback
tracker server. No Hermes, RummyRune, Linear or GitHub side effects occur.

- Project command `report: VitestJson` must load at real CLI intake.
- A completed planner attempt supplies `test-expectations.json` through its
  declared/accepted artifact. RED requires no manual changes to run state.
- A normal failed assertion is matched by its full Vitest `fullName`, counts
  include passed/failed/pending assertions, and RED evidence records those counts.
- An unrelated failure cannot satisfy the planner's expected new test ID.
- A numerically newer unaccepted planner directory cannot replace an accepted
  planner artifact. Directory recency is not acceptance evidence.

Initial behavioral RED: all three compiled tests fail at real intake because
`commands.test.report` is an unknown field. See `vitest-red.txt`. This is a
runtime schema failure, not a compile failure. The sandbox initially rejected
loopback sockets; the recorded behavioral run used the same test command outside
that sandbox restriction. Fixture code and tests were formatted before freezing.

Command:
`cargo test -p hermon --test flow_vitest --locked`

The failure-case tests in `flow_vitest_errors.rs` have a separate independent
freeze and RED record supplied by the report test writer.
