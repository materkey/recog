# Python quality gate

The gate compares the working tree (including new, nonignored Python files) with
`git merge-base HEAD <target-ref>`. It runs both non-e2e test suites with fresh
branch coverage in the **same Python interpreter and dependency environment**.
Both revisions run in disposable source worktrees; ignored installation config
and personal data are absent from both. The head snapshot includes tracked
modifications, deletions and new nonignored files without changing the root index.
It does not accept cached coverage or a committed numeric baseline.

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
python3 scripts/quality.py --base origin/main --output .local/quality
# The same gate as part of the regular checks (what CI runs):
RECOG_QUALITY_BASE=origin/main ./scripts/check.sh
# Only the Ruff lint and format check of changed lines, in seconds:
python3 scripts/quality.py --style-only --base origin/main
```

A plain `./scripts/check.sh` runs that `--style-only` check against the local
`origin/main` before its single pytest run. The target ref is used as last
fetched; a stale ref only widens the compared range. A missing ref,
failed baseline or head test suite, missing tool/version mismatch, invalid JSON,
missing function coverage, nonfinite metrics, or a changing working tree fails
the run. Results go into a new `run-*` directory under the selected output root:
`report.json`, `report.md`, baseline/head coverage JSON and test logs. The temporary
worktrees are removed after the run. CI publishes only those reports and
logs, never the temporary source tree. The normal test suite runs policy tests
without the optional quality tools; real analyzer integration tests require them.

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
- Newly detected duplicate function pairs fail until resolved or explicitly
  reviewed in `scripts/quality-exceptions.json`. An exception names exactly two
  canonical function identities and explains why keeping the duplication is
  better than extracting it. The initial exception list is empty. Existing pairs
  remain visible in the JSON report; this adoption gate does not demand a
  repository-wide duplication cleanup.
- SCB verbosity, erosion, cognitive erosion, source LOC, per-function cognitive
  complexity and LOC are informational. They are not independent pass thresholds.

Structural analysis covers Python source under `recog/`, excluding native
source, and production JavaScript: the web UI under `recog/` and what the
extension loads (`browser-extension/src/`, `content.js`, `popup.js`). Tests,
scripts, build configuration, generated bundles and ignored files are not
included. The extension's coverage comes from its jest suite
(`--coverageProvider=babel`); the web UI's from the `node:test` suites
`tests/*.test.cjs` under c8, which see a script only when it is evaluated with
its path as the `vm` filename. c8 runs with monocart, which takes the functions
from the source's syntax tree: raw V8 coverage omits a function that was never
compiled, such as a registered handler no test fires. A JavaScript file no
suite loads still gets its functions, at 0%. Functions written on the same line as the function enclosing them
share its line span, which crapkit refuses to join to coverage; start such a
callback on its own line. Ruff applies to changed/new nonignored Python anywhere in the repository,
ESLint to changed/new nonignored JavaScript.

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

With the `rust/` workspace present, `scripts/check.sh` runs `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings` and the echo canceller's
tests that need no data, from `rust/`, then every crate's tests. The parity tests that
ran the deleted Python modules beside their ports went with them.
With `SPEEXDSP_AEC_GOLDEN` set it also runs
the parity tests against pyaec (`rust/speexdsp-aec/README.md`). cargo comes from
rustup (https://rustup.rs) with the clippy and rustfmt components; CI uses the
runner's stable toolchain, so a new clippy lint can fail a run that passed before.

CRAP calculation, coverage joins, function identity and duplicate detection come
from pinned **crapkit 0.7.3**, not a local complexity implementation. It uses the
smaller standard/modified cyclomatic complexity and per-function branch coverage
(statement coverage for a function without branches; for JavaScript, from the
Istanbul report). Same-named methods use
crapkit's canonical ordinal identities; signature changes are conservatively new
functions. Duplicate candidates use its default minimum eight normalized lines
and 0.8 containment; all pairs are retained, not just the first 50. The gate owns
only comparison policy, orchestration and report formatting, plus one
correction of lizard 1.24.0 (`scripts/lizard_template_literals.py`): lizard ends
a template literal at the next backtick, so one nested in another's `${…}` made
the enclosing function absorb every function after it. The gate replaces that
match with a scanner that finds the literal's real end and yields the same
tokens otherwise; it refuses to run with another lizard version.

SCB uses **scb-check 0.2.0**, including ast-grep. Exit 1 means valid findings and is
accepted for its informational metrics; exit 2 or malformed/incomplete output is
an error. Its Python file count must match the production source inventory.

Tool versions and the working-tree fingerprint are recorded in each report.
Both revisions use the current pinned quality configuration and dependencies,
so results compare code changes rather than analyzer versions. A baseline that
cannot run in that environment is an explicit measurement failure requiring
investigation, never an implicit zero baseline.

Official tool contracts:
[crapkit source](https://github.com/JeanFrancoisGagne/crapkit/tree/v0.7.3),
[scb-check](https://github.com/gabeorlanski/scb-check),
[Ruff](https://docs.astral.sh/ruff/),
[ESLint](https://eslint.org/docs/latest/),
[ShellCheck](https://www.shellcheck.net/wiki/),
[actionlint](https://github.com/rhysd/actionlint/blob/main/docs/checks.md),
[gitleaks](https://github.com/gitleaks/gitleaks),
[pytest-cov](https://pytest-cov.readthedocs.io/).
