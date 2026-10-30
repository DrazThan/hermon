# Hub hosting and pipelines

`hermon menubar` hosts queued workflow runs in the background. With no queued
runs it continues monitoring sessions normally. Intake stays explicit: create
runs with `hermon run` after doctor passes and the base is confirmed.

```sh
hermon menubar --data-dir /path/to/hermon-data
hermon hub --data-dir /path/to/hermon-data
hermon hub --data-dir /path/to/hermon-data --once --no-notify
hermon watch --data-dir /path/to/hermon-data
hermon gui --data-dir /path/to/hermon-data
```

All commands use the same default data directory described by
[intake and controls](intake-controls.md). `watch` and `gui` read workflow state;
the menubar and foreground `hub` advance it. The menubar's Open Watch action
passes its data directory to the new terminal.

Exactly one host can own a data directory. `hub.lock` holds an exclusive kernel
file lock for the host's lifetime. It remains on disk after exit: file existence
does not establish ownership. Closing the descriptor or process death releases
the lock. Never delete it to start another host; replacing the inode defeats
exclusion. A second foreground host reports the conflict and exits unsuccessfully.
A second menubar reports the conflict and continues session/pipeline monitoring.

The hosting thread runs independently of transcript polling. Each run uses the
same `run.lock` as CLI controls, so an answer, cancellation or host tick cannot
overwrite another action. Busy snapshots retain their previous UI state until
the next refresh. Pausing prevents new dispatch. Cancellation remains terminal
while its owned worker settles; restarting a host repeats the durable stop
request without accepting a late result or dispatching a successor.

The installed `hermon` executable includes the worker entry point, so a packaged
app needs no separately installed supervisor binary. Workers keep the existing
owned-process deadlines, cancellation and restart contract. Their complete
environment comes from the project configuration, including executable lookup
through its PATH. Credentials are passed in memory to child processes and are
not copied to launch receipts or run/task files. Launch-at-login still requires
the central tracker token in the host environment named by `tracker.token_env`.

The host snapshots state even when an external adapter fails after a transition.
An adapter/configuration failure records a `host_error` event and alerts the
owner; later ticks retry reconciliation. Errors in one valid run do not prevent
other runs from advancing. Malformed committed runs produce a diagnostic.

## Pipeline views

In the TUI, press **v** for pipelines, **j/k** or arrow keys to select a step,
and **Enter** to open its session. Press **v** or **Esc** to return to sessions.
The GUI has **Sessions** and **Pipelines** tabs; click a running step to open
its transcript in the existing pane below it.

Each run lists its normal chain, then alternate branches. Rows show run, step,
configured bot (or owner/action), attempt number, age since the latest attempt
started, status and last recorded block. Prior step/gate completion comes from
the run events. Future steps remain visible. Age uses the host's clock with
saturating subtraction if the clock moves backwards. Selection tracks the run
and step across refreshes.

Navigation requires the active attempt's recorded session ID in `process.json`.
A session receipt may arrive after the start event. Pending or unavailable
sessions stay visible with an explanation; no prefix/title matching is used.
Only an unambiguous local session can open a pipeline pane.

## Alerts and checks

Owner gates and blocks use the existing permission-wait notification setting;
stalled attempts use the stuck setting and the workflow's
`limits.stall_alert_minutes`. Host errors also request owner attention. Desktop
delivery uses the same `notify::deliver` backends as session alerts. The UI
process's mute, `--no-notify`, individual kind settings and notification
arbitration apply. Repeated polls of one condition are quiet; a new attempt,
gate request or block can alert again. Desktop deduplication is per process,
consistent with existing session notification history. The hub's separate
central tracker alerts retain their durable delivery receipts.
The foreground host shares the terminal watcher's notification priority and
checks for a higher-priority GUI/menubar notifier every tick. `--no-notify`
suppresses its banners explicitly.

The host writes `task.md` for each attempt with ticket context, step requirements,
declared outputs and accepted input artifacts. Produced artifacts belong beside
the attempt's `result.json`; inputs cannot escape that attempt directory. The
host uses the intake base/branch for git checks and PR publication. The owner
merge gate continues to require observed merge ancestry; the host never merges.

For `tests_fail`, the hosting adapter reads trusted new-test expectations from
`runs/<id>/context/test-expectations.json`:

```json
{"new_tests":["module::new_acceptance_case"],"baseline_failures":[]}
```

These expectations must be supplied independently of the result being checked.
The `test` project command currently uses Rust libtest report parsing; missing
expectations or an incompatible report cannot satisfy RED. The pilot/config
pack work is tracked separately by #125.

## Validation

Independent frozen tests and RED/GREEN evidence are under
[`evidence/124`](evidence/124/acceptance.md). That document also contains the
manual macOS visual smoke checklist. Automated state/pane checks do not claim
native desktop visual or notification delivery was manually exercised.
