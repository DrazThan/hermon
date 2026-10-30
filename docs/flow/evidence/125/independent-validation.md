# Issue 125 independent final validation

The independent acceptance/review agent reran all 25 issue-specific frozen tests
after the final implementation changes. All passed. The nine freeze manifests
also verify byte-for-byte unchanged; see `independent-freeze-check.txt`.

## Executed checks

```sh
cargo test -p hermon-flow --test sdlc_pack --test code_host_merge_receipt --locked --offline
cargo test -p hermon --test flow_roles --test flow_inputs --test flow_vitest \
  --test flow_vitest_errors --test flow_vitest_skipped --test flow_block_reason \
  --test flow_publication --test flow_red_fix --locked --offline
```

`independent-flow-green.txt` records 5 passing tests and
`independent-cli-green.txt` records 20. The toolchain and Cargo/Rustup homes were
explicitly bound to the installed `/private/tmp/hermon-116-rust` runtime.
Local subprocess supervision and fixture-server tests ran with sandbox elevation.
An initial sandbox supervision denial is preserved separately in
`independent-flow-sandbox.txt`; it is not a product failure or behavioral RED.
No live tracker, GitHub repository, RUM ticket or vendor session was changed by
these tests. Workspace build/test/Clippy/format/release are recorded separately
by the implementing agent.

## Final code review

The requested integration fixes agree with the tested behavior:

- Intake snapshots optional role prompts before run allocation; bounded readable
  in-pack files and later snapshot hash checks prevent mutable pack guidance from
  replacing the task's frozen role text. Generic packs remain compatible.
- Accepted producer attempts, rather than arbitrary larger numbered directories,
  supply artifacts. The per-reconciliation observation cache makes just-completed
  inputs and exact typed block reasons available to immediately routed successors.
- The accepted planner's independent test expectations feed RED. Vitest reports
  preserve assertion IDs/counts, reject invalid/suite-error/unrelated failures,
  and support the installed Vitest 2.1.9 skipped status as ignored evidence.
- A superseding RED captures its pre-worker baseline for path ownership, while
  its note retains the named test-path filters and accumulated test delta. The
  real regression proves earlier partial production commits do not block test-only
  correction and do not become frozen production paths on the next GREEN.
- Owner GO carries the accepted published plan through the central app and keeps
  its immutable reply marker. Publication is bounded and repeated reconciliation
  does not duplicate the request. A completed Docs run emits its actual summary
  through the configured outcome template, including on Done-state retry.
- The exact merge SHA is persisted only after observing MERGED and confirming
  ancestry against the freshly fetched selected base. The host copies that
  verified receipt for Docs/outcome; the adapter's independent fixture verifies
  restart retention and rejection of OPEN/non-ancestor observations.

The previously reported roadmap table separation was corrected. README,
configuration and pilot runbook describe the opt-in hub, monitoring boundary,
central tracker, owned process control, remaining capability gates and pending
pilot without asserting a completed live run. No remaining actionable finding
was identified within the reviewed issue-125 change scope.

## Local migration and outstanding live acceptance

The separate `independent-migration-validation.md` records exact source/installed/
backup hashes for all five approved profiles, the unchanged installed
`sdlc-reliability` tree and the paused controller readback. This review only read
those live local files.

The owner deferred live intake to a guided pilot and has not selected/enrolled a
small RUM ticket. Named Hermes profile dispatch remains blocked by unverified
shared-gateway cancellation; verified distinct GREEN/Review model-family
provenance is also required. Those gates must be resolved before a real pilot.
Fixture success does not establish a live Owner GO, RED/GREEN, Review/PR,
30-minute block-routing/alert timing, owner merge or Docs acceptance. Issue 125
and M11 must retain that outstanding live acceptance rather than being closed by
these synthetic results.
