# Independent final verification

The independent test-writer reviewed `GithubCodeHost` after implementation and
reran both frozen suites. `independent-green.txt` records **15 passing tests**:
9 original acceptance tests and 6 independently frozen review regressions.
`independent-clippy.txt` records matching Clippy with warnings denied passing.
Both `freeze.sha256` and `review-freeze.sha256` verify without any test edits.

The final implementation addresses the independent review finding: it binds all
expanded fetch URLs and all push URLs, checks them before an operation, and
includes the persisted endpoint fingerprint in the PR marker used for recovery.
A changed or additional push endpoint now fails before publication. GitHub.com
commands explicitly set `GH_HOST=github.com`; the configured ambient host cannot
redirect a run. The configured environment is excluded from persisted state.

Review also confirmed that merge observations check the recorded PR identity,
base/head, head owner and marker before accepting the exact MERGED state. The
merge commit must be an ancestor of a freshly fetched run-specific base ref.
Neither the public code-host trait nor its concrete implementation exposes or
invokes a merge operation. Publication uses an explicit non-forced run-branch
refspec with tag following disabled and passes PR body text as a file.

No further concrete acceptance defects remain from this independent review.
All independent commands used synthetic local repositories and a fake gh; no
live GitHub operation was performed. Full workspace, release and platform CI
validation are recorded by the implementation host separately.
