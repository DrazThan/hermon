# GitHub PRs and the owner-merge gate

Slice 7 supplies `GithubCodeHost`, an implementation of the hub's `CodeHost`
interface. It invokes `git` and `gh` directly, without a shell. The existing
`open_pr` action publishes a run branch and creates a PR; `owner_merge` observes
what the owner did in GitHub. Neither the adapter nor the hub has a merge operation.

The host constructs `GithubConfig` from confirmed intake: the worktree,
GitHub.com `owner/repo`, git remote name, base branch, run branch and run ID.
It also supplies the PR title, approved plan text and independent review summary.
The body includes those artifacts verbatim and a deterministic hidden run marker.
The base is always explicit, including when it differs from the repository's
default branch. Hosting and intake CLI wiring belong to later slices.

Give the adapter a dedicated state directory, separate from the hub's `run.json`,
and serialize its calls under the host's per-run lock. The atomic receipt binds
its repository, worktree, run, branches, title and artifact content. Reusing the
directory with a different intake or artifact revision fails closed. All expanded
fetch and push endpoints are hashed separately and rechecked before publication
or merge polling, including remotes with multiple push URLs.
The environment and credentials are never serialized.

Every operation uses the configured complete environment, including `PATH` and
any `GH_TOKEN`, `HOME`, `GH_CONFIG_DIR` or SSH settings required by that project.
It does not inherit the hub's environment. The GitHub host is fixed to
`github.com`, and prompts are disabled. Each command has
a 60-second deadline, an owned process group and a 2 MiB output limit. Child
output is excluded from errors so authentication details cannot leak through
command diagnostics. Missing executables are capability blocks; failed transport
commands are transient blocks. Identity conflicts require owner intervention.

Before creating a PR, the adapter queries all PR states for the exact head and
base. A matching run marker recovers the existing open or merged PR after a
restart, lost local receipt, or lost create response. A foreign, ambiguous or
closed unmerged PR blocks publication. A successful empty lookup is required
before a non-forced push of the explicit run-branch refspec and `gh pr create`.
The body is passed through a temporary file, preserving newlines and literal
shell characters. The host must not run two publishers for the same run at once.

The merge gate verifies the recorded URL/number, repository, head owner,
head/base and run marker against `gh pr view`. Only the exact `MERGED` state with
a valid merge-commit OID can proceed to ancestry verification. The adapter fetches
the configured remote's base into a run-specific ref, then runs
`git merge-base --is-ancestor <merge-oid> <fetched-base>`. An open or closed PR,
a missing merge commit, or a merge absent from that base keeps the gate waiting.
Observation and fetch errors never approve the gate. Local base branches are
not updated. The configured remote must point to the configured GitHub repository;
the host is responsible for that intake mapping.

Acceptance uses a fake `gh` on the configured `PATH` and real, disposable local
repositories and bare remotes. It covers fresh base ancestry, recovery and the
real hub staying at its gate until an owner merge is observed. No live GitHub PR
or merge is needed by the tests. Independent freezes and RED/GREEN evidence live
under `docs/flow/evidence/122/`.
