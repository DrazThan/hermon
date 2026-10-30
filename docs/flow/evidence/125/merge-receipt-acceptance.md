# Independent verified merge receipt regression

Issue #125's Docs and final outcome need the exact owner-merged commit.
`GithubCodeHost::merged` currently verifies ancestry but returns only a boolean,
so the observed commit is lost before those later consumers run.

`crates/hermon-flow/tests/code_host_merge_receipt.rs` exercises the existing
adapter with a synthetic `gh`, a local bare remote, and the real git ancestry
check. It requires the code-host state `run.json` to retain `merge_commit` only
after the selected PR is MERGED and its reported commit is an ancestor of the
freshly fetched selected base. Reopening the adapter must preserve that exact
SHA. OPEN and non-ancestor observations must leave the receipt absent/null.

The tests compile against the existing public API and inspect serialized state,
so their initial RED is behavioral. Before product changes, the invalid-merge
case passed; the verified-merge case failed because `merge_commit` was null.
The initial sandbox prevented process supervision; the recorded rerun outside
the sandbox reached the actual missing-receipt assertion.

```sh
cargo test -p hermon-flow --test code_host_merge_receipt --locked
```

The exact output is in `merge-receipt-red.txt`; the immutable source hash is in
`merge-receipt-freeze.sha256`. No external GitHub API or live repository was
changed. This regression checks durable metadata; the host's Docs/outcome
consumers must use that receipt rather than inventing a commit from a PR URL.
