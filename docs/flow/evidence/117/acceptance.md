# Slice 2 acceptance tests

Written independently from implementation on 2026-10-29 for [issue #117](https://github.com/DrazThan/hermon/issues/117), using [plan #114](https://github.com/DrazThan/hermon/issues/114), sections 4–6 and 8. The test writer owns `crates/hermon-flow/tests/`; the implementer owns production code. All tests compile against behavior-free public API stubs before implementation is released.

The 24 integration tests cover all eleven named issue acceptance tests, plus these failure modes:

- Workflow: nested unknown and duplicate keys, unsupported anchor/alias/tag/merge syntax, unresolved `next`/block/verdict routes, unbounded cycles including a bypass cycle in an otherwise bounded strongly connected component, invalid versions/step kinds/limits/check arguments, multiple documents, and valid literal YAML punctuation. The complete example SDLC workflow is a fixture. Owner GO is an explicit owner-controlled boundary for the scope-change route; automatic loops still need a positive `max_runs` or `max_rounds` on every cycle.
- Templates: project then pack then built-in selection, all seven built-in names, load-time unknown and per-kind placeholder errors, owner-mention requirements, invalid overrides without fallback, malformed placeholders, and supported question placeholders.
- Run store: JSON round trip and reopening, replacement rather than truncation (an already-open file descriptor retains the original snapshot), serialization failure preserving committed bytes, partial and orphan temp files never becoming committed state, corruption errors with a file path, and append-only JSONL records with a trailing newline. Generic serialized state intentionally leaves the future hub schema to Slice 3. Crash durability is supplemented by production code review; no timing-based crash test is used.

Public API agreed before tests: `Workflow::load(path)` / `from_str(label, text)`; `Templates::load(project_dir, pack_dir)` / `get("name.md")`; `RunStore::new(dir)` / generic `write_run`, `read_run` and `append_event`. Template rendering, hidden markers, escaping, CLI preview, process supervision and hub behavior belong to later slices.

The freeze manifest covers the three integration test files and SDLC fixture. Frozen acceptance files must remain byte-for-byte unchanged during implementation. A genuine test defect must return to the independent test writer for a reviewed correction and a superseding freeze.
