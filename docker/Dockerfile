# syntax=docker/dockerfile:1
#
# Reproducible Alpine-based development environment for Omega, with Claude Code
# and Codex CLI preinstalled. Every version that matters is a pinned build
# argument, so a rebuild of this file on any machine produces the same
# environment.
#
# The project source is never COPYed in -- it is bind-mounted at /workspace by
# `docker/compose.yaml`, so the build context stays tiny and edits on the host
# are instantly visible inside the container (and vice versa).

ARG ALPINE_VERSION=3.23
FROM alpine:${ALPINE_VERSION}

# Pinned versions. Override on the CLI or via the environment, e.g.
#   RUST_VERSION=1.95.0 ./dev.sh rebuild
# CLAUDE_CODE_VERSION takes what `claude install` takes: "stable", "latest",
# or an exact version such as 2.1.220. CODEX_VERSION takes what Codex's own
# installer takes: "latest" (there is no "stable" channel) or an exact
# version such as 0.51.0.
ARG RUST_VERSION=1.94.1
ARG CLAUDE_CODE_VERSION=stable
ARG CODEX_VERSION=latest

# The container user is created with the *host* user's uid/gid so that files
# written into the bind-mounted repo are owned by you, not by root.
# The user is always named "dev"; docker/compose.yaml mounts volumes at
# hardcoded /home/dev paths, so this is deliberately not a knob.
ARG USER_UID=1000
ARG USER_GID=1000

# CLAUDE_CONFIG_DIR / CODEX_HOME: keep all Claude Code / Codex CLI state
#   (config, credentials, history) inside one directory each, so a single
#   named volume persists all of it per tool.
# DISABLE_AUTOUPDATER / CODEX_UPDATE_DISABLED: the version of each tool is
#   pinned by this image. Without these, a session would re-download a large
#   binary into a container whose home directory is discarded on exit.
#   `./dev.sh rebuild` is the update path for both.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/home/dev/.local/bin:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    LANG=C.UTF-8 \
    CLAUDE_CONFIG_DIR=/home/dev/.claude \
    DISABLE_AUTOUPDATER=1 \
    CODEX_HOME=/home/dev/.codex \
    CODEX_UPDATE_DISABLED=1 \
    HISTFILE=/commandhistory/.bash_history

# System packages:
#   * bash/coreutils/findutils/grep/sed/diffutils -- GNU userland, so shell
#     commands behave the way tooling (and Claude) expects instead of hitting
#     busybox's reduced flag set.
#   * build-base/binutils/gdb -- cc, as and ld, which the justfile invokes
#     directly to assemble shims and link what omgc emits, plus a
#     debugger.
#   * ripgrep -- Claude Code's search backend, and useful on its own.
#   * just -- the project's task runner.
#   * rustup -- provides rustup-init, used below to pin the exact toolchain.
# No Node.js: Claude Code and Codex CLI are both installed below as their
# native single-file builds.
RUN apk add --no-cache \
        bash coreutils findutils grep sed diffutils \
        tar xz gzip which less nano \
        ca-certificates curl git openssh-client \
        ripgrep \
        build-base binutils gdb \
        just \
        rustup

# Exact Rust toolchain, targeting musl (the native target on Alpine).
# A `rust-toolchain.toml` in the repo, if one is ever added, still wins at
# build time -- rustup will fetch whatever it names.
RUN rustup-init -y \
        --no-modify-path \
        --profile minimal \
        --default-toolchain "${RUST_VERSION}" \
        --component rustfmt \
        --component clippy \
    && chmod -R a+w "${RUSTUP_HOME}" "${CARGO_HOME}" \
    && rustc --version && cargo --version

# Container user matching the host uid/gid.
RUN set -eux; \
    if ! getent group "${USER_GID}" >/dev/null; then \
        addgroup -g "${USER_GID}" dev; \
    fi; \
    adduser -D -u "${USER_UID}" -G "$(getent group "${USER_GID}" | cut -d: -f1)" \
        -s /bin/bash dev

# Pre-create every path that a named volume mounts over. Docker seeds a fresh
# named volume from the image's contents *and ownership* of that path, so
# creating them here (owned by dev) is what keeps the volumes writable
# without any root-owned-mountpoint fixups at runtime.
RUN mkdir -p \
        /workspace \
        /workspace/target \
        /home/dev/.claude \
        /home/dev/.codex \
        /commandhistory \
        "${CARGO_HOME}/registry" \
        "${CARGO_HOME}/git" \
    && touch /commandhistory/.bash_history \
    && chown -R "${USER_UID}:${USER_GID}" \
        /workspace /home/dev /commandhistory "${CARGO_HOME}"

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 0755 /usr/local/bin/entrypoint.sh

USER dev

RUN printf '%s\n' \
    "# Keep an unlimited, persisted shell history (see the 'history' volume)." \
    "HISTSIZE=100000" \
    "HISTFILESIZE=100000" \
    "shopt -s histappend" \
    "PS1='\[\033[1;35m\]omega\[\033[0m\]:\[\033[1;34m\]\w\[\033[0m\]\\$ '" \
    "alias ll='ls -alF --color=auto'" \
    "alias ls='ls --color=auto'" \
    > /home/dev/.bashrc

# Claude Code, installed as the unprivileged user into ~/.local -- the same
# native build the installer puts on any musl host. It is a self-contained
# executable that links against nothing but musl libc, so no Node.js runtime
# and no glibc compatibility shim is involved. Installed after .bashrc is
# written so that anything the installer appends there survives.
RUN curl -fsSL https://claude.ai/install.sh | bash -s -- "${CLAUDE_CODE_VERSION}" \
    && claude --version

# Codex CLI, installed the same way and for the same reason: its own native
# installer lays down a self-contained Rust binary (musl on this base image)
# into ~/.local/bin, already on PATH above -- no Node.js involved, same as
# Claude Code. CODEX_NON_INTERACTIVE skips the installer's PATH-editing
# prompts (PATH is already correct); CODEX_RELEASE pins the version.
RUN CODEX_RELEASE="${CODEX_VERSION}" CODEX_NON_INTERACTIVE=1 \
        curl -fsSL https://chatgpt.com/codex/install.sh | sh \
    && codex --version

WORKDIR /workspace

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["bash"]
