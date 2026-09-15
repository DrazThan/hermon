# Codex rollout fixtures

These are invented, sanitized records for the private format described in #104
(Codex CLI 0.154.0, inspected checkout 9874215). No real transcripts, prompts,
credentials, base instructions, or encrypted reasoning are included.

`mixed.jsonl` exercises response_item message/custom_tool_call/
custom_tool_call_output and their event_msg item_completed duplicates.
`events.jsonl` describes the supported internally tagged item_completed shapes:
UserMessage.content, AgentMessage.text, CommandExecution.command and
aggregated_output. Reasoning and Extension are intentionally ignored. Other
function-call variants are unsupported rather than guessed from the public API.

Response messages win if available before fallback is flushed (poll boundary). Otherwise a role chooses event-only messages for that turn;
subsequent canonical messages for that role are suppressed. Completed command
items use the same turn-scoped stream selection against custom tool calls/results;
call correlation is still maintained even when canonical tool rendering is suppressed. Stable item IDs
are deduplicated per turn, never message text. A new task_started resets stream
selection. This streaming policy cannot retroactively replace an event-only
message already displayed. Opening mid-file reconstructs selection by silently
parsing the prefix; only records starting within Replay.bytes may emit lines.

Only usage for the session's own thread is counted. Explicit foreign thread IDs
are ignored (their separate rollouts can be discovered independently). Missing
thread IDs mean the current rollout. thread_token_usage snapshots supersede
info.total_token_usage snapshots; either supersedes deduplicated response_id
usage. Cached input and reasoning output are already included in parent counts.
No turn totals or child totals are added and no price is estimated.

Reader limits: 1 MiB per record, 8 MiB raw input per poll, 32 directory levels,
16,384 IDs of at most 512 bytes per turn/accounting ledger; event fallback queues
hold at most 256 messages clipped to 16,384 characters. Excess records are skipped
through newline; malformed/oversized runs produce one notice until a valid
record restores normal parsing. Unknown variants are silently ignored. Once an
ID ledger fills, new identified events are ignored until the next turn (response
accounting resumes when a cumulative snapshot arrives). Roster and tail state
are independent. Unchanged files never reparse history. Rewrites are detected
by identity, size, timestamp/ctime, and a 64-byte consumed-boundary check.

CLI registration, the X: roster prefix, --codex-dir, and remote agent end-to-end
wiring belong to the integration issue. The source returns existing serializable
SessionMeta and StyledLine values without protocol changes.
