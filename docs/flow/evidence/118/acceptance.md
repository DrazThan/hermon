# Slice 3 acceptance freeze

An independent test-writer authored `crates/hermon-flow/tests/hub.rs` for [issue #118](https://github.com/DrazThan/hermon/issues/118), using [plan #114](https://github.com/DrazThan/hermon/issues/114), sections 7 and 9. The implementer did not author the acceptance suite. The public API in `src/hub.rs` was introduced as behavior-free stubs before the tests ran.

Behavioral RED: `cargo test -p hermon-flow --test hub --locked` compiled successfully and ran 23 tests: **0 passed, 23 failed**. The retained `red.txt` records the output. `cargo clippy -p hermon-flow --test hub --locked -- -D warnings` passed before freeze. The toolchain was the existing isolated Cargo/Rustup installation at `/private/tmp/hermon-116-rust/`.

The nine named issue acceptance tests cover the happy SDLC path through docs, superseding RED evidence, bounded review loops, transient retries, strict invalid/missing result handling, manual retry retaining automation, owner gates, idempotence and visible blocks. Additional cases cover manual override, RED-fix execution limits, repeated check failures, deadlines taking precedence over late success, unresolved shared-worker outcomes, stalled running work, agent-family separation, concurrency waits, code-host failure, fresh Owner GO after scope change, review verdict validation, and alert delivery retry.

## Agreed semantics

- `advance` is a deterministic core with injected Agent, Tracker, CodeHost, Git and Clock. It performs no real network/process/filesystem operations. The caller owns per-run locking and durable writes; later slices implement adapters.
- Clocks use seconds. A timeout is reached at `attempt_timeout_minutes * 60`; a stall alert at `stall_alert_minutes * 60` does not stop the worker.
- `transient_retries` counts retries beyond the first attempt. Review `max_rounds` counts review executions and blocks immediately when the final review requests changes. `red_fix.max_runs` prevents a third correction start when set to two. Failed checks automatically rerun once; a second failure blocks and alerts without success evidence.
- Success requires both process success and an explicit successful completion signal, then valid `result.json` and successful checks. The result schema uses `schema`, `status`, `summary`, `artifacts`, `block`, `verdict`, `questions`; verdict is null, `clean`, or `changes_needed`. Review requires a known non-null verdict. Contradictory done-plus-block, block-without-reason, missing/invalid result, unknown schema/status/block/verdict are unknown blocks.
- Freeze refs are supplied to each attempt and checks; the second successful `red` tag supersedes the first active reference without deleting history.
- Owner GO requests carry a persisted monotonically increasing generation. Old approval cannot pass a new request after scope change. Tracker approval and merged/ancestor validation are adapter contracts; adapter implementations are later slices. The hub exposes no merge method.
- An unresolved worker outcome blocks, retains its active handle, alerts, and rejects both manual retry and override. Later confirmed settlement removes the uncertainty but does not accept the late result as success; the owner can then explicitly retry. A timeout whose stop cannot establish settlement follows the same rule.
- Retry and override require a reason and record an event. Override cannot bypass a gate or unresolved worker. Both preserve automatic downstream transitions.
- A blocked or waiting state must have an automatic route or a deduplicated owner alert. Failed alert delivery returns an error and is retried on a later advance; it is never recorded as successfully delivered prematurely.

## Frozen ownership

`freeze.sha256` covers the new acceptance suite and its existing SDLC fixture. Production implementation may change `src/hub.rs` but must keep these test inputs byte-for-byte unchanged. A genuine test defect returns to the independent test-writer for a correction, a rerun and a superseding freeze with the reason recorded here.

## Superseding freeze v2

The independent test-writer corrected a genuine test fixture defect: `hub_failed_alert_delivery_is_retried_without_restarting_work` reaches green through Owner GO, which has already delivered a required alert. Its original assertion incorrectly expected the lifetime alert list to be empty. The correction captures the previously delivered count, requires it to stay unchanged when the new alert fails delivery, and requires exactly one additional alert after retry, without restarting the blocked bot. Production behavior was not changed to accommodate the bad assertion.

The implementer requested additional independent pre-fix coverage. Ten additional tests require every documented result field to be present (nullable `block` and `verdict` still require their keys), reject extra top-level and nested block fields, reject verdicts on non-review steps, exercise bounded transient start failures, preserve unresolved workers after poll/stop adapter errors, prevent git evidence failure from releasing a successor, and enforce the configured transient retry allowance for settled git-check, git-evidence and gate-observation adapter failures.

Before the additional production changes, the updated suite compiled and ran **33 tests: 27 passed, 6 failed**. The six failures expose mandatory-field omissions, nested unknown fields, non-review verdicts, and missing transient retries for check/evidence/gate adapters; `supplemental-red.txt` retains this evidence. The four other new adapter-fault regressions passed the initial implementation.

The complete corrected suite was also replayed against the original behavior-free stubs from commit `fde56ee` in an isolated `/private/tmp/hermon-118-freeze-v2-*` workspace: **0 passed, 33 failed**, retained in `red-v2.txt`. The isolated workspace contains only the flow crate, so its lock file was generated offline first; the recorded test command then used `--locked`. The live production source was never reverted. Clippy with warnings denied passed for the complete test target before v2 freeze.

`freeze.v1.sha256` preserves the original manifest. The active `freeze.sha256` supersedes it and freezes the corrected 33-test suite and SDLC fixture. The same independent ownership and defect-correction rules continue to apply.

For a settled transient check/evidence adapter failure, the retry reruns the owning bot and its checks/evidence; the evidence revision remains the same idempotency key until evidence succeeds. A transient gate-observation retry may create a fresh gate-request generation; the acceptance contract requires bounded retry and continued gate safety, without prescribing generation reuse. Explicit workflow `on_block` routes can handle non-transient kinds; the supplied SDLC defaults surface owner-question, capability and unknown blocks rather than retrying an unchanged semantic block.
