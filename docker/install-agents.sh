#!/usr/bin/env bash
#
# Install or update the agent CLIs (Claude Code, Codex, omp, opencode).
#
# These deliberately are not part of the image: they release constantly, none
# of them is pinned to a version by default, and together they weigh about a
# gigabyte -- more than LLVM. Baking them in meant every `./dev.sh rebuild`
# re-downloaded all of it, while the Docker layer cache silently froze
# "stable"/"latest" at whenever the last rebuild happened. So they live in the
# volumes that already persist their logins, and this script is the one thing
# that writes them:
#
#   entrypoint.sh --if-missing <agent>   install on first use of that agent
#   ./dev.sh update-agents [agent...]    move to the current release
#
# Each agent takes an optional version from the environment, which is how you
# roll back a bad release (see docker/README.md). Unset means the tool's own
# default channel.
set -euo pipefail

ALL_AGENTS="claude codex omp opencode"

if [ -t 2 ]; then
    BOLD=$'\033[1m'; RED=$'\033[31m'; RESET=$'\033[0m'
else
    BOLD=""; RED=""; RESET=""
fi

die() {
    printf '%serror:%s %s\n' "${RED}${BOLD}" "${RESET}" "$*" >&2
    exit 1
}

note() {
    printf '%s==>%s %s\n' "${BOLD}" "${RESET}" "$*" >&2
}

# Where each agent's entry point ends up. Three of the four install into
# ~/.local/bin; opencode has a prefix of its own. Both trees are volumes, and
# both are already on PATH from the Dockerfile.
agent_binary() {
    case "$1" in
        claude)   printf '%s\n' "${HOME}/.local/bin/claude" ;;
        codex)    printf '%s\n' "${HOME}/.local/bin/codex" ;;
        omp)      printf '%s\n' "${HOME}/.local/bin/omp" ;;
        opencode) printf '%s\n' "${HOME}/.opencode/bin/opencode" ;;
        *)        die "unknown agent '$1' (known: ${ALL_AGENTS})" ;;
    esac
}

install_claude() {
    # The musl build of this installer lays down a self-contained executable
    # whose only dynamic dependency is musl libc -- no Node.js runtime.
    # ~/.local/bin/claude is a symlink into ~/.local/share/claude/versions/,
    # so both halves have to be on the same volume. They are.
    #
    # "latest", not the installer's own "stable": stable is the conservative
    # rollout channel and trails by enough to matter -- it sat on 2.1.274 while
    # latest was 2.1.283, and a current model would not run on it. The other
    # three agents track their newest release too, so this also makes the four
    # behave alike. Set CLAUDE_CODE_VERSION=stable to opt back in.
    curl -fsSL https://claude.ai/install.sh \
        | bash -s -- "${CLAUDE_CODE_VERSION:-latest}"
}

install_codex() {
    # An older layout kept this payload in the image at /opt/codex, with
    # entrypoint.sh linking $CODEX_HOME/packages at it -- the only way to stop
    # a volume from pinning a version the image was supposed to own. Nothing in
    # the image owns it now, so the installer gets its real directory back;
    # drop the stale link if this volume predates that change.
    if [ -L "${CODEX_HOME}/packages" ]; then
        rm -f "${CODEX_HOME}/packages"
    fi

    # Fetched to a file rather than piped: every setting below is passed
    # through the environment, and `VAR=x curl ... | sh` puts VAR on curl --
    # the first command of the pipeline -- where the installer never sees it.
    # That silently defeated the version pin for as long as this install
    # existed as a Dockerfile layer.
    local script="${tmpdir}/codex-install.sh"
    curl -fsSL https://chatgpt.com/codex/install.sh -o "${script}"
    CODEX_RELEASE="${CODEX_VERSION:-latest}" \
    CODEX_NON_INTERACTIVE=1 \
        sh "${script}"

    # ~/.local/bin/codex is only a symlink; the ~370 MB payload it points at
    # has to land under $CODEX_HOME, which is a volume. An installer that
    # moves it somewhere unpersisted must fail here rather than reinstall
    # itself on every single start-up.
    case "$(readlink -f "${HOME}/.local/bin/codex")" in
        "${CODEX_HOME}"/*) ;;
        *) die "codex payload landed outside ${CODEX_HOME}; it would not persist" ;;
    esac
}

install_omp() {
    # --binary is not the installer's default, and it is what keeps Bun out of
    # this image: left alone the installer builds from source through Bun, and
    # installs Bun itself to do it. The binary path resolves to the linux-musl
    # release asset. "latest" means the newest release; anything else is passed
    # as --ref, which in binary mode must name an existing release tag and so
    # carries the leading "v".
    local args=(--binary)
    if [ -n "${OMP_VERSION:-}" ] && [ "${OMP_VERSION}" != latest ]; then
        args+=(--ref "${OMP_VERSION}")
    fi
    curl -fsSL https://omp.sh/install | sh -s -- "${args[@]}"
}

install_opencode() {
    # This installer detects musl on its own -- it looks for
    # /etc/alpine-release, then falls back to `ldd --version` -- and fetches
    # the opencode-linux-<arch>-musl asset. It reads its version from VERSION,
    # where unset means the newest release, so that variable is only set when
    # actually pinned. Fetched to a file and run with the environment on bash,
    # for the same reason as codex above.
    local script="${tmpdir}/opencode-install.sh"
    curl -fsSL https://opencode.ai/install -o "${script}"
    if [ -n "${OPENCODE_VERSION:-}" ] && [ "${OPENCODE_VERSION}" != latest ]; then
        VERSION="${OPENCODE_VERSION}" bash "${script}"
    else
        bash "${script}"
    fi
}

# The version each agent was asked for, or empty for its default channel. Used
# only to check that a pin was honoured: none of these installers fails on its
# own when it cannot find the release you named.
requested_version() {
    case "$1" in
        claude)   printf '%s\n' "${CLAUDE_CODE_VERSION:-}" ;;
        codex)    printf '%s\n' "${CODEX_VERSION:-}" ;;
        omp)      printf '%s\n' "${OMP_VERSION:-}" ;;
        opencode) printf '%s\n' "${OPENCODE_VERSION:-}" ;;
    esac
}

install_agent() {
    local agent="$1"
    note "installing ${agent}"
    "install_${agent}"

    # Bash caches command lookups, and for a first install the agent was
    # absent when this script started.
    hash -r

    local binary
    binary="$(agent_binary "${agent}")"
    [ -x "${binary}" ] || die "${agent} installer finished but ${binary} is not executable"

    local version
    version="$("${agent}" --version 2>&1 | head -1)" \
        || die "${agent} installed but will not run: ${version}"

    local want
    want="$(requested_version "${agent}")"
    if [ -n "${want}" ] && [ "${want}" != latest ] && [ "${want}" != stable ]; then
        printf '%s\n' "${version}" | grep -qF "${want}" \
            || die "asked for ${agent} ${want}, got: ${version}"
    fi

    note "${agent}: ${version}"
}

if_missing=0
case "${1:-}" in
    --if-missing) if_missing=1; shift ;;
esac

agents=("$@")
[ ${#agents[@]} -gt 0 ] || read -r -a agents <<<"${ALL_AGENTS}"

tmpdir="$(mktemp -d)"
trap 'rm -rf "${tmpdir}"' EXIT

for agent in "${agents[@]}"; do
    binary="$(agent_binary "${agent}")"
    if [ "${if_missing}" = 1 ] && [ -x "${binary}" ]; then
        continue
    fi
    install_agent "${agent}"
done
