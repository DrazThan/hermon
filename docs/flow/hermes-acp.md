# Hermes ACP adapter

`HermesAcp` explicitly opts a bot into the public `hermes -p PROFILE acp`
transport. It leaves the legacy `Hermes` binding and its supervision gate intact.
The five-profile example is [pilot.yaml](../../projects/rummyrune/pilot.yaml).

Each binding supplies `profile`, `model_id`, `version` and an existing absolute
`state_db`. Profile names accept ASCII letters, digits, underscores and hyphens.
The adapter verifies ACP v1, the `hermes-agent` identity, exact version and exact
current model before submitting a prompt. Unknown model/provider combinations
fail closed. The supported mappings are OpenAI Codex `gpt-*`, xAI OAuth `grok-*`
and the configured direct-SDK Anthropic `opus`/`claude-*` provider.

The public transport retains profile skills, credentials and configured MCP
servers. Hermon supplies the worktree as cwd and selects `accept_edits` mode.
Incoming client capability requests are denied. Permission requests can receive
only an offered `allow_once` for `write_file` or narrowly scoped `patch` updates
inside the exact owned attempt directory. The session, absolute path, canonical
parent, existing file type and protected metadata names must match.

Patch approval reads raw arguments, requires `mode: patch` and accepts only
canonical `*** Begin Patch` / `*** Update File: <absolute path>` /
`*** End Patch` framing with explicit, nonempty hunks. Each target must already
be a regular file; every target must pass before a multi-file patch is approved.
Add, Delete, Move, unknown operations, malformed headers, trailing path
whitespace, traversal, symlinks beneath the attempt, hidden paths and locks are
denied. Displayed titles and diffs do not establish scope. The original
`write_file` creation policy is unchanged.

Other tools, outside paths and relative paths are denied. Workspace edits use
Hermes' own `accept_edits` policy. No blanket approval or dangerous-command
permission is granted by the client.

Before submission, `process.json` records both stable ACP identity and current
Hermes session identity. An owner answer explicitly loads the recorded ACP
session. Cancellation intent is durable before `session/cancel`; only the exact
prompt's terminal response establishes settlement. A late `end_turn` cannot undo
timeout or cancellation. The adapter waits up to 30 seconds for cancellation.
EOF, ambiguous request errors or transport loss during submitted work leave the
attempt unfinished and retain capacity. The pinned adapter's explicit
`admitted_turn_failed` response (code `-32603`, message `Internal error`,
`error.data.details`) instead confirms a failed admitted turn. It becomes a
capability block, never a successful step, and does not trigger a follow-up.

For a dead older worker that recorded this exact failure but remained unsettled,
use `hermon reconcile RUN STEP --data-dir DATA`. This control holds the run and
worker locks and validates the complete bounded ACP trace against the owned
launch, pinned agent/version/model and session. It accepts only the expected
failed prompt response after the initialization/open/mode sequence and at most
three interim end turns. Missing, malformed, foreign, unmatched or trailing
frames, symlink receipts and live workers are refused. No transport or prompt
is started, and the launch, result file, trace and candidate remain unchanged.
The host must still observe failure settlement; an explicit `hermon retry`
remains required. Inspect existing background jobs and verify their actual
outcomes before retrying. Do not resend an uncertain turn or stop a shared
gateway to make it appear settled.

After a successful terminal response, the adapter reloads the current session
tip and opens the profile usage database read-only. It requires positive
`session_model_usage` main-task calls (`task=''`), excluding auxiliary calls.
It follows only compression parents, rejecting cycles and chains over 100
sessions. Every contributing model/provider must belong to the configured
family. Missing, unreadable or mixed-family evidence creates a capability block.
Actual usage, family and session identities remain in `process.json`. A missing
assigned `result.json` also blocks; an `end_turn` alone cannot fabricate success.

Doctor exercises the same transport, assigned-result validation and provenance
checks in scratch without creating a run, branch or worktree. Unknown settlement
preserves its scratch evidence. Doctor must pass for the complete project before
intake. Credentials belong in the host environment, never project YAML, task
files or exported receipts.

[Independent validation](evidence/125/acp-independent-validation.md) covers 22
frozen acceptance tests. The [live prerequisite record](evidence/hermes-acp/live-contract.md)
covers five-profile result/provenance probes and cancellation during active
tools with an unrelated control. These are prerequisites to the guided pilot
workflow. Owner GO, product acceptance, PR, owner merge and Docs require their
own live evidence.
