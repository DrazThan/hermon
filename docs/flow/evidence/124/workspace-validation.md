# Issue 124 workspace validation

Validated on macOS with the toolchain recorded in `acceptance.md`.

| Check | Result | Evidence |
|---|---|---|
| Workspace build (`cargo build --workspace --locked`) | passed | `workspace-build.txt` |
| Workspace tests (`cargo test --workspace --locked`) | **959 passed, 0 failed, 3 existing ignored** | `workspace-test.txt` |
| Clippy (`cargo clippy --workspace --all-targets --locked -- -D warnings`) | passed | `workspace-clippy.txt` |
| Format (`cargo fmt --all --check`) | passed | `workspace-fmt.txt` |
| Release (`cargo build --release --locked -p hermon`) | passed | `release-build.txt` |
| Independent frozen acceptance and review suites | **23 passed**, five test files unchanged | `independent-green.txt`, `independent-validation.md` |

The independently authored RED commits precede production implementation. The
initial acceptance tests and each supplemental regression suite have separate
SHA-256 manifests. Verify from repository root:

```sh
shasum -a 256 -c docs/flow/evidence/124/freeze.sha256
shasum -a 256 -c docs/flow/evidence/124/review-freeze.sha256
shasum -a 256 -c docs/flow/evidence/124/host-isolation-freeze.sha256
```

After independent GREEN, an additional GUI unit regression reproduced the
pipeline tab being hidden by a previously zoomed session. `zoom-red.txt` and
`zoom-green.txt` capture its RED/GREEN cycle. The final workspace run caught a
footer hint clipping at 70 columns (`workspace-test-footer-red.txt`); shortening
the product hint preserved the unchanged existing render assertion. Rustfmt
then reflowed that constant declaration; the final format check passed.

Test execution required permission for isolated loopback fixture servers and
owned child processes. No provider credentials, real tracker issues or desktop
banners were used. The independent suite exercises actual foreground host
dispatch, configured PATH/environment, detached worker supervision, durable
completion and restart recovery, plus both UI pane command paths.

The manual native macOS checklist in `acceptance.md` remains unperformed.
Linux and static musl builds remain covered by the repository's CI matrix.
