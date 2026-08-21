# syntax=docker/dockerfile:1
#
# Reproducible Alpine-based development environment for Omega, with Claude Code,
# Codex CLI, omp (oh-my-pi) and opencode preinstalled. Every version that
# matters is a pinned build argument, so a rebuild of this file on any machine
# produces the same environment.
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
# version such as 0.51.0. OMP_VERSION is "latest" (the newest GitHub release)
# or an exact release tag, which is what omp's installer means by --ref and so
# carries the leading "v", e.g. v17.2.12. OPENCODE_VERSION is "latest" (the
# default) or an exact version such as 0.4.2, passed to opencode's installer
# through its VERSION variable. LLVM_VERSION is an LLVM *major* version and,
# unlike every other knob here, is constrained from two sides at once: it must
# be a major Alpine packages (3.23 carries 16, 20 and 21) *and* one the Rust
# bindings target. See the LLVM layer below.
ARG RUST_VERSION=1.94.1
ARG CLAUDE_CODE_VERSION=stable
ARG CODEX_VERSION=latest
ARG OMP_VERSION=latest
ARG OPENCODE_VERSION=latest
ARG LLVM_VERSION=21

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
# OPENCODE_DISABLE_AUTOUPDATE: same purpose again -- opencode downloads new
#   releases on startup otherwise. It has no single state directory to point
#   at: config lives in ~/.config/opencode and credentials/sessions in
#   ~/.local/share/opencode, so those get a volume each rather than one
#   consolidating variable. Its own install prefix (~/.opencode/bin, added to
#   PATH above) is deliberately *not* a volume -- that is the pinned binary,
#   which belongs to the image.
# omp needs neither knob: it keeps all of its state under ~/.omp already (one
#   volume covers config, credentials, sessions and blobs), and it only
#   updates itself when you explicitly run `omp update` -- which, in a
#   throwaway container, `./dev.sh rebuild` supersedes.
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
#   * build-base/binutils/gdb -- cc, as and ld, which the justfile invokes
#     directly to assemble shims and link what omgc emits, plus a
#     debugger.
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
# No Node.js and no Bun: Claude Code, Codex CLI, omp and opencode are all
# installed below as their native single-file builds.
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
# creating them here (owned by dev) is what keeps the volumes writable
# without any root-owned-mountpoint fixups at runtime.
RUN mkdir -p \
        /workspace \
        /workspace/target \
        /home/dev/.claude \
        /home/dev/.codex \
        /home/dev/.omp \
        /home/dev/.config/opencode \
        /home/dev/.local/share/opencode \
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

# omp (oh-my-pi), third of the four. `--binary` is not the installer's default
# but is what keeps this image free of a Bun toolchain: left to itself the
# installer builds from source whenever it finds (or can install) Bun. The
# binary path resolves to omp's linux-musl release asset on Alpine and drops it
# in ~/.local/bin, already on PATH above, then smoke-tests `omp --version`
# itself -- so a musl build that cannot start fails this layer rather than the
# first session. "latest" means the newest release; anything else is passed as
# --ref, which in binary mode must name an existing release tag.
RUN set -eux; \
    if [ "${OMP_VERSION}" = "latest" ]; then \
        set -- --binary; \
    else \
        set -- --binary --ref "${OMP_VERSION}"; \
    fi; \
    curl -fsSL https://omp.sh/install | sh -s -- "$@"; \
    omp --version

# opencode, fourth and last. Its installer detects musl on its own (it looks
# for /etc/alpine-release, then falls back to `ldd --version`) and fetches the
# opencode-linux-<arch>-musl asset, another self-contained binary with no Node
# or Bun behind it. Unlike the other three it installs into ~/.opencode/bin
# rather than ~/.local/bin, which is why that directory is on PATH above.
# The installer takes its version from VERSION; leaving it unset means the
# newest release, so it is only passed when actually pinned.
RUN set -eux; \
    if [ "${OPENCODE_VERSION}" = "latest" ]; then \
        curl -fsSL https://opencode.ai/install | bash; \
    else \
        VERSION="${OPENCODE_VERSION}" curl -fsSL https://opencode.ai/install | bash; \
    fi; \
    opencode --version

WORKDIR /workspace

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["bash"]
