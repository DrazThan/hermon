# Independent issue 124 validation

All 23 frozen acceptance/regression tests passed independently against the final
implementation. `independent-green.txt` records the exact run:

```text
cargo test -p hermon --locked --test flow_host --test flow_pipeline --test flow_pipeline_review --test flow_pipeline_visual --test flow_host_isolation
```

The fake tracker binds a loopback HTTP port, so this run used the approved
unsandboxed test execution. It contacts no real provider or tracker and sends
no real desktop banners. The toolchain is the one recorded in `acceptance.md`.

All five test files match their original freeze manifests: `freeze.sha256`,
`review-freeze.sha256`, and `host-isolation-freeze.sha256`. No frozen assertions
were relaxed or modified to fit implementation.

Independent review found and reproduced three material gaps after the first
GREEN: qualified production roster IDs did not match raw worker session IDs;
unavailable session selection retained an unrelated pane; and one broken run
lock could abort all host reconciliation. Each received additional tests and
RED evidence before its production correction. Final inspection confirms:

- Local session routing decodes the full Route envelope/vendor ID, rejects
  remote or malformed identities, and refuses ambiguous matches. Both UIs
  send the qualified key to the existing engine pane command.
- Unavailable selection clears the prior session and closes its pane. Pipeline
  mode prevents normal roster fallback from silently selecting another session.
- The host isolates directory, lock, snapshot, adapter, and save failures within
  each run, and continues later runs. Busy run locks are skipped without
  blocking transcript polling. Paused/cancelled runs do not dispatch; cancelled
  active attempts use the existing durable stop-request recovery.
- The menubar starts the host thread while TUI/GUI poll shared snapshots. The
  engine delivers flow alerts through the existing notification backend and
  shared mute/configuration decisions. Foreground hosting now checks existing
  notifier arbitration while continuing reconciliation.

The implementer also added a headless GUI regression for switching to pipeline
mode from a zoomed session; the final draw condition preserves pipeline rows.
The independent frozen suite includes a real ratatui frame entered through the
keyboard shortcut and executable queued-to-Running-to-Done progression using a
detached fixture worker with the configured project PATH/environment.

No remaining material issue was found in this review. The native macOS
menubar/GUI visual smoke checklist in `acceptance.md` remains unperformed; the
headless tests do not claim desktop interaction or actual banner appearance.
Workspace-wide build, test, clippy, formatting and release validation are
recorded separately by the implementer.
