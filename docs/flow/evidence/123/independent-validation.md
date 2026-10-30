# Independent Slice 8 validation

The independent acceptance author verified all three frozen suites against the
final implementation on 2026-10-29. All SHA-256 manifests pass unchanged:

```text
shasum -a 256 -c docs/flow/evidence/123/freeze.sha256
shasum -a 256 -c docs/flow/evidence/123/review-freeze.sha256
shasum -a 256 -c docs/flow/evidence/123/cancel-recovery-freeze.sha256
```

An independent run of `cargo test -p hermon --test flow_cli --test
flow_cli_regressions --test flow_cancel_recovery --locked` exits 0: **20 passed,
0 failed**. Exact output is in `independent-green.txt`. The command ran with
permission for loopback fake tracker connections and isolated owned-process
probes; no live tracker or vendor account was contacted. `git diff --check` also
passes.

The review fixes address the four independently reproduced failures:

- Doctor now maps workflow checks to their required project command names and
  reports missing definitions before intake creates a run.
- Doctor checks the configured GitHub repository through `gh repo view`, in
  addition to authentication, under the explicit project environment.
- Journal replay now persists a recovered snapshot even when its reconstructed
  event count already equals the append-only journal length.
- Cancelled runs continue polling an existing owned handle until settlement;
  settlement clears the handle without accepting late success, running checks,
  publishing evidence or starting a successor.

The final cancellation recovery regression also passes: while an owned handle
remains unresolved, the core idempotently repeats the stop request. This covers
a host crash after durable cancellation intent but before the first stop call.
Either a finished observation or confirmed stop clears the old handle;
unsettled stops retain it for the next observation. Cancellation remains terminal
for routing and acceptance throughout.

Code inspection also confirms the publication locking fix: intake creates and
locks `run.lock` inside its staging directory before renaming that directory
into the published run location. It retains the same file handle through
started-comment delivery and the final snapshot. Controls and the future host
therefore acquire the same locked inode when the run becomes visible.

No material defect remains in the reviewed Slice 8 scope. The hosting loop and
pipeline UI remain the next slice; this validation exercises the shared core
and real CLI with synthetic peers. It does not claim live bot, GitHub account,
or Linear delivery validation. No production source or frozen test was edited
by the independent validator.
