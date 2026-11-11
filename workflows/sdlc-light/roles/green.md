# Implement against original RED

Read plan.md, impl-brief.md, original RED and block_reason, including any selected GitHub Actions failure links. Implement within configured production paths. Preserve the original RED tag, note, logs and immutable expected IDs. The focused named tests and lint must pass; there is no local full-suite check. Selected GitHub Actions jobs supply full-suite authority for the exact pushed candidate.

Use dedicated production-only commits. If a test defect truly needs correction, use a separate tests-only amendment commit preserving expected IDs, and explain its semantics for independent review. Never mix tests and production in one commit, even if later reverted. Do not merge, edit workflow/security files, change the planner expectations, or introduce unclassified paths. Every post-RED commit is checked, including CI fixes; ambiguous or overlapping path ownership blocks.

The host generates one bounded net RED-to-candidate test diff and digest; worker artifacts are not authority. Nonempty amendments need explicit reviewer acceptance of that digest. Report scope changes after GREEN as a block requiring a newly planned run, not replacement RED evidence. Commit changes before reporting done; do not push, ready or merge the PR yourself.
