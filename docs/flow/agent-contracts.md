# Agent CLI contracts — Flow slice 0

Evidence for [#115](https://github.com/DrazThan/hermon/issues/115), against
[plan #114, sections 9 and 15](https://github.com/DrazThan/hermon/issues/114).
Probed on **2026-10-29**, macOS 26.6.2 / Darwin 25.6.0 arm64. No product code changed.

**Result:** all three installed CLIs support headless task-file input, session-ID
capture and explicit-ID resume with demonstrated history continuity. Skills and
MCP use were also observed. Exit status alone is insufficient: Codex returned
`0` after SIGTERM, and Claude returned `0` with an interrupted, erroneous result.
Hermes shared-gateway supervision needs the qualification below before Slice 3.

## Evidence and reproduction

| CLI | Resolved executable | Installed version |
|---|---|---|
| Hermes | `/Users/taloz/.local/bin/hermes` → `/Users/taloz/.hermes/hermes-agent/.hermes/bin/hermes` | `Hermes Agent v0.22.4 (2026.9.24) · stable · local 4d04303818b6` |
| Codex | `/Users/taloz/.local/share/mise/installs/node/26/bin/codex` | `codex-cli 0.160.0` |
| Claude | `/Users/taloz/.local/share/mise/installs/node/26/bin/claude` | `2.1.309 (Claude Code)` |

Hermes source HEAD was `4d04303818b61bf3aa09278af48356315515965b`, with no tracked
modifications. These are installed-version observations, not promises about other
releases, providers or configurations. Existing CLI authentication was reused.
Exact version/platform/source commands and outputs, plus resolved executable paths,
are preserved in [provenance.json](evidence/115/provenance.json). Hermes is a shell
wrapper; its target was read directly rather than resolved as a symbolic link.
The restricted workspace sandbox initially prevented Hermes acquiring its install
lock; probes ran with normal host access after approval review. That startup
failure is not a model error.

[transcript.json](evidence/115/transcript.json) contains 22 labelled probes:
exact argv, working directory, stdin, a snapshot of input text, stdout/stderr,
child return code, elapsed time and supervisor action. Claude thinking-only and
account rate-limit events are explicitly omitted from fixture transcripts.
Session metadata excerpts omit private Notion results and full skill bodies;
tool-result SHA-256 values identify the originals. No credentials are included.

The working directory was `/tmp/hermon-115`, resolved by macOS to
`/private/tmp/hermon-115`. Hermes received `/dev/null` as stdin; Codex and Claude
received regular files that reached EOF. No PTY was allocated. Each process was
started with `subprocess.Popen(..., cwd=..., start_new_session=True)`.

For a rerun, copy the harnesses and fixtures from [evidence/115](evidence/115/)
into `/tmp/hermon-115`. Use [probe.py](evidence/115/probe.py) for start, resume,
model-error and signal checks; [config_probe.py](evidence/115/config_probe.py)
for Codex/Claude skill and MCP checks. Install `fixture-SKILL.md` as both
`.agents/skills/flow115/SKILL.md` and `.claude/skills/flow115/SKILL.md` there.
The earlier `*-initial` and `*-race` inputs and outputs remain in the transcript;
the final harness uses the smaller signal task. Reruns need installed/authenticated
CLIs; timings and IDs will differ.

## Task-file input and resume

The first [task.md](evidence/115/task.md) asks the agent to remember
`FLOW115_7C82` and answer `FLOW115_OK`, without using tools. The
[followup.md](evidence/115/followup.md) asks for the previous nonce without
containing it. All six start/resume processes exited `0`; each continuation
returned `FLOW115_7C82` and retained its original session ID.

### Hermes

Exact successful commands, from the probe working directory:

```sh
hermes -p default chat -Q --query-file /tmp/hermon-115/task.md
hermes -p default chat -Q --query-file /tmp/hermon-115/followup.md --resume local-dddff2885ca92d131b1c1937463d6b22b26712206512dd89ea56178019ef636c
```

First stdout was `FLOW115_OK\n`; resume stdout was `FLOW115_7C82\n`.
Both stderr streams ended with:

```text
session_id: local-dddff2885ca92d131b1c1937463d6b22b26712206512dd89ea56178019ef636c
```

Resume also printed a human-readable `↻ Resumed session ...` line on stderr.
Capture the last line matching `^session_id: (\S+)$`, allowing CRLF. Keep stdout
and stderr separately. Treat IDs as opaque strings: this Hermes ID is not a UUID.
The installed help says `-Q` implies one-shot mode; the live runs exited without
interactive input. Gateway snapshots confirmed the recorded cwd.

### Codex

```sh
codex exec --ignore-user-config --ignore-rules --skip-git-repo-check --json -c 'approval_policy="never"' --sandbox read-only - < /tmp/hermon-115/task.md
codex exec --ignore-user-config --ignore-rules --skip-git-repo-check --json -c 'approval_policy="never"' resume 01a1889b-382e-7612-92b3-5a0746d3f7fd - < /tmp/hermon-115/followup.md
```

Both stdout streams included this exact identity event:

```json
{"type":"thread.started","thread_id":"01a1889b-382e-7612-92b3-5a0746d3f7fd"}
```

Each also contained an `item.completed` agent message with the expected reply
and a `turn.completed` event; stderr was empty. Parse `thread.started.thread_id`.
`exec resume ID -` accepts follow-up stdin. The start probe explicitly selected
read-only permissions; resume reused its saved session policy.

### Claude

```sh
claude -p --output-format json --safe-mode --tools '' --strict-mcp-config --permission-mode dontAsk --permission-prompts none < /tmp/hermon-115/task.md
claude -p --output-format json --safe-mode --tools '' --strict-mcp-config --permission-mode dontAsk --permission-prompts none --resume 8a8eba8c-e22f-4fdf-8108-bac7285c0f0d < /tmp/hermon-115/followup.md
```

The single stdout JSON result contained `session_id` equal to
`8a8eba8c-e22f-4fdf-8108-bac7285c0f0d` on both calls, the expected `result`,
`is_error: false`, and `terminal_reason: "completed"`; stderr was empty.
Use `--output-format stream-json --verbose` when early identity/tool events are
needed; its `system/init` and terminal `result` also carry `session_id`.

The configuration-bypass flags above isolate smoke tests. They are not production
presets for bots that need their configured skills/MCP. Neither CLI accepts
Hermes's `--query-file`. The shell examples use redirection; a supervisor should
open the task as child stdin directly. An argv-only template cannot express that.

## Skills, MCP and profile scope

| Probe | Observed evidence | Qualification |
|---|---|---|
| Hermes `-p default`, plain `-Q` | Loaded the profile's `notion` skill (`success: true`, heading `# Notion`), described `mcp__notion__notion_search`, and dispatched it through `tool_call` with the probe query and `page_size: 1`; returned `0` | Actual configured MCP invocation, not merely a model claim. Private search content omitted; the final reply reported success and one result. |
| Hermes `-p sdlc-docs`, `--format stream-json` | Snapshot identified `sdlc-docs`, its own skill inventory, and connected `notion`/`linear` servers. Its `skill_view` result succeeded and `tool_describe` returned the Notion search schema. | No completed MCP search before the 90-second deadline. This variant is not accepted as an end-to-end contract. |
| Codex controlled project | `$flow115` selected the `.agents/skills/flow115/SKILL.md` fixture; a read-only `cat` read it. JSON events show the `flow115/probe` MCP result. Both independent nonces were returned; exit `0`. | Explicit command-line MCP configuration; no user config loaded. This verifies the supplied fixture, not every user MCP server. |
| Claude controlled project | `system/init.skills` listed `flow115`; `mcp_servers` showed the fixture connected. `Read` loaded `.claude/skills/flow115/SKILL.md`, and `mcp__flow115__probe` returned its nonce; exit `0`. | Used `--setting-sources project`, `--tools Read`, explicit allowed tools, and `--strict-mcp-config --mcp-config ...`. User settings were excluded. |

The [profile task](evidence/115/profile-task.md) requested only skill reading and
a read-only search for `FLOW115_7C82_NONEXISTENT_PROBE`. Search can match unrelated
content; the test does not assume zero results. Actual call arguments, result
hashes, cwd and settled flags are in
[hermes-session-evidence.json](evidence/115/hermes-session-evidence.json).
The [decoded MCP response shape](evidence/115/hermes-mcp-result-shape.json)
independently shows a search `results` array; all page text and identifiers are
redacted. The tool response wrapped JSON inside a `result` string and an
`untrusted_tool_result` envelope, so it was not a top-level JSON status object.
[hermes-named-profile.json](evidence/115/hermes-named-profile.json) separates named
profile discovery, successful skill reading and incomplete MCP execution.

The Codex/Claude [fixture server](evidence/115/mcp_fixture.py) returns
`FLOW115_MCP_9A64`; the skill independently contains `FLOW115_SKILL_4D91`.
Exact configuration argv and tool outputs are the `codex-config` and
`claude-config` transcript entries. Codex's first fixture probe prohibited the
shell read needed for its skill: it completed MCP but reported a null skill nonce.
Claude's first fixture used empty settings sources: file reading worked, but
project skill discovery was disabled. Corrected probes establish both capabilities.

Hermes `-p` selects a Hermes profile. Codex has its own `-p` configuration overlay;
Claude's `-p` means print, not profile selection. The tests do not equate these
identity/configuration concepts or claim that all MCP servers are healthy.

## Exit outcomes

All 12 cells below are **observed**, with exact commands/outputs under
`<agent>-<case>` in the transcript. `success`, `model-error`, `interrupt`, and
`timeout` are case names accepted by `probe.py`.

| Outcome | Hermes plain `-Q` | Codex `exec --json` | Claude `-p --output-format json` |
|---|---|---|---|
| Success | `0`; reply and stderr session ID | `0`; `turn.completed` | `0`; `is_error: false`, completed result |
| Model/provider rejection | `1`; stdout `Provider said: HTTP 404: model: hermon-contract-model-does-not-exist-115` | `1`; `turn.failed`, provider error `400`, `model_not_found` | `1`; `is_error: true`, `api_error_status: 404`, `terminal_reason: "api_error"` **despite `subtype: "success"`** |
| SIGINT at 5 seconds | `130`; no answer, stderr session ID | `1`; `thread.started` and `turn.started`, no terminal completion | `0`; `is_error: true`, `terminal_reason: "aborted_streaming"`, `subtype: "error_during_execution"` |
| Supervisor deadline at 5 seconds → SIGTERM | `130`; no answer, stderr session ID | `0`; start events only, no `turn.completed` | `143`; empty stdout/stderr |

Model-error probes added `--model hermon-contract-model-does-not-exist-115` to
the corresponding start argv. These reached provider/model rejection; they were
not parser errors or missing credentials. Hermes retried three times before
reporting the 404. This does not measure every provider error or retry policy.

Signal probes requested a long, tool-free response. SIGINT tests for Hermes and
Codex used counting to 100000; timeout tests and the final Claude SIGINT test used
counting to 2000. Their exact input bytes are preserved per probe. These measure
termination during a started task, not cancellation of active external tools.
No completed model reply was received in those runs. An earlier Claude SIGINT
trial raced normal completion (`claude-interrupt-race`) and is not the interruption
proof.

The harness sent a signal **only to its newly owned process group** using
`os.killpg(child.pid, signal)`. A deadline sent SIGTERM, allowed 8 seconds, then
would send SIGKILL; no measured run needed SIGKILL. Native return codes above are
`Popen.returncode`, not the harness's own exit `0`. A signal-killed child has a
negative Python return code; for example the separate Hermes stream viewer was
`-15` (shell convention `143`). Claude's timeout return was positive `143`, not
Python `-15`. The harness invents no native timeout code such as `124`.

The supervisor must record `timed_out` before signalling and retain it even if the
child subsequently exits `0`. Require a successful terminal event/result as well
as an acceptable process status, then validate `result.json` and the step checks
from plan sections 6–8. A session ID is not completion evidence.

## Required section 9 amendment before Slice 3

Apply these qualifications to plan #114's Agent adapter contract; they are also
recorded in the local plan's section 9:

1. Keep the verified Hermes start/resume argv above, including profile selection.
   Parse the **stderr** session-ID trailer; persist it as an optional opaque ID.
   Retain separate stdout/stderr and explicit start/resume capture policies.
   Do not substitute the stream transport without separately accepting it.
2. Extend `command` configuration beyond `{task}`/`{workdir}`/`{session}` argv
   replacement: it needs task-file stdin, distinct start/resume argv, and
   agent-specific session/completion parsers. Invoke argv directly. If a wrapper
   script is configured, make that explicit; shell redirection is not an argv item.
3. The hub owns deadline/cancellation state. A zero child exit does not override
   it or a failed/missing completion event. Claude's `subtype` alone is also
   insufficient. Invalid/missing step results remain blocks, never success.
4. **Process ownership gap:** this Hermes installation delegates work to a shared
   gateway. The named profile's stream viewer exited on SIGTERM while its session
   still needed a scoped `session.interrupt`. An installed internal client sent
   that interrupt using the probe's exact ID and execution generation; a later
   snapshot confirmed `running: false`. No shared gateway was stopped. Plain
   `-Q` SIGINT/SIGTERM probes did settle their sessions, confirmed by snapshots.
   Thus process-group death alone is not universal proof of Hermes work stopping.
   Before accepting Slice 4 supervision, verify an available, stable, scoped
   cancellation mechanism for the selected transport during active tools, or use
   an independently verified owned-worker mode. The internal RPC cleanup is
   evidence, not a new supported product dependency. Keep unsupported modes gated.
5. If session-ID capture or resume fails for a configured CLI/version, start a
   fresh session with prior task/context and owner answers inlined, as section 15
   proposes. Do not silently use `--last`, guess a session, or resend work whose
   shared-gateway outcome is unknown; reconcile it first.

Capability completeness: the required task-file, ID, continuity, skills/MCP and
exit observations are present for the tested configurations. Unverified extensions
are named-profile stream completion/resume and cancellation while an external
tool is executing. Minimum follow-up: rerun that profile through the selected
transport with one bounded harmless tool, interrupt its exact session, and prove
both viewer exit and gateway settlement without affecting another session. This
gate must be represented in Slice 3's fakes before Slice 4 can depend on it.

## Source checks and workflow

Installed Hermes source inspection supports, but does not replace, the live
results: `hermes_cli/cli_single_query.py:188` routes quiet output/session IDs;
`:402` installs quiet SIGINT/SIGTERM handlers; `hermes_cli/turn_exit.py:17` maps
turn outcomes; `hermes_cli/gateway_client.py:1` explicitly states that closing a
viewer never stops its authority. These paths refer to the Hermes source revision
above, not this repository. Stream mode's initial JSON ID was empty and its stderr
used `Session: ...`; do not treat an initial empty ID as a usable session.

Official references are consistent with the supported input/session mechanisms:
[OpenAI non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode),
[OpenAI CLI reference](https://learn.chatgpt.com/docs/developer-commands?surface=cli),
and [Claude headless usage](https://code.claude.com/docs/en/headless).
The exit matrix is local evidence, not inferred from those pages.

An independent test writer froze [acceptance.md](evidence/115/acceptance.md) before
the evidence document was written, SHA-256
`dc5cb5c4cc27042242c0baa55983e8056998e5352b3060e147fe2279a7794f88`.
Independent validation is recorded separately in `evidence/115/validation.md`.
This is a documentation spike; no fabricated behavioral RED/GREEN or product CI
claim is made. No product tests or adapters were changed.
