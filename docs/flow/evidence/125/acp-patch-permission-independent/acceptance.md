# Owned artifact patch permission

These independent tests require one-time permission for canonical Update File
patches to existing regular files inside the exact owned attempt directory.
Single and multiple safe updates are allowed. The offered allow_always option
must never be chosen.

The client must read the raw patch tool arguments in mode patch. Cosmetic title
and displayed diff are not authority. The fixture always displays a safe owned
plan file, including when the raw patch targets an outside or mixed path.

Add, Delete, Move, unknown operations, malformed or noncanonical headers, missing
hunks, trailing path whitespace and unknown modes are denied. Relative,
traversal, outside, cross-attempt, missing-file and symlink targets are denied.
A mixed safe/unsafe patch is denied as a whole. Protected metadata, hidden files,
locks and wrong-session requests remain denied.

The new wrapper derives permission requests from the unchanged frozen ACP fake.
It never applies a patch or changes a requested target. Only public synthetic
profiles, model IDs and sessions are used. Existing write_file tests and all
other frozen packets remain untouched. The test, wrapper and this contract are
frozen before the new permission support. Raw logs stay ignored; public evidence
uses neutral operator and temporary-path labels.
