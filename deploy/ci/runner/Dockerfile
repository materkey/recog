# Self-hosted GitHub Actions runner for materkey/recog: an Ubuntu 24.04 like the
# hosted ubuntu-latest image the workflow was written for, with rustup inside the
# container so the host's own cargo stays untouched.
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive LANG=C.UTF-8 LC_ALL=C.UTF-8
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git build-essential pkg-config libssl-dev libasound2-dev \
      python3 python3-venv python3-pip perl jq zstd xz-utils unzip zip file tzdata \
      sudo procps psmisc lsof iproute2 netbase tmux libicu74 \
    && rm -rf /var/lib/apt/lists/*
RUN useradd -m -s /bin/bash runner && echo "runner ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/runner
USER runner
WORKDIR /home/runner
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
      --component clippy,rustfmt,llvm-tools-preview
ENV PATH=/home/runner/.cargo/bin:$PATH
ARG RUNNER_VERSION=2.338.2
ARG RUNNER_SHA256=baebf204307bac6193480852732f0aba2ae97b6c5597c244f4d5558390147c40
RUN mkdir actions-runner && cd actions-runner \
    && curl -fsSLo runner.tar.gz https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz \
    && echo "${RUNNER_SHA256}  runner.tar.gz" | sha256sum -c - \
    && tar xzf runner.tar.gz && rm runner.tar.gz
COPY --chown=runner:runner entrypoint.sh /home/runner/entrypoint.sh
ENTRYPOINT ["/home/runner/entrypoint.sh"]
