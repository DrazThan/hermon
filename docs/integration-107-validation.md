# Six-source integration validation

The core proof needs no Docker, paid account, vendor executable, or real home
store. `cargo test --test integration_sources --test roster` creates isolated
roots, runs the built `hermon agent`, follows X/G/Gm sessions with identical raw
IDs through `RemoteSource`, checks remote labels and filtering, and checks
exactly-once appended text/patch delivery and serialized LastEvent/turn state.
It also covers the six-source local roster and binary `ls`/`render` paths.
CLI tests cover every SourceArgs consumer; transport tests cover all new flags,
spaces, hostile SSH arguments, complete `cmd:` argv, and fixture discovery,
removal, re-addition and explicit-name collision precedence.

## Local results (2026-09-15)

- `cargo build --locked`: passed on aarch64 macOS, including GUI/menubar.
- `cargo test`: 723 passed; three existing manual desktop-notification tests
  ignored. All suites passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo fmt --check`: passed.
- Linux agent binary cross-compilation: `cargo zigbuild --locked --target
  x86_64-unknown-linux-musl` passed with the rustup toolchain selected on PATH.
  Linux runtime tests remain the CI runner's responsibility; this machine
  runs macOS. The Homebrew Rust toolchain lacks the Linux target, so using
  the installed rustup toolchain was necessary for this additional check.

## Real-container check

Availability on 2026-09-15: the Docker client is installed, but
`docker info --format '{{.OSType}}/{{.Architecture}}'` fails because
`/var/run/docker.sock` does not exist. No real-container check was run.

When a daemon is available, use a disposable image containing the **matching
Linux build** of this checkout on PATH as `hermon`. Seed only sanitized fixtures:

- Copy `tests/fixtures/codex/*.jsonl` under `/fixtures/codex home/sessions/`.
- Copy `tests/fixtures/grok/{summary.json,usage.json,chat_history.jsonl}` under
  `/fixtures/grok/sessions/project/nested-session/`.
- Copy the contents of `tests/fixtures/gemini/` under `/fixtures/gemini/tmp/`.
- Advance fixture timestamps to the check's clock so finished sessions remain
  within the freshness window. Keep the fixtures writable for appending test
  records; hermon itself only reads them.

Start that image as `hermon-check` with `--label dev.hermon.agent=1` and
`--label dev.hermon.agent.name=fixture`, keeping its main process alive. Then:

```bash
hermon watch --docker-auto --remote-flags "--claude-dir /nonexistent/c --hermes-db /nonexistent/h --opencode-db /nonexistent/o --codex-dir '/fixtures/codex home' --grok-dir /fixtures/grok --gemini-dir /fixtures/gemini"
```

Verify `fixture/X:`, `fixture/G:` and `fixture/Gm:` rows and open each pane.
Append supported text events and a Gemini `$set.messages` update (see
`tests/common/new_stores.rs`) and check each appears exactly once in its pane.
Stop the disposable container and verify removal; restart it and verify the
same roots, rows and live tails return. Repeat with
`--remote docker:hermon-check:fixture` alongside `--docker-auto` and verify
only the explicit remote owns `fixture`. Remove the disposable container
when finished. No source labels, automatic mounts or installation are involved.

CI runs the required build/test/fmt gates and Clippy with `--all-targets` on
Linux and macOS; the macOS build includes native GUI/menubar code. Workflow
results for these uncommitted changes cannot be observed until they are
submitted by the orchestrator.
