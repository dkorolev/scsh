# syntax=docker/dockerfile:1
#
# The base image scsh builds for every skill container. This file IS the source of truth:
# scsh reads it verbatim (include_str!) and streams it to the container builder's stdin —
# it is never written to disk. It is PLATFORM-AGNOSTIC: every architecture-specific
# download resolves the target arch at build time (dpkg --print-architecture -> amd64|arm64),
# so the same Dockerfile builds on x86_64 and arm64 (Apple Silicon, arm servers) alike.
#
# The image is GENERIC: opencode (the harness) plus a broad dev/CLI toolchain and a non-root
# `agent` user — and no per-skill CMD. Each skill's container supplies its own harness command
# at run time (see runtime::harness_command / run_command), so one image serves every skill.
FROM debian:bookworm-slim
LABEL scsh.generated=true

# Host user's UID/GID, so the agent owns the files it writes into the bind-mounted clone.
ARG AGENT_UID=1000
ARG AGENT_GID=1000
# The builder host's timezone (scsh passes --build-arg TZ); defaults to UTC.
ARG TZ=UTC
ENV DEBIAN_FRONTEND=noninteractive

# 1. Base dev/CLI userland from apt, in one cached layer: build deps (build-essential,
#    pkg-config, libssl-dev, cmake), interpreters/tools (python3, perl, gawk), data/CLI
#    (jq, sqlite3, postgresql-client, protobuf-compiler, ripgrep, shellcheck), VCS/transport
#    (git, git-lfs, openssh-client, gnupg, curl/wget), archives, and network diagnostics.
#    Java is deliberately NOT installed: nothing here is JVM and a JDK is ~300 MB.
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
      ca-certificates curl wget gnupg \
      git git-lfs openssh-client \
      bash less tree file procps psmisc \
      tar gzip xz-utils zip unzip patch diffutils \
      locales tzdata \
      build-essential pkg-config libssl-dev cmake \
      python3 python3-venv \
      perl gawk \
      jq sqlite3 postgresql-client \
      ripgrep shellcheck \
      protobuf-compiler \
      iputils-ping traceroute bind9-dnsutils netcat-openbsd iproute2 whois socat; \
    rm -rf /var/lib/apt/lists/*

# 2. UTF-8 locale (C.UTF-8 — a glibc built-in, no locale-gen) + the builder host's timezone.
RUN set -eux; \
    ln -snf "/usr/share/zoneinfo/${TZ:-UTC}" /etc/localtime; \
    echo "${TZ:-UTC}" > /etc/timezone

# 3. Node + opencode (the harness). opencode installs from npm (far more reliable than its
#    curl|bash installer); the global modules are made world-readable for the non-root agent;
#    `opencode --version` runs last, so a broken image fails the build — a successful build is
#    itself proof the harness runs in the container.
RUN set -eux; \
    curl -fsSL https://deb.nodesource.com/setup_20.x | bash -; \
    apt-get install -y --no-install-recommends nodejs; \
    rm -rf /var/lib/apt/lists/*; \
    npm install -g opencode-ai; \
    chmod -R a+rX "$(npm root -g)"; \
    opencode --version

# 4. Single-binary / apt-repo CLIs into /usr/local: uv (Python), yq (YAML), kubectl, gh.
#    Arch is resolved at build time, so this works on amd64 and arm64. Each is version-checked.
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    case "$arch" in \
      amd64) uvarch=x86_64 ;; \
      arm64) uvarch=aarch64 ;; \
      *) echo "scsh: unsupported architecture '$arch'" >&2; exit 1 ;; \
    esac; \
    tmp="$(mktemp -d)"; \
    curl -fsSL "https://github.com/astral-sh/uv/releases/latest/download/uv-${uvarch}-unknown-linux-gnu.tar.gz" \
      | tar xz -C "$tmp" --strip-components=1; \
    install -m 0755 "$tmp/uv" "$tmp/uvx" /usr/local/bin/; rm -rf "$tmp"; \
    curl -fsSL "https://github.com/mikefarah/yq/releases/latest/download/yq_linux_${arch}" -o /usr/local/bin/yq; \
    chmod +x /usr/local/bin/yq; \
    curl -fsSL "https://dl.k8s.io/release/$(curl -fsSL https://dl.k8s.io/release/stable.txt)/bin/linux/${arch}/kubectl" \
      -o /usr/local/bin/kubectl; \
    chmod +x /usr/local/bin/kubectl; \
    install -d -m 0755 /etc/apt/keyrings; \
    curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      -o /etc/apt/keyrings/githubcli-archive-keyring.gpg; \
    chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg; \
    echo "deb [arch=${arch} signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      > /etc/apt/sources.list.d/github-cli.list; \
    apt-get update; apt-get install -y --no-install-recommends gh; rm -rf /var/lib/apt/lists/*; \
    uv --version; yq --version; kubectl version --client; gh --version

# 5. Go toolchain (latest stable) from the upstream tarball into /usr/local/go.
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    GO_VER="$(curl -fsSL 'https://go.dev/VERSION?m=text' | head -1)"; \
    curl -fsSL "https://go.dev/dl/${GO_VER}.linux-${arch}.tar.gz" | tar xz -C /usr/local; \
    /usr/local/go/bin/go version

# 6. Rust toolchain via rustup into world-writable CARGO_HOME/RUSTUP_HOME, so the non-root
#    agent can run cargo and write its registry cache. rustup auto-detects the architecture.
RUN set -eux; \
    export CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup; \
    curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal; \
    chmod -R a+rwX /usr/local/cargo; \
    /usr/local/cargo/bin/rustc --version; /usr/local/cargo/bin/cargo --version

# 7. AWS CLI v2 (official zip installer) into /usr/local.
RUN set -eux; \
    case "$(dpkg --print-architecture)" in \
      amd64) awsarch=x86_64 ;; \
      arm64) awsarch=aarch64 ;; \
      *) echo "scsh: unsupported architecture" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "https://awscli.amazonaws.com/awscli-exe-linux-${awsarch}.zip" -o /tmp/awscliv2.zip; \
    unzip -q /tmp/awscliv2.zip -d /tmp; /tmp/aws/install; rm -rf /tmp/aws /tmp/awscliv2.zip; \
    aws --version

# 8. Google Cloud CLI (gcloud + gsutil) from the upstream tarball. Large (~1 GB).
RUN set -eux; \
    case "$(dpkg --print-architecture)" in \
      amd64) gclarch=x86_64 ;; \
      arm64) gclarch=arm ;; \
      *) echo "scsh: unsupported architecture" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "https://dl.google.com/dl/cloudsdk/channels/rapid/downloads/google-cloud-cli-linux-${gclarch}.tar.gz" \
      | tar xz -C /usr/local; \
    /usr/local/google-cloud-sdk/install.sh --quiet --usage-reporting=false --path-update=false --command-completion=false; \
    ln -snf /usr/local/google-cloud-sdk/bin/gcloud /usr/local/bin/gcloud; \
    ln -snf /usr/local/google-cloud-sdk/bin/gsutil /usr/local/bin/gsutil; \
    gcloud --version

# 9. Create the non-root `agent` user the skills run as, with the host user's numeric UID/GID so
#    files written into the mount are owned by the host user. Its home (/home/agent) is kept
#    SEPARATE from the repo: the clone is mounted at /home/agent/repo, so the harness's home-dir
#    scratch (~/.cache, ~/.config, ~/.npm, …) never lands in the repo's working tree. Tolerant of
#    a UID/GID that already exists in the base image (the UID match is what makes the mount
#    writable, so a clashing GID falls back without failing the build).
RUN set -eu; \
    uid="${AGENT_UID:-1000}"; gid="${AGENT_GID:-1000}"; \
    groupadd -g "$gid" agent 2>/dev/null || groupadd agent 2>/dev/null || true; \
    useradd -u "$uid" -g "$gid" -m -d /home/agent -s /bin/sh agent 2>/dev/null \
      || useradd -g "$gid" -m -d /home/agent -s /bin/sh agent 2>/dev/null || true; \
    mkdir -p /home/agent/repo; chown -R "$uid:$gid" /home/agent

WORKDIR /home/agent/repo
USER agent

# Run opencode unattended: no permission prompts (a skill runs headless in a throwaway
# container, so there's no human to approve tool calls).
ENV OPENCODE_YOLO=true
ENV OPENCODE_DANGEROUSLY_SKIP_PERMISSIONS=true
# Mark the environment as scsh-managed. Skills read SCSH=1 to know they run under scsh —
# e.g. to accept the (expectedly) dirty sandbox clone they would refuse on the host.
ENV SCSH=1
# Home is /home/agent (NOT the repo mount at /home/agent/repo), so the harness's home-dir
# scratch — opencode's ~/.cache/~/.config, npm's ~/.npm — stays out of the cloned repo.
ENV HOME=/home/agent
# opencode's data dir (where scsh forwards the credential, and opencode writes session data).
# Pointed under the mounted repo's gitignored tmp/, so the auth and session data never appear
# as untracked files — and are removed with the run dir afterward.
ENV XDG_DATA_HOME=/home/agent/repo/tmp/.xdg-data
# UTF-8 everywhere; carry the builder host's timezone into the run.
ENV LANG=C.UTF-8
ENV LC_ALL=C.UTF-8
ENV TZ=${TZ}
# Make the agent-usable Go/Rust toolchains resolvable on PATH.
ENV CARGO_HOME=/usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup
ENV PATH=/usr/local/go/bin:/usr/local/cargo/bin:/usr/local/bin:$PATH
# Each harness run tees its combined output to this log under the mount's gitignored tmp/, so
# the full intra-container output can be examined on the host (<run_dir>/tmp/scsh-run.log).
ENV SCSH_RUN_LOG=/home/agent/repo/tmp/scsh-run.log
