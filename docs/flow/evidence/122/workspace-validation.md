# Slice 7 workspace validation

Final macOS integration validation on 2026-10-29:

| Command | Result | Evidence |
| --- | --- | --- |
| `cargo build --workspace --locked` | passed | `workspace-build.txt` |
| `cargo test --workspace --locked` | 915 passed, 0 failed, 3 existing ignored | `workspace-test.txt` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed | `workspace-clippy.txt` |
| `cargo fmt --all --check` | passed | `workspace-fmt.txt` |
| `cargo build --release --locked -p hermon` | passed | `release-build.txt` |

The test total sums all 49 completed libtest reports, including empty targets
and doc-tests. The three ignored tests are existing manual notification checks.
The independent writer verified all 15 new frozen tests and matching Clippy;
see `independent-validation.md`. Both freeze manifests remain byte-identical.

The initial acceptance freeze ran 9 compiled tests with 9 behavioral failures
at the adapter stubs (`red.txt`). The first implementation reached 8 passing
and 1 failing test (`initial-green.txt`): endpoint rebinding was rejected in the
constructor, preventing the test from exercising its operation failure. Endpoint
verification now belongs to the publication/gate operations, preserving intake
binding and refusing changed endpoints before either operation. The original
9 tests then passed (`green.txt`). Independent review added six separately
frozen regressions: three passed and three exposed push-URL and GitHub-host
redirection defects (`review-red.txt`). Their fixes pass in the final independent
15-test replay. Neither frozen test file nor the original fake gh was edited by
the implementer.

The runtime is the existing isolated Rust toolchain under
`/private/tmp/hermon-116-rust/`. Host access is required for the workspace's
loopback servers and owned-process settlement observations. Code-host tests use
only fake gh and disposable local repositories/bare remotes. No live GitHub
publication or merge occurs in the tests. Linux/musl validation belongs to CI.
