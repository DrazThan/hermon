# Installed Vitest skipped-status compatibility

Independent review inspected the actual RummyRune dependency, Vitest 2.1.9:
`node_modules/vitest/dist/chunks/index.DsZFoqi9.js`, JsonReporter/StatusMap.
It emits assertion `status: "skipped"` for skipped tests and includes those tests
in `numPendingTests`. `pending` is also emitted for run/only modes; both forms
must remain ignored evidence rather than assertion failures.

The independent `flow_vitest_skipped.rs` fixture adds one real skipped assertion
to an otherwise valid report with the independently expected behavioral failure.
The host must accept RED and preserve ignored count 1 in the git evidence note.
`vitest-skipped-red.txt` captures the intended rejection from the unsupported
skipped status. Existing frozen synthetic pending/todo cases remain unchanged.

This is compatibility validation from installed dependency source, not a live
RUM ticket run. The shared fixture is already frozen by vitest-freeze.sha256.
