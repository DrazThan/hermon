# Process supervision

Slice 4 supplies a `ProcessAgent` implementation of the hub's `Agent` interface
and a small `hermon-flow-worker` executable. Install the worker alongside the
host and pass its absolute path to the adapter. The host supplies a run root,
worktree, bot definitions and deadline. It writes `task.md` before starting an
attempt under `<root>/<run>/steps/<step>/<number>/`. The worker retains stdout
in `log`, stderr separately, process metadata and a durable completion receipt.

The worker runs in a detached session and starts the configured argv directly
in its own process group. It owns the deadline even if the hub exits. A new
adapter uses the same persisted handle to observe that worker; it never sends
signals to an arbitrary persisted PID. Cancellation is a durable request to the
worker, which records cancellation before signalling only the group it created.
Timeout and cancellation cannot be changed into success by a later zero exit.
The leader stays unreaped until all members have stopped; a queued kill signal
alone is insufficient. Group settlement uses the host's `/bin/ps` on macOS and
Linux. A stuck group retains an unresolved receipt instead of permitting retry.

An atomic launch reservation and per-attempt lock make repeated starts return
the same handle. A per-bot lock serializes capacity across runs sharing the same
root. A live worker holds a lifetime file lock; a dead supervisor without a
completion receipt produces a `transient` observation error. The hub retains
the unresolved attempt for reconciliation. It cannot infer worker settlement
from a numeric PID or automatically resend work after that error.

Command definitions provide distinct start/resume argv, optional task-file stdin,
an agent family and a completion protocol. `{task}`, `{workdir}` and `{session}`
are literal substitutions within argv elements. A missing resume command starts
a fresh invocation; the host must include prior context and owner answers in
the new task file. No adapter guesses a session or invokes `--last`.
Relative executable paths and relative PATH entries resolve in the worktree.
A result already present before a fresh launch is archived as
`result.before-launch.json`; only output written by the new process is delivered
to the hub. Known failures before spawn leave the attempt available for repair
and retry. Uncertain launches retain their reservation.

`ResultFile` uses the process exit and step result as completion evidence.
`CodexJson` additionally requires a successful `turn.completed` event;
`ClaudeJson` requires a non-error result with `terminal_reason: completed`.
The hub still validates the result schema and runs its checks. Session identity
alone never establishes successful completion.
Result and protocol reads are bounded to 16 MiB; unreadable or oversized evidence
cannot establish completion. Full stdout/stderr logs remain on disk.

The Hermes command builder preserves Slice 0's exact
`hermes -p <profile> chat -Q --query-file <task>` argv and explicit
`--resume <session>`. Its parser takes the final opaque `session_id:` trailer
from stderr. These legacy Hermes launches remain a capability block: the installed
CLI delegates to a shared gateway, and Slice 0 did not establish a supported
scoped cancellation mechanism during active tools. This slice adds no dependency
on the internal cleanup RPC and never stops the gateway. See
[the recorded supervision gate](agent-contracts.md).

The separate opt-in `HermesAcp` binding uses the public ACP v1 transport. Its
exact-session cancellation waits for the submitted prompt's terminal response;
transport death leaves gateway work unsettled and retains capacity. A pinned
version/current model and read-only main-call usage establish model-family
provenance before accepting a result. See [Hermes ACP](hermes-acp.md) for the
configuration, permissions and live evidence.
