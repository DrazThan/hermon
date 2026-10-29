# Hub core

Slice 3 adds a deterministic state machine to `hermon-flow`. It runs against
injected agent, tracker, code host, git and clock interfaces. The acceptance
suite uses in-memory fakes; this slice does not launch processes or contact a
tracker or code host.

The caller selects the starting step explicitly. `advance` reconciles the
current state and moves the run forward until it needs another observation.
The workflow remains the routing authority. Successful bot results must also
pass the git adapter's checks before evidence is accepted. Gate decisions come
from the tracker or code host adapter, never from a bot's result.

`Run` contains the current step, attempts, events, alerts, retry counters and
current evidence references. It can be serialized with the existing
`RunStore`. A host must hold a per-run lock, persist changes and deliver events
before calling the core again. Process supervision, crash reconciliation,
tracker reply matching, git commands and durable external-effect delivery
belong to the subsequent integration slices.

The agent interface distinguishes process exit, successful agent completion
and a valid step result. All are necessary for success. A worker whose outcome
is unresolved cannot be resent: the core retains its handle until the adapter
confirms settlement. This represents the supervision gate from the
[agent CLI spike](agent-contracts.md).

The core supports manual retry and override with a recorded reason. These
actions leave subsequent steps under automatic workflow control. Gates still
require owner evidence, and unresolved workers must settle before either
manual action can release another attempt.

`advance(&mut Run, &Workflow, &mut Dependencies)` returns after starting one
bot, waiting for a gate or capacity, or recording a terminal state. Repeating a
call with the same running or waiting observation leaves the state unchanged.
The explicit `Run::new(run_id, starting_step)` avoids guessing an entry from
the workflow's sorted map or its return edges.

Timeouts and stall thresholds use seconds from the injected clock. A timeout
is recorded before `Agent::stop`; a stall alert leaves the worker running.
`Poll::Unsettled` and observation errors retain the handle and block resend.
A later `Finished` confirms settlement and still requires a manual retry.

Review `max_rounds` counts executions, stopping immediately if the last allowed
review asks for changes. `max_runs` bounds correction steps. Transient retries
are counted beyond the initial attempt. Checks may rerun the bot once on an
unknown check failure; the second failure alerts. Capability failures require
owner action. Manual retry resets the transient and check-failure counters;
it does not raise the workflow's execution limits.
Successful transitions also reset those counters, giving a later visit to the
same step its full retry allowance.

Owner GO requests have a persisted generation number. The tracker must accept
only an exact authorized reply for that generation. The code host must confirm
both merge state and merge ancestry before releasing the merge gate. Adapters
own those validations; their fake implementations provide the observations in
this slice. There is no merge operation on the code-host interface.

Evidence revisions are counted per tag, so `red_fix` produces a new RED
reference for subsequent starts and checks while prior attempts retain the old
reference. Agent starts use `(run_id, step, number)` as their idempotency key.
The run-bound git adapter must use `(tag, revision)` similarly, and opening a
PR must return the run's existing PR when repeated. These contracts allow later
adapters to reconcile a crash between an external effect and a snapshot write.
This core alone does not provide an exactly-once durable transaction across
external services and the run store.

Alerts are deduplicated within an unchanged wait. Failed delivery propagates
an error without marking the alert delivered; the next call retries delivery.
New gates, attempts and manual actions create new notification generations.

The independent acceptance tests and their freeze records are under
`docs/flow/evidence/118/`.
