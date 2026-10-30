# Independent test correction after partial GREEN

`crates/hermon/tests/flow_red_fix.rs` runs an isolated end-to-end fixture:
accepted planner expectations → behavioral RED → GREEN commits production and
reports a test defect → independent RED_FIX commits only tests and re-establishes
behavioral RED → GREEN completes production against the superseding freeze.

The correction must reach the final gate without owner retries. RED_FIX's
`only_paths_changed:tests` check applies to its own attempt delta, not earlier
production work. The superseding RED note must freeze the independent test path
and exclude the existing production path, so the rerun GREEN may finish it.
The test checks the actual git note and final committed production content.

`red-fix-red.txt` records the intended behavioral failure: despite RED_FIX only
editing `tests/red.test.js`, the host rejects earlier `production.txt` as outside
the test path set. The check retry then ends in an owner-visible block. No frozen
acceptance file or product source was modified by this independent test writer.
