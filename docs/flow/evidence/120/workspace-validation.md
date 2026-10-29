# Workspace verification

The integrating implementer verified Slice 5 on macOS on 2026-10-29 with the
isolated toolchain under `/private/tmp/hermon-116-rust/`.

| Check | Result |
| --- | --- |
| `cargo build --workspace --locked` | Passed |
| `cargo test --workspace --locked` | 882 passed, 0 failed, 3 existing ignored |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed |
| `cargo fmt --all --check` | Passed |
| `cargo build --release --locked -p hermon -p hermon-flow` | Application and detached worker passed |
| Slice 5 corrected and supplemental SHA-256 manifests | All three inputs unchanged |
| Slice 2, 3 and 4 active SHA-256 manifests | All prior frozen inputs unchanged |
| `git diff --check` | Passed |

Command output is retained in `workspace-build.txt`, `workspace-test.txt`,
`workspace-clippy.txt`, `workspace-fmt.txt` and `release-build.txt`. The independent
28-test replay and production review are recorded in [validation.md](validation.md).
The original RED, documented fixture correction, superseding freeze and five
supplemental RED failures remain available with their frozen test inputs.

Process tests used host access because the sandbox blocks the `ps` group
observations needed to establish settlement. They launch synthetic agents and
check commands in temporary repositories. Linux/macOS and musl coverage is
provided by the existing GitHub Actions matrix on the pull request.
