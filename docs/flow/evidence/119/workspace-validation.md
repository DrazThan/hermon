# Workspace verification

The integrating implementer verified the final Slice 4 code on macOS on
2026-10-29 with the isolated toolchain under `/private/tmp/hermon-116-rust/`.

| Check | Result |
| --- | --- |
| `cargo build --workspace --locked` | Passed |
| `cargo test --workspace --locked` | 854 passed, 0 failed, 3 existing ignored |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed |
| `cargo fmt --all --check` | Passed |
| `cargo build --release --locked -p hermon -p hermon-flow` | Application and detached worker passed |
| Slice 4 original and supplemental SHA-256 manifests | All four frozen inputs unchanged |
| Slice 2 and Slice 3 active SHA-256 manifests | All prior inputs unchanged |
| `git diff --check` | Passed |

Process tests used host process access because the sandbox blocks `ps` group
observations. Only synthetic agents and test-owned groups were launched. The
independent writer's 17-test verification and review are in
[validation.md](validation.md); RED logs, freeze manifests and final GREEN
output are retained in this directory.

Hermes command construction and stderr identity parsing are verified; actual
Hermes starts remain a capability block pending the scoped cancellation gate
documented by Slice 0. No live vendor sessions or shared gateways were modified.
Linux and musl builds use the existing GitHub Actions matrix.
