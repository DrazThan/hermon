# Slice 8 final validation

Validated on 2026-10-29 against the issue 123 branch.

| Check | Result | Evidence |
|---|---|---|
| Independent frozen acceptance and regressions | 20 passed, 0 failed | `independent-green.txt` |
| Workspace build (`cargo build --workspace --locked`) | passed | `workspace-build.txt` |
| Workspace tests (`cargo test --workspace --locked`) | 935 passed, 0 failed, 3 existing ignored | `workspace-test.txt` |
| Clippy (`cargo clippy --workspace --all-targets --locked -- -D warnings`) | passed | `workspace-clippy.txt` |
| Format (`cargo fmt --all --check`) | passed | `workspace-fmt.txt` |
| Release (`cargo build --release --locked -p hermon`) | passed | `release-build.txt` |
| All three independent SHA-256 freezes | unchanged | `freeze.sha256`, `review-freeze.sha256`, `cancel-recovery-freeze.sha256` |

The existing remote-agent startup test hit its two-second Hello-frame timeout
once while the release compiler was active (`workspace-test-startup-timeout.txt`).
After release finished, the unchanged agent suite passed in isolation
(`agent-recheck.txt`), followed by the complete successful workspace run above.
No timeout or existing test was changed. An earlier sandbox-only baseline could
not inspect process groups; the accepted integration runs use host access for
local HTTP fixtures and owned process supervision, as in preceding slices.

The acceptance author independently wrote and froze tests before implementation,
then reproduced and froze four review failures and two cancellation crash-recovery
failures before their fixes. See `acceptance.md`, `review-acceptance.md`,
`cancel-recovery-acceptance.md` and `independent-validation.md` for that evidence.
All peers are synthetic; live vendor prompts, tracker delivery and GitHub account
probes are not claimed by these tests. Menubar hosting and pipeline UI are #124.
