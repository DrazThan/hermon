# Issue 125 workspace validation

Independent acceptance tests were frozen and committed in behavioral RED before
the corresponding implementation changes. All 25 tests subsequently passed with
unchanged hashes in a separate independent run; see `independent-validation.md`.

| Check | Result | Evidence |
| --- | --- | --- |
| `cargo build --workspace --locked` | passed | workspace-build.txt |
| `cargo test --workspace --locked` | 984 passed, 0 failed, 3 existing ignored | workspace-test.txt |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed | workspace-clippy.txt |
| `cargo fmt --all --check` | passed | workspace-fmt.txt |
| `cargo build --release --locked -p hermon` | passed | release-build.txt |
| Release template preview and isolated empty-directory `hub --once --no-notify` | passed; no live run started | release-smoke.txt |
| Nine acceptance SHA-256 manifests | unchanged | independent-freeze-check.txt |

Commands used the installed Rust toolchain at
`/private/tmp/hermon-116-rust/rustup/toolchains/stable-aarch64-apple-darwin/bin`,
with `CARGO_HOME=/private/tmp/hermon-116-rust/cargo` and
`RUSTUP_HOME=/private/tmp/hermon-116-rust/rustup`. Integration tests requiring
localhost fixtures/process supervision ran with sandbox elevation. No dependency
or lockfile change was needed.

The first release build linked successfully but its local rust-objcopy could not
resolve libLLVM.dylib, leaving a stripping warning (`release-initial.txt`). Setting
`DYLD_LIBRARY_PATH` to the toolchain's `lib` directory fixed the tool lookup; the
release target was relinked and rebuilt with stripping. No repository build
configuration was changed for this machine-specific environment repair.

The initial partial correction run in `correction-green.txt` exposed the second
freeze-validation issue; `correction-final-green.txt` and the independent/full
runs confirm the completed fix. Sandbox-only failed probes are retained separately
and are not counted as behavioral RED or product failures.

The owner approved all five local profile revisions and the exact controller
pause; `profile-migration.json` and `independent-migration-validation.md` record
readback and persistent backup locations. The historical plugin remains unchanged.

The guided live RummyRune pilot is deferred by the owner. No real RUM ticket,
Owner GO, RED/GREEN, Review/PR, timed block alert, owner merge or post-merge Docs
acceptance is claimed. Named Hermes profiles retain the existing cancellation
capability gate and require verified distinct model-family provenance before
pilot intake. Linux and musl remain CI validation targets. This change builds
on the issue 124 host implementation from PR #135.
