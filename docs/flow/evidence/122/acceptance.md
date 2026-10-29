# Slice 7 independent acceptance freeze

The independent test-writer authored `tests/code_host.rs` and `tests/fixtures/gh.py`
from issue #122 and plan #114 section 9 before production implementation.
`freeze.sha256` freezes these files. `src/code_host.rs` supplies behavior-free
public API stubs only; the implementer owns that source after this freeze.

`cargo test -p hermon-flow --test code_host --locked` compiled and ran **9 tests,
0 passed, 9 failed** (`red.txt`). All failures reach `open_pr`'s unimplemented
behavior, rather than setup or compilation errors. Matching Clippy with warnings
denied passes (`red-clippy.txt`).

Accepted behavior:

- `open_pr` pushes the run branch to the configured remote before `gh pr create`.
  It uses the intake base (`qa` in the fixture, deliberately distinct from
  `main`), configured head and title. The body includes the full chosen plan,
  independent review summary and a stable run marker. Shell-like text remains
  literal. A fake `gh` is selected by the adapter's configured `PATH`.
- Repeated calls and adapter restarts reuse the same PR. Losing local receipts
  still reconciles the externally marked PR. A lost create response cannot cause
  another PR on the next call. Closed, unmerged and foreign-body PRs fail closed
  rather than binding an unrelated PR or creating a duplicate.
- Wrong run ID, changed intake base with durable state, failed PR lookup and
  failed push cannot create a PR. No live GitHub repository is contacted.
- `owner_merge` requires state exactly `MERGED`, a merge OID, the correct PR
  URL/base/head and real git ancestry on the freshly fetched configured remote
  base. OPEN and CLOSED do not pass even when an OID is present. The fixture
  keeps the local base stale while the simulated owner advances remote `qa`.
- An unrelated PR URL/base/head and an option-like malformed OID never pass.
- The real deterministic hub opens the PR, stays at the merge gate through
  repeated observations, survives a serialized run and adapter restart, then
  reaches Done after the owner advances the remote base. Every fake `gh` call is
  recorded, and no merge invocation is accepted or observed.

All repositories, bare remotes, PRs and actors are synthetic and temporary.
Commands use per-child configured environments; tests do not mutate process-wide
PATH. No live comment, PR, push or merge is performed. Core frozen tests must not
be edited by the production implementer; a genuine fixture defect returns to
the independent test-writer for a documented superseding freeze.
