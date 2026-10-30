# macOS host fixture socket correction

PR #138 CI failed in the existing `flow_host.rs` tracker fixture before reaching
the ACP tests. The accepted stream's first read returned WouldBlock at line 48.
The [macOS job](https://github.com/DrazThan/hermon/actions/runs/41084378597/job/124161541625)
ran head `daddaddf62cba1e9a7d4d3f411c122d9a8d6485e`.
`ci-failure.txt` contains the relevant job-log excerpt fetched through the GitHub
connector; trailing whitespace was normalized.

I reproduced the socket behavior independently on macOS. The listener is
nonblocking so its accept loop can stop. On macOS an accepted stream inherits
that flag. A read can therefore return WouldBlock while the client is still
sending its HTTP request, even though the fixture set a read timeout.

`socket_probe.rs` connects a client that waits 150 ms before writing. The old
setup returns WouldBlock. Adding `set_nonblocking(false)` to the accepted stream
reads the byte successfully. `socket-probe.txt` records both results.

The correction adds that one call before the existing three-second read timeout.
The listener stays nonblocking. All request parsing, timeout bounds, HTTP
responses and host acceptance assertions are unchanged. Product code and the
pilot configuration are unchanged.

The original frozen source is `original/flow_host.rs`, SHA-256
`0d28c48c9eae950a556133eebbfab04c2df67f92b9dec3045e58b4e6d3814d4e`.
`original/freeze.v1.sha256` preserves the original issue-124 manifest verbatim.
The active `docs/flow/evidence/124/freeze.sha256` replaces only the corrected
flow_host.rs hash; its flow_pipeline.rs entry is unchanged. This directory's
freeze records the correction, reproducer and archived originals.

All three focused `flow_host` tests passed after the correction (`green.txt`):
`cargo test -p hermon --test flow_host --locked --offline`.
Targeted Clippy passed with warnings denied (`clippy.txt`). The loopback tests
and reproducer used sandbox elevation and contacted no live tracker or vendor.
