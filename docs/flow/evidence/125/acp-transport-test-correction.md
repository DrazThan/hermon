# ACP workdir test correction

The first implementation run passed all nine provenance tests and five transport
tests. The remaining transport assertion compared the ACP cwd to tempfile's raw
path. On this Mac that path starts with `/var/folders`; ProcessAgent::new has
always canonicalized it to `/private/var/folders` before launching a worker.
Sending that canonical path is correct.

The independent test writer changed only the expected cwd spelling to
`fixture.workdir.canonicalize()`. The assertion still requires the exact configured
worktree and empty mcpServers. No product behavior or protocol expectation changed.
The existing agent acceptance suite uses the same canonical-path comparison.

The original manifest is `acp-transport-freeze.v1.sha256`; its original test source
is retained in `acp-transport-v1/hermes_acp.rs`. The original failed run is in
`acp-initial-validation.txt`. The active `acp-transport-freeze.sha256` records the
corrected independent test, unchanged fixture/contract and this explanation.
