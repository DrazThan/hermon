# Workflow files and run storage

`hermon-flow` supplies the file contracts for [slice 2](https://github.com/DrazThan/hermon/issues/117)
of the [bot pipeline plan](https://github.com/DrazThan/hermon/issues/114).
It is a library; starting bots, running checks, passing gates, posting comments,
and exposing CLI commands belong to later slices.

## Workflow validation

`Workflow::load(path)` reads one version-1 YAML document.
`Workflow::from_str(path_label, text)` applies the same validation to in-memory
text, retaining the label in errors. The SDLC example is exercised by the
independent acceptance fixture in
`crates/hermon-flow/tests/fixtures/sdlc.yaml`.

Mappings reject unknown and duplicate keys. Anchors, aliases, merge keys, and
explicit tags are outside the supported YAML subset. Literal punctuation in
quoted strings and comments remains valid. Each step has exactly one of `bot`,
`gate`, or `action`; check names and arguments are validated at load.
All `next`, `on_block`, and `on_verdict` destinations must exist.

Every automatic cycle must pass through a step with a positive `max_runs` or
`max_rounds`. An `owner_go` or `owner_merge` gate is also a boundary: repeating
the cycle requires a fresh owner decision. Removing those boundaries must leave an acyclic
graph, so an unrelated bounded branch cannot hide an infinite loop. These
limits describe the contract for the future hub; the loader does not execute it.

## Comment templates

`Templates::load(project_templates, pack_templates)` resolves each of the seven
template names independently: project directory, pack directory, then compiled
default. The arguments name template directories themselves. The names are
`started.md`, `question.md`, `go_request.md`, `answer_ack.md`, `step_done.md`,
`alert.md`, and `outcome.md`.

The selected text is validated when loaded. Placeholders use `{{name}}` with
no expressions or logic, and each template has a defined set of permitted names.
`question.md` and `go_request.md` must include `{{owner_mention}}`.
`Templates::get(name)` returns the selected, validated text. Rendering, tracker
escaping, and appending the hub's hidden request marker belong to the later
tracker integration.

The supported placeholders are:

| Template | Names |
|---|---|
| `started.md` | `workflow`, `base_branch`, `branch`, `run_status_cmd` |
| `question.md` | `owner_mention`, `bot`, `step`, `question`, `technical`, `options`, `run_status_cmd` |
| `go_request.md` | `owner_mention`, `artifact_links`, `technical`, `run_status_cmd` |
| `answer_ack.md` | `owner_mention`, `bot`, `step`, `answer`, `technical`, `run_status_cmd` |
| `step_done.md` | `bot`, `step`, `summary`, `technical`, `artifact_links`, `run_status_cmd` |
| `alert.md` | `owner_mention`, `bot`, `step`, `summary`, `reason`, `technical`, `run_status_cmd` |
| `outcome.md` | `workflow`, `summary`, `technical`, `pr`, `merge_commit`, `docs`, `artifact_links`, `run_status_cmd` |

## Run storage

`RunStore::new(directory)` selects a single run directory. `write_run` and
`read_run` use Serde-compatible state types so the later hub can define its
state schema. `append_event` writes a JSON value followed by a newline to
`events.jsonl`. State and event writes are separate operations; the hub owns
their ordering and reconciliation.

The committed `run.json` is authoritative. Interrupted temporary writes are
never promoted over it. A directory without a committed state returns no run;
a corrupt committed state returns an error. Callers must hold the future hub's
per-run lock when coordinating state updates across processes.

`write_run` serializes before touching disk, writes a unique temporary file
in the run directory, syncs it, atomically replaces `run.json`, and syncs the
directory. Event writes serialize first, open with append semantics, write a
complete JSONL record, and sync the file and directory. An I/O error can occur
after data reaches disk; callers should reconcile state before retrying.

## YAML dependency

The selected parser is `yaml-rust2` 0.13.0 with default features disabled.
Its [upstream maintenance policy](https://github.com/Ethiraric/yaml-rust2)
promises basic maintenance and a stable API. Its
[event API](https://docs.rs/yaml-rust2/0.13.0/yaml_rust2/parser/enum.Event.html)
exposes anchors, aliases, tags, and mapping entries, allowing the loader to
reject unsupported syntax and duplicate keys before building the workflow
model. The parser is pure Rust. Disabling encoding support keeps this UTF-8
file loader's dependency set small.

On 2026-10-29, the complete, non-truncated
[RustSec database tree at `ee1a49a`](https://github.com/rustsec/advisory-db/tree/ee1a49a2769a24a596afc1c20cb33748d2769586)
contained no advisory entry for `yaml-rust2`. This is a point-in-time dependency
check, not a guarantee about future advisories. The same check found no entries
for its dependencies `arraydeque`, `hashlink`, `foldhash`, or `equivalent`, or
for the schema diagnostic helper `serde_path_to_error`. Its locked `hashbrown`
0.17.1 is outside the affected version of
[RUSTSEC-2024-0402](https://rustsec.org/advisories/RUSTSEC-2024-0402.html).

Acceptance tests and their freeze record live in `docs/flow/evidence/117/`.
The current manifest is `frozen-v3.sha256`: it preserves the original acceptance
behavior after a documented lint correction and adds a regression for long,
shallow workflow graphs. The original freeze and RED output remain archived.
