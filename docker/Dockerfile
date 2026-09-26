# syntax=docker/dockerfile:1
#
# Reproducible Alpine-based development environment for Omega. Every version
# that matters is a pinned build argument, so a rebuild of this file on any
# machine produces the same environment.
#
# What this image is, exactly: the Omega build toolchain. Rust, LLVM, binutils,
# just and ruby, all pinned. The agent CLIs (Claude Code, Codex, omp, opencode)
# are deliberately *not* here -- they release constantly, none of them is
# pinned to a version by default, and together they measure about a gigabyte,
# more than LLVM. See docker/install-agents.sh, which puts them in the volumes
# that already hold their logins. That keeps this image reproducible in the
# sense the word is worth having, and means a rebuild neither re-downloads them
# nor silently freezes "stable" at whenever the last rebuild happened.
#
# The project source is never COPYed in -- it is bind-mounted at /workspace by
# `docker/compose.yaml`, so the build context stays tiny and edits on the host
# are instantly visible inside the container (and vice versa).

ARG ALPINE_VERSION=3.23
FROM alpine:${ALPINE_VERSION}

# Pinned versions. Override on the CLI or via the environment, e.g.
#   RUST_VERSION=1.95.0 ./dev.sh rebuild
# LLVM_VERSION is an LLVM *major* version and, unlike every other knob here, is
# constrained from two sides at once: it must be a major Alpine packages (3.23
# carries 16, 20 and 21) *and* one the Rust bindings target. See the LLVM layer
# below.
#
# The agent CLIs have no build argument, because they are not built into this
# image at all. Their versions are chosen at install time instead; see
# `./dev.sh update-agents` and docker/install-agents.sh.
ARG RUST_VERSION=1.94.1
ARG LLVM_VERSION=21

# The container user is created with the *host* user's uid/gid so that files
# written into the bind-mounted repo are owned by you, not by root.
# The user is always named "dev"; docker/compose.yaml mounts volumes at
# hardcoded /home/dev paths, so this is deliberately not a knob.
ARG USER_UID=1000
ARG USER_GID=1000

# CLAUDE_CONFIG_DIR / CODEX_HOME: keep all Claude Code / Codex CLI state
#   (config, credentials, history) inside one directory each, so a single
#   named volume persists all of it per tool. On the Codex side that volume
#   also ends up holding the binary itself, since its installer puts the
#   payload under $CODEX_HOME/packages and makes ~/.local/bin/codex a symlink
#   into it. That is fine now that no layer here installs codex: the volume is
#   the only copy, rather than one shadowing an image-owned one.
# DISABLE_AUTOUPDATER / CODEX_UPDATE_DISABLED / OPENCODE_DISABLE_AUTOUPDATE:
#   these three tools replace themselves in place on startup otherwise. Their
#   install prefixes now persist, so that would work -- but it would mean four
#   tools updating on four schedules, each adding start-up latency, and a
#   binary being rewritten underneath a session that is already running it
#   (several containers share these volumes at once). One explicit update path
#   instead: `./dev.sh update-agents`. That is the model omp already has --
#   it only moves when you run `omp update` yourself, which is why it is the
#   one agent here that needs no switch.
# OPENCODE_DISABLE_AUTOUPDATE is also the odd one in *where* it keeps things:
#   no single state directory to point at, with config in ~/.config/opencode
#   and credentials/sessions in ~/.local/share/opencode, so those get a volume
#   each rather than one consolidating variable. Its install prefix is a third
#   directory again, ~/.opencode/bin, added to PATH above.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/home/dev/.local/bin:/home/dev/.opencode/bin:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    LANG=C.UTF-8 \
    CLAUDE_CONFIG_DIR=/home/dev/.claude \
    DISABLE_AUTOUPDATER=1 \
    CODEX_HOME=/home/dev/.codex \
    CODEX_UPDATE_DISABLED=1 \
    OPENCODE_DISABLE_AUTOUPDATE=1 \
    HISTFILE=/commandhistory/.bash_history

# System packages:
#   * bash/coreutils/findutils/grep/sed/diffutils -- GNU userland, so shell
#     commands behave the way tooling (and Claude) expects instead of hitting
#     busybox's reduced flag set.
#   * build-base/binutils/gdb -- cc, as and ld, which the justfile and
#     bin/test-runner invoke directly to link what omgc emits and to build the
#     freestanding C helpers a few tests ship, plus a debugger.
#   * ripgrep -- Claude Code's search backend, and useful on its own.
#   * just -- the project's task runner.
#   * rustup -- provides rustup-init, used below to pin the exact toolchain.
#   * libstdc++/libgcc -- omp's prebuilt musl binary links both dynamically,
#     and stock Alpine ships neither; without them the download succeeds and
#     the binary dies with relocation errors. build-base pulls them in as a
#     side effect today, but omp needs them in its own right.
#   * ruby -- interprets bin/test-runner, the root conformance-suite driver.
#     Alpine's `ruby` package depends on the `libruby.so` shared object
#     provided by `ruby-libs`, so apk resolves and installs that
#     automatically; ruby-libs already carries fileutils/open3/pathname, the
#     only stdlib the runner uses, so no separate stdlib subpackage is
#     needed.
# No Node.js and no Bun. The agents are installed at run time rather than here
# (see docker/install-agents.sh), and every one of them has a native
# single-file build, so neither runtime is needed to install or run them.
RUN apk add --no-cache \
        bash coreutils findutils grep sed diffutils \
        tar xz gzip which less nano \
        ca-certificates curl git openssh-client \
        ripgrep \
        build-base binutils gdb \
        libstdc++ libgcc \
        just \
        rustup \
        ruby

# LLVM, the compiler's required codegen toolchain (see `omega-codegen` and
# docs/architecture/mir-and-codegen.md). Deliberately its own layer and its
# own apk invocation rather than joining the block above, for two reasons:
# it is by far the largest
# thing in this image (~700 MB installed -- /usr/lib/llvm<N> alone measures
# 674 MB, several times everything else put together), and it is the only
# system dependency whose major version the
# Rust side has to agree with -- so moving LLVM_VERSION should rebuild this
# and nothing above it.
#
#   * llvm-dev     -- headers, and llvm-config, which is how llvm-sys (and
#                     inkwell, which wraps it) finds everything else. Pulls
#                     in llvm-libs, the shared library.
#   * llvm-static  -- the static archives, under `llvm-config --libdir`. Not
#                     optional on this image: the toolchain is
#                     x86_64-unknown-linux-musl, where Rust enables
#                     crt-static by default, so LLVM ends up statically
#                     linked whether or not anyone asked it to be.
#   * *-static     -- `llvm-config --link-static --system-libs` here reports
#                     `-lrt -ldl -lm -lz -lzstd -lxml2`, so a static link
#                     needs the .a counterparts of zlib, zstd and libxml2 as
#                     well; libstdc++.a already arrives with build-base
#                     above. Note the `--link-static`: plain `--system-libs`
#                     reports *nothing* on this build, which makes it a
#                     misleading thing to check. libxml2 earns its place
#                     even though no LLVM path here draws a symbol from it
#                     -- `-lxml2` is on the link line regardless, and the
#                     link fails outright if the archive is missing. The
#                     smoke test below asserts all four exist and records
#                     the real list in the build log.
#   * clang        -- LLVM's integrated assembler. It cross-assembles for
#                     any target through `-target`, where binutils' `as` is
#                     one binary per target; that is what lets one image
#                     assemble for every target LLVM can emit.
#   * lld          -- the same argument, for linking.
#
# Alpine keeps each LLVM major self-contained under /usr/lib/llvm<N> and
# ships no unversioned `llvm-config`, so that directory goes on PATH. With
# it there llvm-sys discovers llvm-config unaided and needs no
# LLVM_SYS_<ver>_PREFIX -- worth the PATH entry on its own, because that
# variable embeds the version in its *name* and so could never track
# LLVM_VERSION. Appended, not prepended: nothing here should shadow a
# system tool of the same name.
ENV PATH="${PATH}:/usr/lib/llvm${LLVM_VERSION}/bin"
RUN apk add --no-cache \
        "llvm${LLVM_VERSION}-dev" \
        "llvm${LLVM_VERSION}-static" \
        "clang${LLVM_VERSION}" \
        "lld${LLVM_VERSION}" \
        zlib-static zstd-static libxml2-static \
    && llvm-config --version | grep -q "^${LLVM_VERSION}\." \
    && test -f "$(llvm-config --libdir)/libLLVMCore.a" \
    && test -f /usr/lib/libz.a && test -f /usr/lib/libzstd.a \
    && test -f /usr/lib/libxml2.a && test -f /usr/lib/libstdc++.a \
    && clang --version && ld.lld --version \
    && echo "system libs: $(llvm-config --link-static --system-libs)"

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
# creating them here (owned by dev) is what keeps the volumes writable without
# any root-owned-mountpoint fixups at runtime. That now includes the two trees
# the agents install into -- ~/.local and ~/.opencode -- which stay empty here:
# the image creates the mountpoints, install-agents.sh fills the volumes.
RUN mkdir -p \
        /workspace \
        /workspace/target \
        /home/dev/.claude \
        /home/dev/.codex \
        /home/dev/.omp \
        /home/dev/.config/opencode \
        /home/dev/.local/bin \
        /home/dev/.local/share/opencode \
        /home/dev/.opencode/bin \
        /commandhistory \
        "${CARGO_HOME}/registry" \
        "${CARGO_HOME}/git" \
    && touch /commandhistory/.bash_history \
    && chown -R "${USER_UID}:${USER_GID}" \
        /workspace /home/dev /commandhistory "${CARGO_HOME}"

# install-agents.sh is deliberately NOT copied in. It runs from the repo at
# /workspace/docker/, which compose always bind-mounts, so changing how an
# agent installs takes effect on the next run instead of needing a rebuild --
# the same reason the agents themselves are not in this image. entrypoint.sh
# has to be copied, since it is what the image runs.
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 0755 /usr/local/bin/entrypoint.sh

# Alpine's /etc/profile overwrites PATH with a fixed list, so any login shell
# (`bash -l`, which is how Codex and some other agents run every command) would
# lose cargo, the agents and LLVM. profile.d runs after that reset, so pin the
# image's final PATH back there. Written after every `ENV PATH` above on
# purpose: this is a build-time snapshot of it.
RUN printf 'export PATH="%s"\n' "${PATH}" > /etc/profile.d/10-omega-path.sh \
    && bash -lc 'command -v cargo && command -v rustc && command -v llvm-config'

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

WORKDIR /workspace

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["bash"]
