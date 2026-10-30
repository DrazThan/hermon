# Additional independent host isolation regression

After the original 22 tests passed, code review found that `Host::tick` handled
snapshot and adapter failures per run but propagated directory/lock-open
failures out of the whole tick. A damaged `run.lock` path in one run could
therefore starve every later run on every tick. Snapshot polling already
handled this error per run.

`invalid_run_lock_cannot_starve_other_runs` creates a directory at one run's
lock path to provoke a deterministic filesystem-open error, then requires
snapshot polling and repeated host ticks to keep returning the other healthy
paused run. This requires no agents, credentials, network, or production edits.

The supplemental file is frozen separately in `host-isolation-freeze.sha256`.
`host-isolation-red.txt` captures the original failure before correction. Both
earlier freeze manifests remain unchanged.
