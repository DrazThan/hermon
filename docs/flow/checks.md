# Checks and git evidence

Slice 5 supplies `checks::GitChecks`, the real implementation of the hub's
`Git` interface. It runs local git and project commands. Project config loading,
intake and hub hosting are subsequent slices; no new CLI command is introduced.

The host constructs a `CheckConfig` for one run, selecting the worktree, immutable
base commit, ticket, run branch and an attempt log directory outside the tracked
worktree (or ignored by git). The adapter resolves the base once at construction.
It requires a clean worktree and the configured branch when running checks or
creating new evidence. Changes to HEAD or the worktree during checks invalidate
the result. The host must hold its run lock and prevent competing edits while
advancing the run.

Before reconciling an attempt, the host calls `set_attempt(AttemptContext)` with
its attempt number, bot and optional model. For RED, this also supplies the
independently selected new test IDs and the baseline's existing failures. These
are acceptance inputs: the adapter does not infer new test identity from a
bot's exit code. Setting a new context revokes any earlier check receipt. The
hub can then call `check` and `evidence` through its existing `Git` trait.

| Check | Accepted result |
| --- | --- |
| `only_paths_changed:<set>` | Every committed path changed from base to HEAD belongs to the configured named glob set. |
| `frozen_unchanged:<tag>` | The supplied evidence reference belongs to this ticket/base, is an ancestor of HEAD, and all paths in its frozen RED delta remain unchanged. |
| `lint` | The named `lint` command exits zero. |
| `tests_fail` | The named `test` command exits nonzero, with a valid report identifying every expected new failing test. |
| `project_check` | The named `project_check` command exits zero. |
| `cmd:<name>` | The selected named command exits zero. |

Path globs are repository-relative and slash-separated: `*` and `?` match
within one component, and a complete `**` component matches zero or more
components. For example, `**/*.test.*` includes root and nested tests, and
`tests/**` includes everything beneath `tests/`. Absolute paths, parent
traversal, backslashes and bracket character classes are unsupported and rejected.
Renames include both the removed and added paths. NUL-separated git output
preserves spaces, newlines and pathspec metacharacters in filenames; paths must
be UTF-8. Frozen paths are the complete base-to-RED delta, including deletions,
and are verified against the tag's actual commit before use.

Each `CheckCommand` supplies an argv vector and a positive `timeout_seconds`.
There is no implicit shell; a project that needs one must explicitly configure
its executable and arguments. `env` is the entire child environment. Bare
executable names search only its `PATH`; relative paths resolve in the worktree.
Absolute executables need no PATH. A missing executable, invalid argv or missing
command configuration produces a `capability` block. Ordinary check failures
produce `unknown`; deadlines produce `transient`. These classifications feed
the hub's existing retry and owner-alert policies.

Commands run in owned process groups with null stdin and file-backed stdout
and stderr. The shared process supervisor retains the leader's identity until
its group settles, including descendants surviving an otherwise successful
exit. Deadline state cannot become success when a signal handler exits zero.
Each invocation preserves raw output plus a combined log, so long diagnostics
never need to fit in the evidence annotation. Only synthetic processes run in
the acceptance suite; no vendor agent or shared gateway is involved.

## Test reports

Set `CheckCommand.report` for the `test` command:

- `Json`: the final nonempty stdout line is an object with required integer
  counts and unique nonempty failure IDs, for example
  `{"passed":3,"failed":1,"ignored":0,"failures":["new_case"]}`.
  The failed count must equal the number of failure IDs. Unknown fields,
  duplicate IDs and inconsistent counts are rejected.
- `Libtest`: Rust's normal `test ID ... FAILED` lines and `test result:`
  summaries supply failure IDs and counts. Compiler errors alone do not
  establish RED. IDs must be unique across the reported test targets; use a
  JSON reporter with qualified IDs when target names would be ambiguous.

`tests_fail` rejects empty expectations, expected new tests already present in
the baseline failure set, missing expected failures, missing/invalid reports,
signal exits and zero exits. Old failures may accompany the new failures.
Reports have a bounded parser budget; excessive or invalid report output blocks
rather than substituting an exit-code-only result.

## Evidence

A successful check creates an in-memory receipt for that step and commit. A
failed recheck revokes the receipt. New evidence requires that exact step, HEAD,
branch and a clean worktree; after a restart with no tag yet, checks must rerun.

The first evidence reference is `hermon/<ticket>/<tag>`, then `.2`, `.3`, etc.
For example, `red_fix` may produce `hermon/ISSUE-120/red.2` while retaining the
original RED tag. The annotation is JSON, repeated as a git note under
`refs/notes/hermon` on the annotated tag object. It records schema, ticket,
step, evidence revision, attempt, bot, optional model, branch/base/commit,
frozen paths, argv, exit codes, durations, test counts and log paths with
SHA-256 digests. The full JSON must be below 8192 bytes. Oversized metadata
blocks before creating a tag; frozen authority is never silently truncated.

`(tag, revision)` is an immutable idempotency key. Repeating evidence with the
same identity after a crash returns the original reference, even if HEAD has
advanced. A tag that exists without its explicit git note is recovered from
its annotation. Conflicting metadata or notes block rather than moving a tag
or overwriting evidence. The caller owns retention of referenced attempt logs.
Any future code-host adapter that publishes evidence must explicitly push the
annotated tags and `refs/notes/hermon`; a normal branch push does not include them.

Independent acceptance inputs, RED output, freezes and subsequent validation
are retained under `docs/flow/evidence/120/`.
