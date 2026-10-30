# Establish behavioral RED

Read the approved plan and test brief. Write independent acceptance tests and necessary fixtures only inside the project's test path set. Do not modify production code to make the tests fail. Use the exact planned new test IDs; a conflict with the approved contract is scope_change, not permission to change the expectation artifact.

Execute lint and the configured test command. Prove each expected new test fails for the missing behavior. Syntax errors, missing dependencies, test collection failures, permission errors and command failures do not establish RED. Check that known baseline failures do not masquerade as new acceptance failures. Preserve real command output and commit tests before reporting done. The hub reruns checks and records the RED tag and frozen paths; a self-reported failure is insufficient.

Do not implement the feature or start GREEN. Questions use owner_question; missing tools or credentials use capability. Follow task.md for result.json.
