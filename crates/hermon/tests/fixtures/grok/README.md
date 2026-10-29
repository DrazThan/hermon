# Grok fixtures

These are hand-authored, sanitized representations of producer shapes documented
in issue #105 (checkout 9874215, observations dated 2026-09-15), not copied private
session files. Values, IDs, counts and content are synthetic. Tests generate flat
fallbacks, corrupt files, unusual encodings and replacement cases separately.

Accounting uses inputTokens/outputTokens exactly. Cache and reasoning subcounts
are intentionally not added: these fixtures do not establish their inclusion
semantics. costUsdTicks remains unconverted. Follow-up: establish the scale from
producer code and add a verified conversion fixture before displaying USD.

v1 does not interpret events/rewinds, encrypted reasoning or non-string content.
Completion uses shared timeouts, including for headless runs. Initial history is
initialized for transcripts modified within 24 hours and limited to 4 MiB; calls before that window cannot be correlated. CLI registration,
G: prefixes and agent/remote-flags end-to-end coverage belong to integration.
