# Independent Vitest report acceptance

These cases were written before report-format and expectation-flow implementation
for issue #125. They use the shared synthetic CLI fixture in
`crates/hermon/tests/support/vitest_fixture.rs`; fake agents, a temporary git
repository and a loopback tracker replace external services.

The seven tests in `crates/hermon/tests/flow_vitest_errors.rs` require:

- A file-level suite/runtime error cannot satisfy behavioral RED even if an
  expected assertion also failed.
- Collection/import failure without failed assertions cannot satisfy RED.
- Inconsistent assertion counts cannot satisfy RED.
- Exit zero cannot satisfy RED even when the report contains failures.
- An unrelated failure cannot substitute for the expected new test's full name.
- A known baseline failure cannot be relabelled as a new RED failure.
- Pending and todo assertions contribute to ignored counts in accepted evidence;
  nested suite counts may exceed the file-level result count.

The report shape was checked against the installed Vitest 2.1.9 JSON reporter.
Its `testResults[].message` records file-level errors; `numTodoTests` is separate
from `numPendingTests`; full names include ancestor titles. Report fixtures do
not require the owner's RummyRune checkout or installed Vitest.

RED command (with the temporary Rust toolchain environment):

```sh
cargo test -p hermon --test flow_vitest_errors --locked
```

The sandbox run could not open the fake loopback tracker. The rerun outside the
sandbox compiled successfully and failed all seven cases during real CLI intake
because `commands.test.report` is unsupported. This is recorded in
`vitest-errors-red.txt`; no compiler or sandbox failure is claimed as behavioral
RED. The source SHA-256 is frozen in `vitest-errors-freeze.sha256`; the shared
fixture is frozen separately by its independent author.
