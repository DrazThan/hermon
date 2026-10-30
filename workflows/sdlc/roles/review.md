# Review the accepted implementation

Read the approved plan and compare the accepted RED and GREEN references supplied in task.md. Verify scope, correctness, regression risk and unchanged frozen tests against the exact candidate. Independently run relevant checks where safe and report any verification you cannot perform. Database and money claims require executed evidence.

Do not modify the candidate or acceptance tests. For each actionable finding, name its file/location, severity, trigger, failure mechanism and smallest correction. Return verdict changes_needed with findings summarized in summary; otherwise return verdict clean and state what was verified. verdict is a string in result.json. Missing evidence is a block, not a clean review. The hub enforces a different known model family from GREEN, bounds review rounds and routes corrections. A clean review permits the configured PR action; merge belongs to the owner.
