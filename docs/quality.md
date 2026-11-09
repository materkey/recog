# Quality gate

The gate compares the working tree (including new, nonignored files) with
`git merge-base HEAD <target-ref>`. It measures both revisions with fresh coverage
from the **same toolchain, Python interpreter and dependency environment**: the
Rust workspace's tests under `cargo llvm-cov`, and the JavaScript suites under
jest and c8. Then it runs the pytest suite once, on the head: there is no
production Python left to measure, so pytest (the shell scripts' and the tooling's
tests) only passes or fails. Both revisions run in disposable source worktrees;
ignored installation config and personal data are absent from both. The head
snapshot includes tracked modifications, deletions and new nonignored files
without changing the root index. It does not accept cached coverage or a committed
numeric baseline.

Install the test/runtime dependencies first. Quality tools are separate from the
runtime setup and require Python 3.12 or newer:

```bash
# Into the interpreter scripts/check.sh uses (RECOG_CHECK_PYTHON, else python3),
# with PyYAML for the workflow's tests, as in CI.
python3 -m pip install -r deploy/python/requirements-quality.txt pyyaml
# ESLint and c8 with monocart for the JavaScript part (Node 22.13 or newer), both systems,
# and the extension's jest for its coverage:
npm ci --prefix deploy/node
npm ci --prefix browser-extension
# cargo llvm-cov for the Rust coverage (CI pins 0.9.1), and the LLVM tools it drives.
# A toolchain without rustup (a distribution's rustc): set LLVM_COV and LLVM_PROFDATA
# to the llvm-cov and llvm-profdata of the LLVM version that rustc was built with.
cargo install cargo-llvm-cov --version 0.9.1 --locked
rustup component add llvm-tools-preview
python3 scripts/quality.py --base origin/main --output .local/quality
# The same gate as part of the regular checks (what CI runs):
RECOG_QUALITY_BASE=origin/main ./scripts/check.sh
# Only the Ruff lint and format check of changed lines, in seconds:
python3 scripts/quality.py --style-only --base origin/main
```

A plain `./scripts/check.sh` runs that `--style-only` check against the local
`origin/main` before its single pytest run. The target ref is used as last
fetched; a stale ref only widens the compared range. A missing ref,
failed baseline or head test suite, missing tool/version mismatch, invalid JSON or
lcov, a crate without any measured file, a Rust file lizard reads differently from
the syntax tree, nonfinite metrics, or a changing working tree fails the run.
Results go into a new `run-*` directory under the selected output root:
`report.json`, `report.md`, the baseline/head lcov exports, coverage JSON and test
logs. The temporary worktrees are removed after the run. CI publishes only those
reports and logs, never the temporary source tree. The normal test suite runs
policy tests without the optional quality tools; real analyzer integration tests
require them (the Rust ones also `cargo llvm-cov`, on a one-function crate).

Both revisions build into one target directory, `CARGO_TARGET_DIR` (relative to
the checkout) or else `target`, under its `llvm-cov-target/`: the dependencies are
compiled once, and only the workspace's own crates again for each tree. On a
32-core Linux machine with the dependencies already built, the whole gate took
about 7.5 minutes, most of it the two instrumented builds and test runs; the first
run also builds the dependencies with instrumentation.

## Policy

- A new function fails when CRAP is **greater than 10**.
- An existing function fails when its new CRAP is **greater than 10** and its
  increase is **greater than 1**. Exactly 10 or exactly +1 is allowed. Smaller
  increases remain visible in the report.
- New Ruff lint findings fail. Existing findings survive only on unchanged
  source lines with the same diagnostic, column and rule; moving a line does not
  erase its history. Formatting changes are required on changed/new lines;
  unrelated existing formatting debt is preserved. Tests and quality scripts
  participate in this check too. Nothing is automatically reformatted by the gate.
- New ESLint findings in JavaScript (`*.js`, `*.cjs`, `*.mjs`) fail the same
  way: `eslint:recommended` from `scripts/quality-eslint.config.mjs`, compared
  against the merge base line by line like Ruff. There is no JavaScript
  formatter check. Generated bundles are excluded.
- A file that git reads as moved since the merge base (`git diff -M`: renamed, at
  least half of it unchanged) is compared with itself under its former name: its
  functions and duplicate pairs are the baseline's, not new ones, and its existing
  lint findings and formatting stay where they were. The fast style check sees the
  moves git already tracks (`git mv`, `git add`).
- Newly detected duplicate function pairs, Rust or JavaScript, fail until resolved
  or explicitly reviewed in `scripts/quality-exceptions.json`. An exception names
  exactly two canonical function identities and explains why keeping the
  duplication is better than extracting it. The initial exception list is empty.
  Existing pairs remain visible in the JSON report; this adoption gate does not
  demand a repository-wide duplication cleanup.
- SCB verbosity, erosion, cognitive erosion, source LOC, per-function cognitive
  complexity and LOC are informational. They are not independent pass thresholds.

Structural analysis covers production Rust and production JavaScript.

**Rust** is every `crates/<crate>/src/**/*.rs` (binaries under `src/bin/` included)
without its test code; a crate's `tests/`, `benches/` and build script are outside
`src/` and never read (`src/live/build.rs` of recog-live is a module, not a build
script, and is measured). Test code is found in the syntax tree
(`scripts/quality_rust.py`, tree-sitter-rust), not by file or module name: an item
carrying `#[test]`, `#[<runner>::test]` or a `#[cfg(…)]` that is false without
`test` (`cfg(test)`, `cfg(all(test, …))`; not `cfg(any(test, …))` or
`cfg(not(test))`) is blanked out of the source with its attributes, every other
byte and line kept in place, and a file that is a module declared under such an
attribute (`#[cfg(test)] mod tests;`, resolved like rustc does from the declaring
file), a module of such a file, or a file opening with `#![cfg(test)]` is left
out whole. A test module whose file cannot be found stops the run. Function
signatures without a body (trait methods, extern fns) are blanked too.

Coverage comes from `cargo llvm-cov` over the workspace's tests, as check.sh runs
them: `--workspace` except speexdsp-aec, which runs only `--lib --test properties
--test cli --test adapted_golden` because its parity tests need golden data CI does
not have; doctests are not instrumented on stable. Stable rustc records lines, not
branches, so a Rust
function's coverage is that of the lines of the lcov export inside its span that
belong to no function nested in it (branch hits are used when a toolchain records
them). A function without a single line in the export was not compiled on this
host (`#[cfg(target_os = "macos")]` on Linux): it is listed in the report as not
built and not scored. CI runs the gate on Linux only (on the macOS runner it would
double a 10-minute job and its disk use, docs/ci-runners.md), so macOS-only code is not scored: keep its
untested shells at CC ≤ 2 and its decisions in portable, tested functions. Every crate must have at
least one measured file, which also catches a report whose paths do not join.

lizard 1.24.0 misreads Rust in two ways the gate works around before handing it a
file: it reads `'_'` or `'a'` as a lifetime followed by an opening quote that
swallows code up to the next quote, and a `#` as a C preprocessor line, so a
multi-line `r#"…"#` leaks its text as code; and it does not end a function at
`;`, so a bodiless trait method takes the body of the next function. Each moved
or merged functions. lizard reads a copy whose char literals hold only dots and
whose raw strings are plain strings of spaces, lines unchanged, and the bodiless
signatures are blanked; its functions must then match the syntax tree's `fn`
items by name and last line (lizard alone may add the functions of a
`macro_rules!` template), or the run stops.

**JavaScript** is the web UI (`crates/recog-web/static/`), the desktop bootstrap
script (`crates/recog-desktop/bootstrap.js`) and what the extension loads
(`browser-extension/src/`, `content.js`, `popup.js`). Tests, scripts, build
configuration, generated bundles and ignored files are not included. The
extension's coverage comes from its jest suite (`--coverageProvider=babel`); the
web UI's from the `node:test` suites `tests/*.test.cjs` under c8, which see a
script only when it is evaluated with its path as the `vm` filename. c8 runs with
monocart, which takes the functions from the source's syntax tree: raw V8 coverage
omits a function that was never compiled, such as a registered handler no test
fires. A JavaScript file no suite loads still gets its functions, at 0%. Functions
written on the same line as the function enclosing them share its line span, which
crapkit refuses to join to coverage; start such a callback on its own line.

Ruff applies to changed/new nonignored Python anywhere in the repository (the
scripts and tests), ESLint to changed/new nonignored JavaScript.

## Shell, workflow and secret checks

`scripts/check.sh` also runs three whole-repository checks with no baseline:
any finding fails, and an accepted one carries an inline directive with its reason.

- **shellcheck** over every nonignored `*.sh`, following `source` from each
  script's directory (`-x --source-path=SCRIPTDIR`).
- **actionlint** over `.github/workflows`, with the same shellcheck for `run:` scripts.
- **gitleaks** over the committed history and over a copy of the files a commit
  would take (tracked and new, not ignored), with the default rules and the
  reviewed false positives in `scripts/quality-gitleaks.toml`.

shellcheck and actionlint are pinned in `deploy/python/requirements-quality.txt`.
gitleaks has no pip distribution: CI downloads release 8.30.1 and checks its
sha256; locally install it from a package manager (`brew install gitleaks`).

## Rust checks

With the Rust workspace present (`Cargo.toml` at the root), `scripts/check.sh` runs
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and the
echo canceller's tests that need no data, then every crate's tests. The parity tests that
ran the deleted Python modules beside their ports went with them.
With `SPEEXDSP_AEC_GOLDEN` set it also runs
the parity tests against pyaec (`crates/speexdsp-aec/README.md`). cargo comes from
rustup (https://rustup.rs) with the clippy and rustfmt components; CI uses the
runner's stable toolchain, so a new clippy lint can fail a run that passed before.

CRAP calculation, coverage joins, function identity and duplicate detection come
from pinned **crapkit 0.7.3**, not a local complexity implementation. It uses the
smaller standard/modified cyclomatic complexity (with its corrected Rust reader,
which counts `match` arms) and per-function branch coverage (statement coverage for
a function without branches; for JavaScript, from the Istanbul report; for Rust,
the lines above). Same-named methods use crapkit's canonical ordinal identities;
signature changes are conservatively new functions. Duplicate candidates use its
default minimum eight normalized lines and 0.8 containment; all pairs are retained,
not just the first 50. The gate owns only comparison policy, orchestration and
report formatting, the Rust inventory and line attribution above, and one
correction of lizard 1.24.0 for JavaScript (`scripts/lizard_template_literals.py`):
lizard ends a template literal at the next backtick, so one nested in another's
`${…}` made the enclosing function absorb every function after it. The gate
replaces that match with a scanner that finds the literal's real end and yields the
same tokens otherwise; it refuses to run with another lizard version.

SCB uses **scb-check 0.2.0**, including ast-grep, on a copy of the production Rust
(test code blanked) outside any repository, since SCB skips files a `.gitignore`
above them names. Its slop rules are Python's, so for Rust its verbosity counts
only clones. Exit 1 means valid findings and is accepted for its informational
metrics; exit 2 or malformed/incomplete output is an error. Its file count must
match the production Rust inventory.

Tool versions (with `rustc` and `cargo-llvm-cov`) and the working-tree fingerprint
are recorded in each report. Both revisions use the current pinned quality
configuration and dependencies, so results compare code changes rather than
analyzer versions. A baseline that cannot run in that environment is an explicit
measurement failure requiring investigation, never an implicit zero baseline.

Official tool contracts:
[crapkit source](https://github.com/JeanFrancoisGagne/crapkit/tree/v0.7.3),
[cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov),
[scb-check](https://github.com/gabeorlanski/scb-check),
[Ruff](https://docs.astral.sh/ruff/),
[ESLint](https://eslint.org/docs/latest/),
[ShellCheck](https://www.shellcheck.net/wiki/),
[actionlint](https://github.com/rhysd/actionlint/blob/main/docs/checks.md),
[gitleaks](https://github.com/gitleaks/gitleaks).
