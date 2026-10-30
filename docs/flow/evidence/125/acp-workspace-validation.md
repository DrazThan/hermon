# Hermes ACP prerequisite validation

The final workspace run passed 1,006 tests with zero failures and three existing
ignored tests across 72 test summaries. Command:

```sh
cargo test --offline --workspace -- --test-threads=1
```

`acp-workspace-final-test.txt` is the complete final output. Local loopback test
servers required execution outside the restricted filesystem/network sandbox.
The earlier serial sandbox run failed at `TcpListener::bind` with Operation not
permitted; it is retained in `acp-workspace-serial-test.txt`.

Two earlier parallel workspace runs failed existing notification/hub-lock tests
outside the ACP changes. The notification test passed in isolation; both passed
in the complete final serial run. Their original outputs remain in
`acp-workspace-first-test.txt`, `acp-workspace-test.txt` and
`acp-notifier-recheck.txt`. No existing test was changed to clear these failures.

Workspace build, strict all-target Clippy, format check and optimized release
build passed. Outputs are `acp-workspace-build.txt`, `acp-final-clippy.txt`,
`acp-fmt.txt` and `acp-release-build.txt`. The configured local Rust toolchain,
offline Cargo home and release LLVM library path were supplied explicitly.

All 22 new acceptance tests also passed independently, with all five active ACP
freeze manifests unchanged. See `acp-independent-validation.md`. The earlier
canonical temporary-path assertion correction was made by the independent test
writer and is retained with its original source, manifest and failure.

Live five-profile result/provenance and two-session active-tool cancellation
probes passed on Hermes 0.22.4 and were repeated successfully on 0.22.5 after the
first integrated doctor detected a version change before submitting any prompt.
The pilot configuration pins 0.22.5. These prerequisite results do not complete
the live workflow or its owner gates.
