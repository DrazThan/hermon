# Behavioral RED and test freeze

Independent test-writer freeze, 2026-10-29. Production methods were compileable stubs returning explicit `not implemented` errors; no workflow, template or store behavior had been implemented.

Command (with the existing toolchain environment):

```sh
env CARGO_HOME=/private/tmp/hermon-116-rust/cargo \
  RUSTUP_HOME=/private/tmp/hermon-116-rust/rustup \
  PATH=/private/tmp/hermon-116-rust/cargo/bin:$PATH \
  cargo test -p hermon-flow --tests --no-fail-fast
```

Result: exit **101** after compilation succeeded. **24 tests failed, 0 passed** across workflow (11), templates (7), and store (6). These are executed assertion/unwrap failures due to missing behavior, not compile failures. All eleven acceptance test names specified in #117 are represented. Full output: [red.log](red.log).

The four files in [frozen.sha256](frozen.sha256) are frozen before implementation. Verify from repository root with `shasum -a 256 -c docs/flow/evidence/117/frozen.sha256`.
