# hermon v0.1.0 — release and Homebrew tap

Everything needed to cut the first release and publish the personal tap. The
files in this repo (release profile, `.github/workflows/release.yml`,
`packaging/homebrew/Formula/hermon.rb`) are ready; the steps below are the
parts that need push/tag/repo-create rights and so have **not** been run.

## 1. Release profile: measured before/after

Added to `Cargo.toml`:

```toml
[profile.release]
lto = "fat"
codegen-units = 1
strip = "symbols"
```

Measured on this machine — macOS 26.6.2, arm64 (Apple silicon),
`rustc 1.98.0 (88d9e12ae 2026-08-18)` — with a clean `rm -rf target/release`
before each build, `cargo build --release --locked`:

| | Baseline (no `[profile.release]`) | Tuned | Change |
|---|---|---|---|
| Binary size | 5,834,592 B (5.56 MiB) | 4,270,480 B (4.07 MiB) | **−1,564,112 B, −26.8 %** |
| Clean build time | 15.8 s | 28.8 s | +13.0 s (+82 %) |

Startup, 150 interleaved runs per binary (alternating baseline/tuned each
round so thermal drift hits both equally), timed from `subprocess.run` around
the whole process:

| Command | Baseline median | Tuned median | Baseline p90 | Tuned p90 |
|---|---|---|---|---|
| `hermon --version` | 2.68 ms | 2.64 ms | 3.03 ms | 2.83 ms |
| `hermon ls` (empty fixture stores) | 2.78 ms | 2.73 ms | 3.04 ms | 2.97 ms |

The `ls` figure points every source at paths with nothing behind them
(`--claude-dir` at an empty dir, the two DBs and the log at nonexistent
paths), so it measures process start + config + store-open + an empty scan
without depending on whatever sessions happen to be live.

Read that as: **size down ~27 %, startup unchanged.** The startup deltas
(~0.04 ms, ~1.5 %) are inside run-to-run noise — a first, non-interleaved
pass had the tuned binary looking *slower* by 0.5 ms, which is what motivated
re-running interleaved. Nothing here is startup-bound; the win is the smaller
download and a smaller resident binary. The cost is build time, which a
source-building Homebrew formula pays once per install.

Reproduce with:

```bash
rm -rf target/release && cargo build --release --locked && stat -f%z target/release/hermon
```

## 2. Tag and release (operator)

From a clean checkout of `main` at the commit that carries these files:

```bash
git checkout main && git pull --ff-only
git tag -a v0.1.0 -m "hermon v0.1.0"
git push origin v0.1.0
```

Pushing the tag fires `.github/workflows/release.yml`, which runs the full
suite (build, test, clippy, fmt, plus a `cargo build --release` so the LTO
profile is proven on Linux too), checks
the tag matches `Cargo.toml`'s version, and then creates the GitHub release
with generated notes. GitHub attaches the source tarball automatically — no
binary artifacts are uploaded, since the tap builds from source.

Watch it and confirm the release exists:

```bash
gh run watch --repo DrazThan/hermon "$(gh run list --repo DrazThan/hermon --workflow Release --limit 1 --json databaseId --jq '.[0].databaseId')"
gh release view v0.1.0 --repo DrazThan/hermon
```

If CI is red, delete the tag (`git push --delete origin v0.1.0 && git tag -d
v0.1.0`), fix, and re-tag — the release job only runs after the test job
passes, so a red run leaves no release behind.

## 3. Fill in the formula's sha256

`packaging/homebrew/Formula/hermon.rb` ships with an all-zeros placeholder:

```ruby
sha256 "0000000000000000000000000000000000000000000000000000000000000000"
```

Once the tag exists, get the real digest of GitHub's source tarball:

```bash
curl -fsSL https://github.com/DrazThan/hermon/archive/refs/tags/v0.1.0.tar.gz | shasum -a 256
```

That prints `<digest>  -`; paste `<digest>` over the zeros. (Homebrew's
`brew fetch --formula ./Formula/hermon.rb` won't help until the digest is
right — it verifies against it.)

## 4. Create the tap repo (operator)

The tap is a plain GitHub repo named `homebrew-hermon`; Homebrew maps
`drazthan/hermon` to `github.com/DrazThan/homebrew-hermon`.

```bash
cd "$(mktemp -d)"
gh repo create DrazThan/homebrew-hermon --public --description "Homebrew tap for hermon" --clone
cd homebrew-hermon
mkdir -p Formula
cp /path/to/hermon/packaging/homebrew/Formula/hermon.rb Formula/hermon.rb
# make sure the sha256 from step 3 is in place before committing
git add Formula/hermon.rb
git commit -m "hermon 0.1.0"
git push
```

`packaging/homebrew/Formula/hermon.rb` in this repo stays the source of
truth; the tap repo gets a copy. On the next release, bump `url`, `sha256`
and re-copy.

## 5. Verify the tap on a clean machine

```bash
brew tap drazthan/hermon
brew install hermon
hermon --version    # expect: hermon 0.1.0
hermon ls
```

Paste that transcript into issue #46. If a stale copy is already installed,
`brew uninstall hermon && brew untap drazthan/hermon` first; add
`--verbose --build-from-source` to `brew install` when a build fails and you
need the cargo output.

## 6. Audit status

Run against the formula in a throwaway local tap, since `brew audit` refuses
a bare file path (`Calling brew audit [path ...] is disabled`):

```bash
brew tap-new hermonaudit/scratch --no-git
cp packaging/homebrew/Formula/hermon.rb "$(brew --repo hermonaudit/scratch)/Formula/hermon.rb"
brew audit --strict hermonaudit/scratch/hermon
brew style hermonaudit/scratch/hermon
brew untap hermonaudit/scratch
```

Results here, Homebrew 6.0.20:

- `brew audit --strict` — **passes**, no output, exit 0.
- `brew style` — **passes**, "1 file inspected, no offenses detected".
- `brew audit --strict --online` — **fails on one check only**, and expectedly:
  `Stable: The source URL https://github.com/DrazThan/hermon/archive/refs/tags/v0.1.0.tar.gz is not reachable (HTTP status code 404)`,
  because the tag doesn't exist yet. Re-run `--online` after step 2; it should
  come back clean once the digest from step 3 is in place.

## 7. Predecessor: the Python implementation

hermon began as `hermon.py`, a single-file Python script at the repo root —
the same read-only model, but driving real tmux panes instead of a built-in
TUI, and carrying its own `unittest` suite under `tests/`. It stayed in the
tree as the parity oracle for the whole rewrite and was deleted in
[#47](https://github.com/DrazThan/hermon/issues/47) once `hermon ls` and
`hermon render` were signed off row-for-row against it on all three sources.

It is one checkout away:

```bash
git show python-final:hermon.py
git checkout python-final    # the whole pre-deletion tree, tests included
```

The `python-final` tag marks the last commit that carries it. Comments under
`src/` and `tests/` still cite `hermon.py` and `tests/test_*.py` line numbers
as provenance for the behavior and cases they port; those references resolve
against that tag.

## 8. What's deliberately not here

No signed or notarized binaries, no bottles, no homebrew-core submission, no
Linux packages. Every `brew install hermon` compiles from source against the
`rust` build dependency. That's the right trade until there are enough users
for a cold install's ~30 s of LTO to matter.
