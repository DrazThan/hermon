# Intake, doctor and CLI controls

Slice 8 adds CLI intake and durable controls. Runs are queued at their explicit
starting step. The menubar/foreground hosting loop and pipeline UI are Slice 9
(#124); this slice does not start a background hub.

```sh
hermon doctor workflows/sdlc --project projects/example/project.yaml --ticket EX-123
hermon run workflows/sdlc EX-123 --project projects/example/project.yaml \
  --base main --confirm
hermon status --json
hermon status RUN-ID --json
hermon answer RUN-ID 'Use the existing API'
hermon approve RUN-ID
hermon retry RUN-ID green
hermon override RUN-ID green --reason 'Owner accepts the documented exception'
hermon pause RUN-ID
hermon resume RUN-ID
hermon cancel RUN-ID
```

`--confirm` confirms the supplied workflow and selected base/initial step. The
project supplies defaults; `--base` and `--start` select explicit alternatives.
Intake does not infer an entry from sorted step names or use a model to grant
owner approval. `--run-id` supplies an optional explicit identifier; otherwise
a unique local ID is generated. The branch is `<branch_prefix>/<run-id>`.

Every run/control/status command accepts `--data-dir` (default: the platform
application-data directory plus `hermon`). It contains `runs/<run-id>/`. Doctor
uses temporary probe state and does not allocate a run or worktree.

## Project configuration

Project files accept strict YAML or JSON, with the same duplicate-key, alias,
anchor and unknown-field rejection used by workflow files. Paths are resolved
relative to the project file. Command argv elements are literal arguments;
there is no implicit shell. For example:

```yaml
repo: /path/to/repository
worktree_root: /path/to/hermon-worktrees
branch_prefix: flow
base: main
start: plan
env:
  PATH: /path/to/tools:/usr/bin:/bin
  HOME: /path/to/home
bots:
  planner:
    Command:
      start: [codex, exec, --json, '-']
      resume: [codex, exec, resume, '{session}', '-']
      task_stdin: true
      protocol: CodexJson
      family: openai
commands:
  project_check:
    argv: [cargo, test, --workspace, --locked]
    timeout_seconds: 600
tracker:
  endpoint: https://api.linear.app/graphql
  token_env: HERMON_LINEAR_TOKEN
  owner_ids: [linear-owner-user-id]
  owner_mention: '[Owner](https://linear.app/team/profiles/owner)'
code_host:
  repo: owner/repository
  remote: origin
```

The bots map must supply every bot referenced by the selected workflow. Other
command protocols are `ResultFile` and `ClaudeJson`; their start/resume/task
bindings use the existing [process adapter contract](process-supervision.md).
`{task}`, `{workdir}` and explicit `{session}` placeholders are expanded once.
Hermes profiles remain blocked by the existing unverified shared-gateway
cancellation capability; a trivial prompt cannot waive that requirement.

`env` is the complete child environment. Configure tool paths and any required
authentication variables explicitly; GUI/launchd shells do not load shell
startup files. The central Linear token is read from the host environment
variable named by `token_env`. Neither that token nor project command
environments are copied to run snapshots, task files, or event receipts.

Doctor checks each distinct required bot with a trivial task and a bounded
owned process group, checks project commands under `env`, and authenticates to
the tracker and GitHub. Missing tools identify the configured PATH and a fix.
Bot exit zero alone cannot establish successful completion. Commands have
configured deadlines; bot probes have a 30-second deadline. Probe output is
bounded and excluded from diagnostic errors to avoid exposing credentials.
Doctor executes project commands: configure checks that are safe to repeat.

Intake reads the ticket and its comments, validates templates, runs doctor,
resolves the selected base commit, and only then creates the branch, worktree
and run directory. It snapshots workflow source and all seven selected template
bodies and SHA-256 hashes. Changes to the source files do not rewrite these
snapshots. It exports the issue to `context/issue.md` and posts the started
comment through the central tracker app. A post-doctor notification failure
leaves a named block and a durable idempotent started receipt; fixing credentials
and retrying the blocked starting step retries delivery.

## Controls and persistence

The JSON status envelope is always `{"schema":1,"runs":[...]}` for a single
run or the full collection. Each record includes its selected base, exact base
commit, branch, worktree, workflow/template hashes, pause/cancel flags and the
serialized hub run (current step, attempts, block, gates, events and PR).
Unknown run IDs fail without creating a directory.

Each control holds `runs/<id>/run.lock`, validates current state, and appends
ordered `events.jsonl` entries with `schema`, `run_id`, `sequence`, `kind`,
`step` and `reason`. Entries also contain a recovery snapshot. The journal is
written before the atomic `run.json` snapshot. Complete committed lines can
repair an interrupted snapshot write; only an incomplete trailing line is
removed during recovery. Invalid controls append nothing.

Retry and override must name the current blocked step and require confirmed
worker settlement. Override requires a nonempty reason and cannot waive an
owner gate. Answer requires the current settled owner question and retains its
previous session handle so the process adapter can deliver the answer on resume.
Approve passes only an open, current `owner_go` generation. It cannot pass
`owner_merge`, which still requires code-host merge and ancestry verification.
CLI answers and approvals create durable central-app notifications explicitly
labeled as entered in Hermon. Failed mirrors retain their outbox entries and
report that the local control has already been applied.

Pause stops new dispatch; it does not signal the active worker. Resume clears
the pause. Cancel persists terminal intent before requesting a stop from the
run's owned supervisor. It never signals an arbitrary stored PID. Late success
cannot accept a cancelled step, start a successor or resurrect the run.

The hosting loop must use the same run lock and shared `control::save` API,
respect the persisted hub flags, and retry `intake::announce` and
`control::flush_notices` outboxes. It must hold the lock during those calls.
External project configuration remains host-owned so credentials can be fixed
without editing a run. Live vendor/tracker interaction is not part of the
synthetic acceptance suite under `docs/flow/evidence/123/`.
