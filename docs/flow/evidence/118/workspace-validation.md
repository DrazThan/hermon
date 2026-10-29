# Workspace verification

The integrating implementer checked the final implementation on macOS on
2026-10-29, using the existing isolated Rust toolchain in
`/private/tmp/hermon-116-rust/`.

| Command | Result |
| --- | --- |
| `cargo build --workspace --locked` | Passed |
| `cargo test --workspace --locked` | 837 passed, 0 failed, 3 ignored |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed |
| `cargo fmt --all --check` | Passed |
| `cargo build --release --locked -p hermon` | Passed |
| `shasum -a 256 -c docs/flow/evidence/118/freeze.sha256` | Both frozen inputs unchanged |
| `git diff --check` | Passed |

The hub contributes 35 independently authored acceptance tests. Their RED
commits precede the production implementation; the independent test writer's
final verification is recorded in [validation.md](validation.md).
Linux and musl builds are covered by the repository's existing GitHub Actions
workflow.
