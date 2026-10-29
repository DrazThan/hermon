# Slice 5 independent acceptance freeze

The independent test writer authored `crates/hermon-flow/tests/checks.rs` and
`tests/fixtures/check.sh` from issue #120 and plan #114, section 8 before the
checks adapter was implemented. `freeze.sha256` freezes both files. The public
`checks` API was introduced as behavior-free stubs. Production implementation
must preserve the frozen inputs; genuine test defects return to the independent
test writer for a documented correction and superseding freeze.

Behavioral RED: `cargo test -p hermon-flow --test checks --locked` compiled and
ran **17 tests: 3 passed, 14 failed**. `red.txt` retains the complete output.
The three initially passing tests exercise fail-closed behavior (missing freeze,
missing new-test identity and a rename outside the allowed paths), which the
unconditional error stub already satisfies. All substantive success behaviors
and the timeout/capability classifications fail against those stubs.
`cargo clippy -p hermon-flow --test checks --locked -- -D warnings` passes
(`red-clippy.txt`). These checks used the existing isolated Rust/Cargo toolchain
under `/private/tmp/hermon-116-rust/` on macOS on 2026-10-29.

## Accepted behavior

- `GitChecks` implements the real hub `Git` seam and is bound to a worktree,
  base commit, ticket and run branch. Named path sets match repository-relative
  globs. Renames include their removed source path, so moving a source file into
  a tests directory cannot make it pass a tests-only path restriction.
- RED evidence freezes every path changed from the configured base. A later
  source-only implementation passes `frozen_unchanged:red`; editing a frozen
  path fails, including dirty edits. Missing freeze references fail closed.
- `lint`, `project_check` and `cmd:<name>` require zero command exit status.
  Commands receive direct argv, the configured worktree and explicit environment.
  Configured `PATH` controls executable resolution; a missing binary or absent
  search path yields a `capability` block, never a successful RED check.
- `tests_fail` runs the named `test` command and requires nonzero status plus
  parseable test failures containing every expected new test ID. New test IDs
  are supplied by the host through `AttemptContext` and must not already be in
  its baseline-failure set. An empty new-test set, old-only failures, a compiler
  error, a success exit, malformed counts and duplicate failure IDs cannot
  establish RED. JSON reports use `{passed,failed,ignored,failures}` on the final
  stdout line; Rust libtest text is also supported. JSON failed count equals
  the number of unique failure IDs and is positive.
- Check deadlines are separate from agent deadlines. Timeout returns a transient
  block within a bounded interval and cannot publish successful evidence.
- Successful evidence is an annotated tag `hermon/<ticket>/<tag>` for revision 1,
  then `.2`, `.3`, etc. Its JSON annotation and explicit `refs/notes/hermon` note
  on the tag object agree. JSON contains schema, step, attempt, bot, optional
  model, commands, exit codes, test counts, durations and frozen paths. Long
  stdout/stderr remains in an attempt log referenced by path and SHA-256.
  Notes stay below 8192 bytes; oversized metadata fails before publishing a tag.
- A new tag requires a successful check receipt at the same HEAD and bound run
  branch. A failed recheck revokes an earlier pass. Existing `(tag,revision)`
  evidence is idempotent across adapter restart and never moves after HEAD
  advances. A successful correction creates a superseding RED revision while
  the original remains available.

The fixture repositories, commands and logs are synthetic and local. No vendor
agent, external project, network API or user git tag is touched. User-facing
project loading and hub hosting remain later delivery slices; their host must
supply accurate attempt metadata and expected new/baseline test IDs.
