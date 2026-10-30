# ACP owned-artifact permission freeze

Real run outputs live in an owned attempt directory, which can be outside the
workspace and system temp roots. The client may grant one write_file request
there without granting general command or filesystem approval.

The independent `acp_permission_scope.rs` tests require allow_once for the assigned
result.json and a normal artifact file beneath that exact attempt directory.
The fake offers allow_always too; it must never be selected. The raw request is
exactly a write_file tool with an absolute arguments.path.

Other tests deny relative paths, workspace files outside the attempt, another
attempt's files, symlinked parent escapes and symlinked output files. Hub state,
task, cancellation, logs, owner-answer, lock and hidden basenames remain denied.
The existing frozen dangerous-command test also remains in force.

A separate wrapper, `hermes-acp-artifact-permission.py`, derives these requests
from the unchanged frozen fake. It records the client's decision and never
writes the requested target. All processes and SQLite files are local fixtures.

`acp-permission-red.txt` records behavioral RED: the required owned write is
currently denied. The two denial tests already pass. The new test, wrapper and
contract are frozen before the narrow permission policy is implemented.
