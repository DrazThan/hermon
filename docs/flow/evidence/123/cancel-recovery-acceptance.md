# Cancellation intent crash recovery

Final review identified a second cancellation crash boundary: the CLI saves
terminal cancellation intent before requesting the owned worker stop. A crash
between those operations leaves a cancelled run with a live handle but no stop
request. Merely polling that handle cannot guarantee prompt cancellation.

The independent writer added `tests/flow_cancel_recovery.rs`, frozen separately
by `cancel-recovery-freeze.sha256`, without modifying earlier frozen suites.
Two pure-core tests require repeated advances to resend the owned stop
idempotently while settlement is unresolved, and clear the handle when a stop
confirms settlement. Both require cancellation to remain terminal with no
successor dispatch, check acceptance or evidence publication.

Behavioral RED: `cargo test -p hermon --test flow_cancel_recovery --locked`
compiled successfully and exited 101, **0 passed, 2 failed**, because neither
running cancelled handle received a stop call. Exact output is in
`cancel-recovery-red.txt`. The suite requires no external processes or network.
