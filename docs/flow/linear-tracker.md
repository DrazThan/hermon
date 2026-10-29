# Linear tracker and comment templates

Slice 6 binds tracker effects to a central Linear OAuth app. `LinearTracker`
accepts a `LinearConfig`, the templates selected at intake, and a durable state
directory owned by the hub, such as `<run>/tracker/`. This directory must be
separate from the hub's own `RunStore` directory. The host must serialize access to it,
just as it holds the run lock while advancing the hub.

The token belongs in the host's config or keychain. Supply an app-actor OAuth
client-credentials access token for the central app (for example Hermes SDLC),
with permission to read issues/comments and create comments. The adapter sends
one `Authorization: Bearer` identity for every request and never supplies
`createAsUser`, actor, or avatar overrides. It does not provision credentials or
copy them to task files, run snapshots, or request records. Token acquisition
and renewal remain responsibilities of the host configuration layer.

Owner IDs are exact Linear user IDs. The trusted owner mention is a Linear
profile link from host configuration. Linear recognizes profile URLs as
mentions; bot-supplied values must never supply that trusted mention field.

## Templates and preview

The seven comment kinds are `started.md`, `question.md`, `go_request.md`,
`answer_ack.md`, `step_done.md`, `alert.md`, and `outcome.md`. Selection remains
project override, workflow pack, then compiled defaults. Invalid selected
templates fail validation rather than falling through. Question and GO request
templates require `{{owner_mention}}`.

Preview with synthetic values without contacting Linear:

```sh
hermon templates preview workflows/sdlc/workflow.yaml question.md
hermon templates preview workflows/sdlc go_request.md --project-templates projects/example/templates
```

The optional project directory is explicit until project configuration loading
arrives in a later slice. A workflow's `templates` path is resolved relative to
its workflow file. Templates use one-pass `{{name}}` substitution without logic;
values cannot introduce new substitutions. The renderer escapes bot markdown,
mentions, links, and HTML marker syntax. It appends a validated run/request
marker independently of template contents.

## Replies and restart

Owner GO is a literal `GO` body from a configured owner ID in the exact parent
comment thread for the run, step, and request generation. Whitespace, case
variants, prose, quoted GO, other users, and other threads cannot approve it.
This path makes no coordinator/model call.

Owner questions retain the settled attempt handle in the durable hub state.
An authorized answer resumes that attempt through the agent adapter. The
process adapter uses its recorded explicit session ID when the configured
command supports resume; otherwise it starts with prior task context and the
answer. Adapters must implement owner-answer delivery; the trait default
returns a capability block before launching anything. An unresolved worker cannot be resumed. The answer acknowledgment is
also rendered and posted by the central app.

Reply polling persists its progress and matching result together. Relay cursors
are pagination positions, not permanent subscription offsets: after an
exhausted page, polling must restart at a time watermark so a newer comment is
not skipped. Inclusive timestamp boundaries preserve replies with the same
timestamp. A pending page and its watermark survive restart.

## Reference and validation

Linear's primary references:

- [GraphQL endpoint, OAuth header, and profile URL mentions](https://linear.app/developers/graphql)
- [App actor authorization](https://linear.app/developers/oauth-actor-authorization)
- [Client-credentials tokens](https://linear.app/developers/oauth-2-0-authentication)
- [Relay pagination](https://linear.app/developers/pagination)
- [Filtering](https://linear.app/developers/filtering)
- [Public GraphQL schema](https://github.com/linear/linear/blob/master/packages/sdk/src/schema.graphql)

Acceptance uses a local HTTP fake, with synthetic tokens, issues, comments,
threads, and agent processes. The owner explicitly requested skipping live
posting on 2026-10-29. Actual Linear delivery, persistence of the hidden marker
through Linear's markdown renderer, and readback as the central app actor remain
unverified. No live tracker comment was posted.
