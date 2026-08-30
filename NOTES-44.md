# Issue #44 — notes for the PR

## What changed

- `src/notify.rs`: delivery shell (`probe`/`deliver`, `terminal-notifier` →
  `osascript` → `notify-send` → silent) and the `decide_alerts`/`AlertHistory`
  core were already in place from #43. Found and fixed one real bug in the
  startup-grace latch while doing the live check below (see "Bug found").
- `src/engine.rs`: wired the scan tick to build one `LifecycleTransition` per
  roster row (`build_transitions`), run `decide_alerts`, fire `notify::deliver`
  for each resulting alert, and forward it as `Event::Alert` for the UI.
  `pump_panes` now records the latest `Sem::Error` line per key into a small
  `errors` map that `build_transitions` drains — the only way `decide_alerts`
  can see a session's error line, since only tailed sessions have any
  transcript content to inspect.
- `src/ui/mod.rs`: `[m]` toggles `App::muted`, sends `UiCmd::SetMuted` to the
  engine, and shows `🔕` (or `[muted]` under `HERMON_ASCII`) in the footer.
  Added the `[m]ute` hint to all three footer strings and a help-overlay line.
- `src/ui/palette.rs`: added `muted` to `GlyphSet` and a `mute_indicator` fn.
- `src/cli.rs` / `src/config.rs` / `src/lib.rs`: already had the
  `--no-notify*`/`--notify-cooldown` flags and `NotifyCfg` wiring from the
  in-progress state of this branch; left as-is.
- `README.md`: new "Notifications" section, including the osascript
  generic-icon caveat and the flag table.

## Bug found during the live check, and the fix

The acceptance criteria call for "restart of hermon mid-fleet → zero
banners." `AlertHistory`'s startup-grace latch (`booted`) correctly
suppresses alerts on the very first tick after boot — but for a session
already sitting in `Attention(Stuck)`/`Attention(PermWait)` at boot, it did
**not** suppress the alert on the tick immediately after: since the grace
tick never called `try_fire`, that (session, kind) had no cooldown history
by the second tick, so it read as a brand-new entry into attention and fired
right away. A restart mid-fleet would still bang out a banner, just one
scan tick late.

Fixed in `src/notify.rs`: `AlertHistory::seed_attention` starts the
refire clock during the grace tick itself for any session already in
`Stuck`/`PermWait`, so the next tick sees it as "already announced," not
fresh. Covered by a new unit test,
`notify::tests::restart_mid_attention_does_not_alert_once_grace_lifts`, and
reproduced end-to-end below.

## Live-check evidence

This environment is the real macOS machine the branch is developed on
(Darwin 25.6.0), not a headless CI box, so I ran as much of the live check
as I could without a human watching the screen:

1. **Direct `osascript` smoke test** — ran the exact command shape
   `deliver()` builds for the `Osascript` path by hand:
   ```
   $ osascript -e 'display notification "hermon live-check smoke test" with title "hermon" subtitle "C:livetest"'
   exit=0
   ```
   `terminal-notifier` and `notify-send` are not installed here, so
   `notify::probe()` resolves to `Notifier::Osascript` on this machine —
   confirmed via `which`.

2. **End-to-end through the real engine** — added
   `tests/live_notify_check.rs` (three `#[ignore]`d tests, not part of the
   normal `cargo test` gate — run with
   `cargo test --test live_notify_check -- --ignored --nocapture`). These
   drive the real `Engine::spawn_with_clock` (real `notify::probe()`/
   `deliver()`, so a real banner fires) against a `FakeDeck` that forces
   liveness transitions directly, since reproducing a genuine Claude Code
   permission prompt or a genuine 600-second stuck tool call isn't something
   I can trigger from here:
   - `live_check_perm_wait_fires_a_real_banner`: session goes `Live` →
     `Attention(PermWait)` → `Event::Alert(kind: PermWait)` observed on the
     engine's event channel (meaning `deliver()` already ran against the
     real `osascript`). **Pass.**
   - `live_check_stuck_fires_a_real_banner_after_the_ceiling`: same for
     `Attention(Stuck)`. **Pass.**
   - `live_check_restart_mid_fleet_raises_no_banner`: engine boots with a
     session *already* in `Attention(PermWait)` on the very first tick,
     then five more ticks pass with no change — zero `Event::Alert`
     observed, confirming the bug fix above. **Pass** (failed before the
     fix, which is how the bug above was found).

## What this does *not* cover — please do before merging

I have no way to see the screen from here (no screenshot tool in this
environment), so none of the above is a substitute for actually watching a
banner render. Please, on a real run of `hermon watch`:

- Trigger a real Claude Code permission prompt and confirm a `⏸` banner
  actually appears (title `hermon`, subtitle the session's roster key, body
  the pending-tool detail).
- Run a real `sleep 600` tool call and confirm a `⚠` banner appears once the
  stuck ceiling blows.
- Restart `hermon` while a fleet has a session already mid-attention and
  confirm no banner fires on restart (this is what
  `live_check_restart_mid_fleet_raises_no_banner` reproduces synthetically,
  but a real restart is worth eyeballing once).
- If you have `terminal-notifier` installed, confirm the icon/sound path
  looks right too — everything above exercised the `osascript` fallback
  only, since that's what's on this machine.
