# Independent Hermes ACP metadata and runtime provenance acceptance

Frozen before implementing the opt-in `HermesAcp` adapter. Tests use only an
isolated fake public ACP executable and synthetic SQLite databases. No Hermes
profile, live session, vendor account or tracker is accessed.

The existing public `ProcessAgent` APIs are exercised with configuration decoded
from JSON, avoiding references to a not-yet-implemented Rust enum variant.
Initial behavioral RED: 8 tests fail at runtime because valid `HermesAcp`
configuration is rejected as an unknown variant; the unsupported-model case
already fails closed. The test executable compiles. Targeted Clippy passes.

Accepted behavior:

- Strict provider/model family mapping for OpenAI Codex `gpt-*`, Claude direct
  subscription `opus`/`claude*`, and xAI OAuth `grok-*`; unknown pairs cannot
  claim a review family or launch a process.
- `initialize` agent version must equal the verified version before a session is
  started. `session/new` currentModelId must match the configured model before
  any prompt is sent.
- A transport `end_turn` needs positive main-task usage for the actual Hermes
  session. Auxiliary title usage and unrelated sessions are excluded. Missing,
  zero-call, mixed-family or unknown-provider main usage becomes a host-produced
  capability block even when the fake agent wrote done.
- Model evidence receipts include profile, ACP and actual Hermes session IDs,
  configured model, verified family, contributing model/provider/call rows and
  settled status. The vendor DB bytes remain unchanged by inspection.
- Main usage from a parent is admissible only when that parent ended through
  compression. Traversal is bounded to at most 100 sessions; cycles and usage
  beyond the bound do not establish acceptance.
- A missing vendor DB cannot be created as a side effect of validation.

Commands:
`cargo test -p hermon-flow --test acp_provenance --locked`
`cargo clippy -p hermon-flow --test acp_provenance --locked -- -D warnings`

Shared fake CLI `tests/fixtures/hermes-acp.py` is owned/frozen separately by the
transport test writer. This freeze covers only this independent test source and
acceptance document.
