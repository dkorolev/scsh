# syntax=docker/dockerfile:1
#
# Harness container images for scsh. This file IS the source of truth: scsh reads it
# verbatim (include_str!) and streams it to the container builder's stdin — it is never
# written to disk. PLATFORM-AGNOSTIC: every architecture-specific download resolves the
# target arch at build time (dpkg --print-architecture -> amd64|arm64).
#
# Four final images share one base (`scsh-base`): `scsh-opencode`, `scsh-claude`,
# `scsh-codex`, `scsh-grok`, and `scsh-cursor`. Each installs its harness CLI last. scsh builds
# `scsh-base:latest` first (or skips when up to date), then only the harness image(s)
# a run needs (`--target` per image).
FROM debian:bookworm-slim AS scsh-base
LABEL scsh.generated=true

ARG AGENT_UID=1000
ARG AGENT_GID=1000
ARG TZ=UTC
ENV DEBIAN_FRONTEND=noninteractive

# 1. Base dev/CLI userland from apt.
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
      tmux \
      protobuf-compiler \
      iputils-ping traceroute bind9-dnsutils netcat-openbsd iproute2 whois socat; \
    rm -rf /var/lib/apt/lists/*

# 2. UTF-8 locale + the builder host's timezone.
RUN set -eux; \
    ln -snf "/usr/share/zoneinfo/${TZ:-UTC}" /etc/localtime; \
    echo "${TZ:-UTC}" > /etc/timezone

# 3. Single-binary / apt-repo CLIs into /usr/local.
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    case "$arch" in \
      amd64) uvarch=x86_64; asciinema_arch=x86_64 ;; \
      arm64) uvarch=aarch64; asciinema_arch=aarch64 ;; \
      *) echo "scsh: unsupported architecture '$arch'" >&2; exit 1 ;; \
    esac; \
    tmp="$(mktemp -d)"; \
    curl -fsSL "https://github.com/astral-sh/uv/releases/latest/download/uv-${uvarch}-unknown-linux-gnu.tar.gz" \
      | tar xz -C "$tmp" --strip-components=1; \
    install -m 0755 "$tmp/uv" "$tmp/uvx" /usr/local/bin/; rm -rf "$tmp"; \
    # asciinema 3.x (asciicast v3). Debian bookworm still ships 2.2 (v2-only) — pin the
    # official static binary so every harness recording is the current format.
    curl -fsSL "https://github.com/asciinema/asciinema/releases/download/v3.2.1/asciinema-${asciinema_arch}-unknown-linux-gnu" \
      -o /usr/local/bin/asciinema; \
    chmod +x /usr/local/bin/asciinema; \
    asciinema --version; \
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

# 4. Go toolchain.
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    GO_VER="$(curl -fsSL 'https://go.dev/VERSION?m=text' | head -1)"; \
    curl -fsSL "https://go.dev/dl/${GO_VER}.linux-${arch}.tar.gz" | tar xz -C /usr/local; \
    /usr/local/go/bin/go version

# 5. Rust toolchain.
RUN set -eux; \
    export CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup; \
    curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal; \
    chmod -R a+rwX /usr/local/cargo; \
    /usr/local/cargo/bin/rustc --version; /usr/local/cargo/bin/cargo --version

# 6. AWS CLI v2.
RUN set -eux; \
    case "$(dpkg --print-architecture)" in \
      amd64) awsarch=x86_64 ;; \
      arm64) awsarch=aarch64 ;; \
      *) echo "scsh: unsupported architecture" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "https://awscli.amazonaws.com/awscli-exe-linux-${awsarch}.zip" -o /tmp/awscliv2.zip; \
    unzip -q /tmp/awscliv2.zip -d /tmp; /tmp/aws/install; rm -rf /tmp/aws /tmp/awscliv2.zip; \
    aws --version

# 7. Google Cloud CLI.
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

# 8. Non-root agent user; repo mount is a subdirectory of home.
RUN set -eu; \
    uid="${AGENT_UID:-1000}"; gid="${AGENT_GID:-1000}"; \
    groupadd -g "$gid" agent 2>/dev/null || groupadd agent 2>/dev/null || true; \
    useradd -u "$uid" -g "$gid" -m -d /home/agent -s /bin/sh agent 2>/dev/null \
      || useradd -g "$gid" -m -d /home/agent -s /bin/sh agent 2>/dev/null || true; \
    mkdir -p /home/agent/repo; chown -R "$uid:$gid" /home/agent

WORKDIR /home/agent/repo

# SCSH=1: skill runs under scsh. Repo arrives via bind-mount or git transport. Skills must not git fetch/pull remotes inside. After exit, scsh pulls result OUT; commits OUT only when commits: true.
ENV SCSH=1
ENV HOME=/home/agent
ENV LANG=C.UTF-8
ENV LC_ALL=C.UTF-8
ENV TZ=${TZ}
ENV CARGO_HOME=/usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup
ENV PATH=/usr/local/go/bin:/usr/local/cargo/bin:/usr/local/bin:$PATH
ENV SCSH_RUN_LOG=/home/agent/repo/tmp/scsh-run.log
# Containers run without a runtime-allocated TTY; asciinema gives the harness a PTY and
# records it, and needs a sane TERM for the pseudo-terminal it creates.
ENV TERM=xterm-256color

# scsh-tui-record: record a harness's interactive TUI as an asciicast. `scsh run` invokes it
# instead of embedding a shell program in the container command. It runs the harness TUI (CMD)
# in a COLS x ROWS tmux session, records the attached screen to $SCSH_RUN_LOG.cast, and once
# RESULT (the skill's result file) exists sends the harness its quit keystrokes and ends the
# session. Deliberately NO screen-scraping: every harness is pre-configured (flags + seeded
# config) so no consent/trust/login dialog ever appears; one that blocks is a setup bug that
# should surface (via the container timeout), not be auto-clicked.
RUN cat > /usr/local/bin/scsh-tui-record <<'SCSH_TUI_RECORD'
#!/bin/sh
# Usage: scsh-tui-record COLS ROWS QUIT SUBMIT RESULT CMD
#   QUIT:   slash-exit (Claude) | double-ctrl-c (codex, cursor, grok, opencode)
#   SUBMIT: none (the prompt auto-runs) | enter (opencode --prompt only PRE-FILLS the input,
#           so an Enter must be sent once the TUI is up to actually submit it)
set -u
cols=$1; rows=$2; quit=$3; submit=$4; result=$5; cmd=$6
dbg="$SCSH_RUN_LOG.tuidebug"
exit_file="$SCSH_RUN_LOG.exit"
log() { echo "$(date +%H:%M:%S.%N 2>/dev/null | cut -c1-12) $*" >> "$dbg" 2>/dev/null; }

# Resolve RESULT to an absolute path so a CWD change (git-transport clones into the repo dir)
# can never make the watcher misjudge whether the skill has finished (hypothesis: false match).
case "$result" in /*) : ;; *) result="$(pwd)/$result" ;; esac

log "start cols=$cols rows=$rows quit=$quit result=$result cwd=$(pwd) pid=$$"
log "result_exists_at_start=$([ -f "$result" ] && echo YES || echo NO)"

# Wrap the harness in traps so the pane ALWAYS records how it ended: the EXIT trap writes the
# exit code (fires on normal exit AND catchable signals), and per-signal traps name the signal.
# Only an uncatchable SIGKILL (OOM / force-stop) escapes — so an ABSENT .exit file is itself the
# diagnosis. This replaces a bare `; echo $?`, which a signal would skip.
#
# The harness runs in a RELAUNCH LOOP (max 3 launches): external tools are fallible, and
# cursor-agent has been observed dying to a stray SIGTERM ~1.5s into a run (pane_exit=143,
# nothing on screen — recorded 2026-07-06). A harness that exits while the result file is
# still missing is relaunched inside the same pane and recording; one that exits AFTER
# writing the result (the normal quit path) ends the pane. `.exit` records the final launch.
pane_cmd="trap 'ec=\$?; echo \$ec > \"$exit_file\"; echo pane_exit=\$ec >> \"$dbg\"' EXIT; \
trap 'echo pane_signal=TERM >> \"$dbg\"' TERM; \
trap 'echo pane_signal=INT  >> \"$dbg\"' INT; \
trap 'echo pane_signal=HUP  >> \"$dbg\"' HUP; \
n=0; while :; do $cmd; ec=\$?; \
if [ -f \"$result\" ] || [ \$n -ge 2 ]; then exit \$ec; fi; \
n=\$((n+1)); echo pane_relaunch=\$n after_ec=\$ec >> \"$dbg\"; sleep 1; done"

tmux -f /dev/null new-session -d -x "$cols" -y "$rows" -s scsh "$pane_cmd"
log "tmux new-session rc=$?"
tmux set -t scsh status off >/dev/null 2>&1 || true

# Submitter: some harnesses (opencode) launch with the prompt only PRE-FILLED in the input box,
# so it never runs until an Enter is sent. Send it once the TUI has had time to come up, and once
# more if the result still hasn't appeared — then stop, so we never inject stray empty submits.
if [ "$submit" = enter ]; then
  ( sleep 8; tmux send-keys -t scsh Enter 2>/dev/null; log "submitter: sent Enter (1)"
    sleep 10; [ -f "$result" ] || { tmux send-keys -t scsh Enter 2>/dev/null; log "submitter: sent Enter (2)"; }
  ) >/dev/null 2>&1 &
fi

# Watcher: end the TUI once the skill has produced its result file.
{
  while tmux has-session -t scsh 2>/dev/null; do
    if [ -f "$result" ]; then
      log "watcher: result appeared -> quit via $quit"
      sleep 4
      case "$quit" in
        slash-exit)    tmux send-keys -t scsh -l /exit; sleep 1; tmux send-keys -t scsh Enter ;;
        double-ctrl-c) tmux send-keys -t scsh C-c;      sleep 1; tmux send-keys -t scsh C-c   ;;
      esac
      # Wait for the harness to exit on its own so the pane's EXIT trap runs and `.exit`
      # gets written (killing the session skips the trap — codex used to lose its `.exit`
      # this way). Re-send the quit keys once, force-kill only a harness that ignores both.
      i=0
      while tmux has-session -t scsh 2>/dev/null && [ "$i" -lt 12 ]; do
        i=$((i+1))
        if [ "$i" -eq 6 ]; then
          log "watcher: quit ignored for 6s -> re-send $quit"
          case "$quit" in
            slash-exit)    tmux send-keys -t scsh -l /exit; sleep 1; tmux send-keys -t scsh Enter ;;
            double-ctrl-c) tmux send-keys -t scsh C-c;      sleep 1; tmux send-keys -t scsh C-c   ;;
          esac
        fi
        sleep 1
      done
      if tmux has-session -t scsh 2>/dev/null; then
        tmux kill-session -t scsh 2>/dev/null || true
        log "watcher: killed session (harness ignored quit)"
      else
        log "watcher: session ended after quit"
      fi
      break
    fi
    sleep 2
  done
  log "watcher: exit (session gone or killed)"
} >/dev/null 2>&1 &

# asciinema 3.x defaults to asciicast-v3; --window-size replaces the old --cols/--rows flags.
asciinema rec -q --overwrite -f asciicast-v3 --window-size "${cols}x${rows}" \
  -c 'tmux attach -r -t scsh' "$SCSH_RUN_LOG.cast"
log "asciinema exited rc=$?"
wait
log "done"
SCSH_TUI_RECORD
RUN chmod 0755 /usr/local/bin/scsh-tui-record

# --- OpenCode harness image (install harness last) ---
FROM scsh-base AS scsh-opencode
RUN set -eux; \
    curl -fsSL https://deb.nodesource.com/setup_20.x | bash -; \
    apt-get install -y --no-install-recommends nodejs; \
    rm -rf /var/lib/apt/lists/*; \
    npm install -g opencode-ai; \
    chmod -R a+rX "$(npm root -g)"; \
    opencode --version
ENV OPENCODE_YOLO=true
ENV OPENCODE_DANGEROUSLY_SKIP_PERMISSIONS=true
# opencode reads auth from $XDG_DATA_HOME/opencode and config from $XDG_CONFIG_HOME/opencode.
# Both point under the repo-mounted, gitignored tmp/ so scsh forwards them by copying into the
# run clone (riding the repo mount) — no separate single-file bind mounts (which Docker Desktop
# on macOS rejects), same as claude/codex/grok/cursor.
ENV XDG_DATA_HOME=/home/agent/repo/tmp/.xdg-data
ENV XDG_CONFIG_HOME=/home/agent/repo/tmp/.config
USER agent

# --- Claude Code harness image (install harness last) ---
FROM scsh-base AS scsh-claude
RUN set -eux; \
    curl -fsSL https://deb.nodesource.com/setup_20.x | bash -; \
    apt-get install -y --no-install-recommends nodejs; \
    rm -rf /var/lib/apt/lists/*; \
    npm install -g @anthropic-ai/claude-code; \
    chmod -R a+rX "$(npm root -g)"; \
    claude --version
# Claude reads config/state from $CLAUDE_CONFIG_DIR (state json at .claude.json inside it).
# scsh points it at the gitignored tmp/ of the repo mount, where forwarded host config is
# copied per run — writable, which the interactive TUI needs (else it re-runs onboarding).
ENV CLAUDE_CONFIG_DIR=/home/agent/repo/tmp/.claude-auth/.claude
USER agent

# --- OpenAI Codex CLI harness image (install harness last) ---
FROM scsh-base AS scsh-codex
RUN set -eux; \
    curl -fsSL https://deb.nodesource.com/setup_20.x | bash -; \
    apt-get install -y --no-install-recommends nodejs; \
    rm -rf /var/lib/apt/lists/*; \
    npm install -g @openai/codex; \
    chmod -R a+rX "$(npm root -g)"; \
    codex --version
# Codex reads auth/config from $CODEX_HOME (default ~/.codex). scsh points it at the
# gitignored tmp/ of the repo mount, where forwarded host credentials are copied per run.
ENV CODEX_HOME=/home/agent/repo/tmp/.codex
USER agent

# --- xAI Grok CLI harness image (install harness last) ---
# @xai-official/grok is the npm distribution of Grok Build — no x.ai host or shell-script
# installer needed, and it resolves the target arch itself.
FROM scsh-base AS scsh-grok
RUN set -eux; \
    curl -fsSL https://deb.nodesource.com/setup_20.x | bash -; \
    apt-get install -y --no-install-recommends nodejs; \
    rm -rf /var/lib/apt/lists/*; \
    npm install -g @xai-official/grok; \
    chmod -R a+rX "$(npm root -g)"; \
    grok --version
# Grok reads auth/config from $GROK_HOME (default ~/.grok). Same trick as codex: point it
# at the gitignored tmp/ of the repo mount, where forwarded host credentials are copied.
ENV GROK_HOME=/home/agent/repo/tmp/.grok
USER agent

# --- Cursor Agent CLI harness image (install harness last) ---
FROM scsh-base AS scsh-cursor
ARG CURSOR_AGENT_VERSION=2026.07.01-41b2de7
ENV CURSOR_AGENT_HOME=/usr/local/share/cursor-agent
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    case "$arch" in \
      amd64) cursorarch=x64 ;; \
      arm64) cursorarch=arm64 ;; \
      *) echo "scsh: unsupported architecture" >&2; exit 1 ;; \
    esac; \
    tmp="$(mktemp -d)"; \
    curl -fsSL "https://downloads.cursor.com/lab/${CURSOR_AGENT_VERSION}/linux/${cursorarch}/agent-cli-package.tar.gz" \
      | tar -xzf - -C "$tmp"; \
    rm -rf "$CURSOR_AGENT_HOME"; \
    mv "$tmp/dist-package" "$CURSOR_AGENT_HOME"; \
    rm -rf "$tmp"; \
    ln -sf "$CURSOR_AGENT_HOME/cursor-agent" /usr/local/bin/cursor-agent; \
    ln -sf "$CURSOR_AGENT_HOME/cursor-agent" /usr/local/bin/agent; \
    cursor-agent --version
# cursor-agent reads cli-config from $CURSOR_CONFIG_DIR and OAuth tokens from
# $XDG_CONFIG_HOME/cursor/auth.json on Linux. scsh forwards both into gitignored tmp/ per run.
ENV CURSOR_CONFIG_DIR=/home/agent/repo/tmp/.cursor
ENV XDG_CONFIG_HOME=/home/agent/repo/tmp/.config
USER agent
