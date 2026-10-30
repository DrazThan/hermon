# Issue 124 independent acceptance freeze

The independent test writer authored these tests before production implementation.
The API seams were agreed with the implementer before writing tests. The tests
are behavior assertions over durable run records, process receipts, both UI
state machines, and the executable host; they do not contact a real tracker,
agent provider, or desktop notification service.

Frozen files are listed in `freeze.sha256`.

## Automated contracts

- `hub_single_instance_lock`: a data directory permits exactly one hub, other
  directories remain independent, dropping the owner permits reacquisition.
- `foreground_hub_uses_same_single_instance_lock`: the CLI shares the lock used
  by the embedding host, and reports the conflict rather than dispatching.
- `pipeline_view_rows_from_events`: past, current, gate, and future steps remain
  visible, with configured bot, attempt, elapsed age, historical block reason,
  and the recorded active session ID.
- `pipeline_session_uses_recorded_id_and_running_step_only`: exact ID resolution,
  saturating elapsed time, no prefix guessing, no inactive session opening.
- `pipeline_missing_session_receipt_is_still_visible`: the launch/receipt race
  does not hide a running step or bind it to an unrelated session.
- `tui_and_gui_open_running_pipeline_session_pane`: both consume the shared
  pipeline event, select the recorded session, and emit the existing OpenPane
  command. Invalid selection is harmless.
- `alert_on_stall`: the configured threshold is inclusive, duplicate polls do
  not repeat alerts, and each new stalled attempt can alert.
- `alert_on_owner_needed`: owner GO and owner questions use attention delivery,
  duplicate polls are quiet, a new gate request rearms the alert.
- `flow_alerts_honor_mute_config_and_terminal_states`: mute and notification
  configuration suppress alerts without consuming them; ready, done, and
  cancelled runs do not generate alerts.
- `foreground_hub_once_accepts_empty_data_directory`: foreground one-shot host
  works without an existing run, and exits release the lock.
- `foreground_hub_exposes_documented_control_flags`: CLI exposes data directory,
  one-shot execution, and notification opt-out.
- `foreground_hub_advances_queued_run_with_project_environment`: actual CLI
  hosting dispatches a queued run through the detached worker, prepares its
  task, resolves the configured project PATH, passes project environment,
  survives host restarts without duplicate dispatch, and persists completion
  and ordered journal events. Tracker delivery uses a local fake HTTP server.

## RED execution

`red-host.txt` captures executable tests against the unimplemented `hub`
subcommand. `red-pipeline.txt` captures the missing shared presentation API.
Compilation failures for the new public seam are expected in this RED phase;
the separate executable suite demonstrates runtime failure as well.

Toolchain invocation uses `/private/tmp/hermon-116-rust/cargo/bin`,
`RUSTUP_HOME=/private/tmp/hermon-116-rust/rustup`, and
`CARGO_HOME=/private/tmp/hermon-116-rust/cargo`.

## Manual visual smoke checklist

These items require a macOS desktop and an intentional fixture run. Automated
test success does not claim this checklist was performed.

- [ ] Start `hermon menubar` with the chosen flow data directory. A queued
      configured run advances while no terminal host is running.
- [ ] Start another menubar or `hermon hub` against that directory. Only the
      first host advances the run; the second surfaces a clear lock conflict.
- [ ] Open TUI pipeline view. Every step shows run, step, bot, attempt, age,
      status, and most recent block reason. Refreshes preserve useful selection.
- [ ] Open GUI pipeline view and confirm equivalent information and readable
      layout with both few and many runs, including an empty data directory.
- [ ] Select a running step in each UI. The existing transcript pane opens for
      its recorded session. A launch awaiting its session receipt remains
      visible and does not open another session.
- [ ] Reach owner GO and an owner question. Existing desktop notification
      delivery announces the run/step/reason once; mute and --no-notify suppress
      it. Reach the configured stall threshold and observe the stall alert.
- [ ] Quit the host and restart it. The lock is reusable, the running attempt
      is reconciled, and no second worker is launched.

No test edits are permitted after freeze without a separately documented test
defect, its reason, and a replacement hash; changing an assertion just to fit an
implementation is not a test defect.
