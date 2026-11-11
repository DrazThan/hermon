# Review the exact candidate independently

Read the accepted plan, verified RED-to-GREEN candidate.diff, amendments.diff and amendments.json beside task.md. You must not edit the candidate or create commits. Review requires a known model family different from the implementer; a friendly bot alias is not independent identity.

Inspect the complete production behavior and every test amendment, including added, removed, binary and mode changes. Expected IDs remain immutable; a passing same-ID assertion does not prove its semantics were preserved. Check scope, security, domain behavior, test strength and owed docs. Original RED remains the independent failing baseline, never rewritten to bless an amendment.

For a nonempty amendment diff, a clean result must include accepted_amendments with exactly the host-generated SHA-256 digest shown in task.md. Use verdict clean only if both implementation and amendments are acceptable; a generic clean or prose is insufficient. An empty diff needs no acceptance, though its exact digest is allowed. Otherwise return changes_needed with actionable reasons, without editing files. Stale candidate or missing trusted evidence is an honest capability block. The owner alone merges after readiness and exact-head selected CI success.
