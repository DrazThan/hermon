# ACP doctor preflight freeze

The independent CLI tests in `crates/hermon/tests/hermes_acp_doctor.rs` require
doctor to probe an opt-in HermesAcp bot through the same public ACP and runtime
usage checks as a real attempt. They reuse the frozen fake Hermes executable.

A valid ACP end_turn, the probe's actual result.json and verified main-session
SQLite usage must make the bot finding pass. The overall doctor command still
fails because the fixture's tracker endpoint refuses connections. This separates
bot readiness from unrelated authentication failure. Doctor must not allocate
any run or configured worktree.

Two negative cases require a failed bot finding when main usage is absent or
when end_turn succeeds but the fake writes its result to an unrelated file.
The latter proves transport completion cannot invent the probe's missing result.
No fixture contacts a live gateway, tracker or provider.

`acp-doctor-red.txt` records two intended behavioral failures while the adapter's
doctor arm is still unimplemented. The missing-usage rejection already passes.
The three tests compile, run the real CLI and inspect its JSON findings.
`acp-doctor-freeze.sha256` freezes this contract and the new test file before
successful doctor integration. The shared fake keeps its transport freeze.
