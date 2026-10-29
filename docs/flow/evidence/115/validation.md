# Independent validation — issue #115

Validated on 2026-10-29 against the unchanged acceptance checklist frozen before
implementation. Both `/tmp/hermon-115-acceptance.md` and
[acceptance.md](acceptance.md) have SHA-256
`dc5cb5c4cc27042242c0baa55983e8056998e5352b3060e147fe2279a7794f88`.

**Document completeness: PASS (A1–A8).** No unresolved documentary findings.
**Capability completeness:** the required observations are present for the tested
installed configurations. Production supervision remains gated as described below;
this validation does not accept an untested transport or cancellation of active tools.

This review read the document, all 22 transcript entries, fixture/harness scripts,
provenance, redacted Hermes snapshots and amended plan section 9. Small offline
checks verified transcript metadata, nonce continuity, status tables, local links
and fixture consistency. The reviewer did not run agent/model CLIs, replay live
probes, alter evidence or product code, or treat source inspection as a live test.

| Criterion | Verdict | Independent check |
|---|---|---|
| A1 — Provenance and reproduction | PASS | `provenance.json` records exact installed version/platform/source commands and output, executable resolution and the Hermes shell wrapper. The 22 transcript entries preserve argv, cwd, stdin, input snapshots, stdout/stderr, actual child status, deadline and signal scope. Harnesses and final fixture files support reproduction. Explicit redactions preserve the facts used by the document. |
| A2 — Hermes headless file contract | PASS | `hermes-success` used `-p default chat -Q --query-file`, `/dev/null`, no PTY and the specified cwd; it returned the requested reply and exited 0. The default-profile snapshot matches cwd/profile. `hermes-profile-default` and the tool-call excerpts demonstrate actual skill loading and configured MCP dispatch; the redacted decoded result has a search-results structure. Named-profile discovery and incomplete stream execution are qualified separately. |
| A3 — Session capture and continuation | PASS | Hermes start and resume stderr share the exact opaque `local-…` ID documented. Resume input contains no initial nonce, yet output returns it and exits 0. The document specifies stderr matching and distinguishes the stream viewer's empty JSON ID and alternative stderr line. |
| A4 — Codex/Claude equivalents | PASS | Both start/resume pairs return the initial nonce absent from follow-up input and retain their original machine-readable IDs. Commands match transcript argv and file-stdin behavior. Final fixture events show independent skill/MCP nonces and successful reads/calls. Configuration bypass, project-only settings, profile differences and direct stdin/wrapper requirements are explicit. |
| A5 — Exit outcomes | PASS | All twelve matrix statuses match transcript entries: Hermes 0/1/130/130; Codex 0/1/1/0; Claude 0/1/0/143 for success/model-error/SIGINT/deadline. Model-error output is provider/model rejection, not a parser/authentication failure. Owned-group SIGINT versus deadline SIGTERM, 5-second deadline, 8-second grace, unused SIGKILL escalation, positive child statuses, Python negative signal statuses and supervisor ownership are distinguished. The earlier Claude completion race is excluded correctly. |
| A6 — Evidence classification | PASS | The required exit observations are labelled observed and have captured results. Installed-source checks are separately identified; named-profile stream completion/resume and active-tool cancellation remain unverified. Startup failure is not called a model failure. Empty/start-only outputs are not called completion, and the model-reported Notion result count is explicitly attributed to the final reply. |
| A7 — Section 9 disposition | PASS | Local plan section 9 now specifies Hermes stderr capture, optional opaque IDs, separate command start/resume argv and task stdin, completion/error interpretation, supervisor-owned outcome state, shared-gateway cancellation gating and fresh-session fallback after reconciliation. These address the measured limitations before later slices depend on them. |
| A8 — Deliverable and workflow | PASS | The short main document links exact evidence and accounts for every required agent/outcome. Checklist hash is unchanged. Current git status shows only the plan/evidence directories added, with no tracked product-code changes. Documentary acceptance is separated from capabilities and subsequent supervision gates. |

Two review findings were resolved before this verdict: exact inventory provenance
was initially absent from the evidence package, and the Hermes MCP receipt was
only an opaque hash. The added provenance artifact/wrapper excerpt and redacted
response-shape artifact make those claims independently inspectable without
publishing private search results.

## Remaining capability gates

- Hermes named-profile **stream** completion/resume has not been accepted. The
  default-profile plain `-Q` contract was observed successfully; do not generalize
  across transports or every configured profile/server.
- Process-group death does not prove shared-gateway work stopped. The named
  stream viewer returned `-15`; scoped internal cleanup and a subsequent
  `running: false` snapshot were needed. That internal cleanup is evidence, not
  an approved public product dependency.
- Cancellation while an external tool is executing is unverified. The signal
  tasks requested tool-free output, but the Hermes timeout snapshot includes an
  `execute_code` call/result; it does not establish that a tool was active when
  signalled. Keep the unknown-outcome capability represented in Slice 3 fakes.
  Before accepting Slice 4 supervision, verify bounded harmless active-tool
  cancellation for the selected transport, session-specific settlement and no
  effect on another session, or verify an independently owned-worker mode.
- The measurements cover the installed versions/configurations and a deliberately
  nonexistent-model rejection; they do not establish every provider error,
  retry policy, MCP server or output mode. Reruns require appropriate local auth.

## Reviewed input fingerprints

| Input | SHA-256 |
|---|---|
| `docs/flow/agent-contracts.md` | `8e42cef7e1268862a5a189ecf7678e79ccee07c14d9a677cc31b3647f0c5ba9c` |
| `transcript.json` | `a214397619d31c7c1258b3e5c8bb722f96b8650d5630695a3421ec3418eb93f9` |
| `provenance.json` | `9ae02f07b3a399d4911b40cb749b0cb230ed2cb3d484391684973a92bb46be39` |
| `hermes-session-evidence.json` | `68c6bf2b37da3634453fc15e42e9faff2590c7cdca376c587456627e88b9aa3c` |
| `hermes-named-profile.json` | `723b33dc8e234d3814d1cd426c468d5128cf9d2cf1988e3ccb0a79ab656272a6` |
| `hermes-mcp-result-shape.json` | `58058eadafa2dcb54c418b48f8f1d9e358fe2105cdca62b6f9327ca43bdd1527` |
| Local plan, amended section 9 | `9aac179dddbd83ef6959f095754638fae0062b21833012849426495a67dd5372` |
