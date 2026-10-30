# Complete notifier fixture capture

macOS CI job 124167320272 at head
`d42fba837d70d7d6a86e532327fc2a0e519063e6` failed the existing terminal-notifier
argument assertion at notify.rs:290. `ci-failure.txt` contains the relevant GitHub
job-log excerpt with trailing whitespace normalized.

The shim appended one argument at a time. The reader returned as soon as any
content existed, so the assertion could see only the first part of argv. The
negative sanitization assertions had the same incomplete-evidence risk.

The new regression pauses the shim after its first argument with a stdin-pipe
handshake. Before the fix, that partial capture was already visible and the
regression failed deterministically in 0.15 seconds (`red.txt`). The exact RED
source is `before-publication-fix.rs`, recorded by `regression-red.sha256`.

The shim now writes to a sibling temporary file, then atomically renames the
completed capture to the reader's path. It uses printf to preserve each literal
argument. The checkpoint hook is only used by the regression; ordinary notifier
fixtures do not pause. The regression confirms that no partial file is visible,
releases the writer and reads the complete expected capture.

All 15 original test functions and read_eventually are byte-for-byte unchanged.
The reader still permits 200 attempts at 10 ms intervals. The production prefix
of notify.rs is also byte-for-byte unchanged, including asynchronous delivery.
`verification.txt` records those comparisons. The original test module is archived under `original/`; the production-prefix
hash is retained in `verification.txt`.

All 16 focused notifier tests pass after the correction (`green.txt`). Targeted
library Clippy passes with warnings denied (`clippy.txt`). These checks use only
local shim executables and produce no desktop banners. Product behavior and
pilot configuration were not changed.
