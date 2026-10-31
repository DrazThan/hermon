# Additional recovery binding checks

Source review of the initial working implementation found that session-open
identity fallback could accept a missing sessionId on a new session. The extra
black-box packet reproduces that case and requires refusal of a foreign open
session, a resume identity that differs from the launch, and an owned-attempt
path redirected through a symlink. These are failure-recovery boundaries, not
permission to accept any candidate result.

The scope test was run against the initial working implementation, before its
binding hardening: it failed because missing-open-session was accepted. This
supplement and its RED evidence were frozen separately before those guards.
The original five-test packet and every earlier frozen source remain unchanged.
