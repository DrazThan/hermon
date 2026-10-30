# Public Hermes ACP capability validation

Live verification covered the existing five role profiles on Hermes 0.22.4 and
0.22.5 through the public ACP v1 transport. Each profile completed an assigned
result-file task with positive, read-only main-call model provenance. The
operator's profile models, skills and configured MCP selection were preserved.

A two-session active-tool cancellation check verified that exact-session public
`session/cancel` returned the prompt's cancelled terminal response, stopped its
temporary heartbeat tool and left the unrelated control running. The control
was then settled through the same public mechanism. No shared gateway was
stopped. Viewer death alone remains an unknown outcome.

The integrated release doctor also verified five bot probes, tracker access and
code-host access. A version-pin mismatch was rejected before prompt submission;
the contract checks were repeated before updating the example pin. These
capability checks do not establish a live workflow, owner approval, product
acceptance, PR, merge or documentation outcome.

This public record intentionally excludes live session IDs, usage telemetry,
operator paths, authentication state, private issue details and knowledge-base
identifiers. Raw receipts and operator-specific probe scripts are retained
privately. `live-summary.json` contains the bounded publication summary.
Synthetic acceptance tests, fixtures and their RED/GREEN results remain public
under `../125/`. Their source assertions were not changed for publication.
