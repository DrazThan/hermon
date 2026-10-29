# Slice 6 workspace validation

Final integration validation on macOS, 2026-10-29, after the unsupported-agent
fallback fix:

| Command | Result | Evidence |
| --- | --- | --- |
| `cargo build --workspace --locked` | pass | `final-workspace-build.txt` |
| `cargo test --workspace --locked` | 900 passed, 0 failed, 3 ignored | `final-workspace-test.txt` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass | `final-workspace-clippy.txt` |
| `cargo fmt --all --check` | pass | `final-workspace-fmt.txt` |
| `cargo build --release --locked -p hermon` | pass | `final-release-build.txt` |

The workspace test total sums 47 completed libtest targets/reports, including
zero-test/doc-test targets. The 3 ignored tests are existing manual live desktop
notification checks. New independently frozen acceptance covers 17 flow tests
and one real CLI preview test. All four active freeze manifests verify.
Independent replay and scope limits are recorded in `validation.md`.

The Rust/Cargo runtime is the existing isolated toolchain at
`/private/tmp/hermon-116-rust/`. Host access is required for loopback fake servers
and existing owned-process settlement observations. Tests use synthetic tracker
credentials and owned synthetic worker processes. No live Linear comment or
vendor agent is invoked. Live app-actor delivery/readback and hidden marker
persistence through Linear's renderer were skipped at the owner's explicit
request and remain unverified.

The first integration attempts and their outputs are retained separately:
`sandbox-test.txt` records socket binding refusal; `initial-green.txt` records
the independently corrected macOS accepted-socket fixture issue;
`initial-clippy.txt` records subsequently fixed production style findings;
`initial-workspace-test.txt` includes the copied-marker regression before its
fix. Final logs above supersede those runs. Frozen tests were not changed by
the implementer; the independent test-writer documented the single fixture
correction and preserved its original freeze and behavioral RED evidence.
