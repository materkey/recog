# CI runners

Pull requests and every workflow run in the public repository use GitHub-hosted Linux
and macOS machines. Trusted pushes and manual runs in a private repository can use
the following dedicated runners.

Before making `materkey/recog` public, remove its repository-level runner
registrations and any access through organization runner groups. The public repository
must have no access to these machines even if a contributor changes a workflow.
Register trusted builders only to a private build repository, and build reviewed
revisions there.

| Job | Labels | Runner |
|---|---|---|
| `check` on Linux, `extension` | `self-hosted`, `linux`, `recog` | a container of `deploy/ci/runner/` |
| `check` on macOS | `self-hosted`, `macos`, `recog` | a Mac running the runner as its user |

Each runner takes one job at a time; register more than one Linux container so the
`extension` job does not wait for the Linux `check`.

## Linux: the container

`deploy/ci/runner/Dockerfile` is Ubuntu 24.04, the system the workflow was written
for, with rustup (stable, clippy, rustfmt, llvm-tools-preview) inside the container,
so the host's own Rust stays untouched, and the runner release checked against its
published SHA-256. The container registers on its first start and keeps its
registration in its own filesystem; recreating it needs a new token.

```bash
docker build -t recog-ci-runner:2.338.2 deploy/ci/runner
token=$(gh api -X POST repos/<owner>/recog/actions/runners/registration-token -q .token)
docker run -d --name recog-ci-runner --restart unless-stopped --cpus 24 --memory 64g \
  -e RUNNER_URL=https://github.com/<owner>/recog -e RUNNER_NAME=<name> \
  -e RUNNER_TOKEN="$token" recog-ci-runner:2.338.2
```

A new runner release: change `RUNNER_VERSION` and `RUNNER_SHA256` (from the release
notes) and rebuild; the runner also updates itself between jobs.

## macOS: the user's runner

The job needs rustup, Xcode (the Swift helpers), Python 3.12 and Node, as the hosted
image had them. Download `actions-runner-osx-arm64-<version>.tar.gz` into its own
directory, check its SHA-256, then:

```bash
./config.sh --unattended --url https://github.com/<owner>/recog --token "$token" \
  --name <name> --labels recog --work _work
./svc.sh install && ./svc.sh start   # a LaunchAgent: it runs while the user is logged in
```

The job adds clippy, rustfmt and llvm-tools-preview to the user's stable toolchain, and
takes Python 3.12 from Homebrew (`/opt/homebrew/bin/python3.12`, `brew install
python@3.12`) into a venv of its own: setup-python's macOS builds install only under
`/Users/runner/hostedtoolcache`, which exists on hosted runners alone.
Both kinds of runner keep the workspace's build in `~/.cache/recog-ci/target` between
jobs (a few GB; delete it to start clean) and update their stable Rust at the start
of a job. A sleeping Mac leaves the macOS job queued (GitHub keeps a queued job for a day). The
quality gate runs on Linux only (docs/quality.md), so the Mac builds the workspace
once per job.

## Security

A runner executes whatever a workflow run asks, as its user. It is registered to
this private repository, whose runs come from its own pushes; do not register it to
a public repository or allow workflows from forks.
