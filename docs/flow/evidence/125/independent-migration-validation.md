# Independent local migration readback

The independent reviewer read the five versioned domain-only skill sources,
their installed profile targets and original backups. SHA-256 was recomputed
from each actual file. Every source and installed target equals the matching
`after_sha256` in `profile-migration.json`; every backup equals its recorded
`before_sha256`.

The installed `sdlc-reliability` tree was independently enumerated in sorted
path order, excluding `__pycache__`, using the receipt generation algorithm:
SHA-256 of JSON mapping relative file paths to content SHA-256, with sorted keys.
Its current aggregate is
`eb90a6a16c121f3022dc94cb4904e95a6b1a73a0860b432be0fb2832c0f205cc`,
matching both recorded before and after hashes. It remains installed.

The reviewer also read the live local cron registry. Job `dcc9fbf66967`,
`SDLC public controller`, has `enabled: false`, `state: paused`, and the paused
readback matches every field recorded in the receipt. No profile, plugin or cron
file was modified during this review.

The five new skill bodies carry domain guidance and omit the retired
release/checkpoint/publication protocol. Their step/file-contract instructions
are versioned separately in the SDLC role prompts. The live pilot remains
deferred by the owner; these readbacks do not establish a pilot, a completed
owner gate, a merged PR, settled historical work or verified Hermes cancellation.
