#!/usr/bin/env bash
#
# Omega development environment.
#
# Everything needed to build and hack on Omega -- the pinned Rust toolchain,
# cc/as/ld, just, Claude Code, and Codex CLI -- lives in an Alpine container
# defined by docker/Dockerfile. This script is the only entry point you need:
#
#   ./dev.sh              start Claude Code inside the container
#   ./dev.sh codex        start Codex CLI inside the container
#   ./dev.sh shell        interactive shell inside the container
#   ./dev.sh run cargo t  run any command inside the container
#
# See docker/README.md for the full story.
set -euo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_FILE="${REPO_ROOT}/docker/compose.yaml"
SERVICE="dev"

if [ -t 2 ]; then
    BOLD=$'\033[1m'; RED=$'\033[31m'; DIM=$'\033[2m'; RESET=$'\033[0m'
else
    BOLD=""; RED=""; DIM=""; RESET=""
fi

die() {
    printf '%serror:%s %s\n' "${RED}${BOLD}" "${RESET}" "$*" >&2
    exit 1
}

note() {
    printf '%s==>%s %s\n' "${BOLD}" "${RESET}" "$*" >&2
}

usage() {
    cat <<EOF
${BOLD}Omega development environment${RESET}

  ${BOLD}./dev.sh${RESET} [command] [args...]

${BOLD}Commands${RESET}
  claude [args...]   Start Claude Code in the container. ${DIM}(default)${RESET}
  codex [args...]    Start Codex CLI in the container.
  shell              Open an interactive bash shell in the container.
  run <cmd...>       Run one command in the container, e.g.
                     ${DIM}./dev.sh run cargo test${RESET}
                     ${DIM}./dev.sh run just build-exe${RESET}
  build              Build the image if it is missing or out of date.
  rebuild            Rebuild the image from scratch, pulling a fresh base
                     image. Use this to pick up a new Claude Code release.
  down               Remove any leftover containers (volumes are kept).
  clean              Remove containers ${BOLD}and all volumes${RESET}: build cache,
                     cargo cache, shell history and your Claude Code login.
  help               Show this message.

${BOLD}Version pins${RESET} ${DIM}(override via the environment, then rebuild)${RESET}
  ALPINE_VERSION        ${ALPINE_VERSION:-3.23}
  RUST_VERSION          ${RUST_VERSION:-1.94.1}
  CLAUDE_CODE_VERSION   ${CLAUDE_CODE_VERSION:-stable}
  CODEX_VERSION         ${CODEX_VERSION:-latest}

  ${DIM}e.g. RUST_VERSION=1.95.0 ./dev.sh rebuild${RESET}

The repo is bind-mounted at /workspace, so edits inside and outside the
container are the same files. Build output goes to a Docker volume instead of
the host's target/, so container (musl) and host (glibc) builds never collide.
EOF
}

# `help` is answered before any environment check, so it still works on a
# machine that has not installed Docker yet.
command="claude"
case "${1:-}" in
    "")             ;;                       # bare ./dev.sh -> claude
    help|-h|--help) usage; exit 0 ;;
    -*)             ;;                       # ./dev.sh --flag -> claude --flag
    *)              command="$1"; shift ;;
esac

[ -f "${COMPOSE_FILE}" ] || die "missing ${COMPOSE_FILE} -- run this script from inside the Omega repo."

command -v docker >/dev/null 2>&1 \
    || die "docker is not installed. See https://docs.docker.com/get-docker/"

if docker compose version >/dev/null 2>&1; then
    compose() { docker compose -f "${COMPOSE_FILE}" "$@"; }
elif command -v docker-compose >/dev/null 2>&1; then
    compose() { docker-compose -f "${COMPOSE_FILE}" "$@"; }
else
    die "docker compose is not available. Install the Compose plugin: https://docs.docker.com/compose/install/"
fi

docker info >/dev/null 2>&1 \
    || die "cannot reach the Docker daemon. Is it running, and is your user in the 'docker' group?"

# Build the image with the host's uid/gid so files created in the mounted repo
# come out owned by you. (HOST_UID rather than UID: bash marks UID readonly.)
HOST_UID="$(id -u)"; export HOST_UID
HOST_GID="$(id -g)"; export HOST_GID

# Carry the host's git identity in, so commits made in the container are
# attributed correctly. Explicit env wins over the host's git config.
GIT_USER_NAME="${GIT_USER_NAME:-$(git -C "${REPO_ROOT}" config --get user.name 2>/dev/null || true)}"
GIT_USER_EMAIL="${GIT_USER_EMAIL:-$(git -C "${REPO_ROOT}" config --get user.email 2>/dev/null || true)}"
export GIT_USER_NAME GIT_USER_EMAIL

case "${command}" in
    claude)
        # `run` builds the image on first use, and --rm keeps things tidy:
        # all state that should survive lives in the named volumes.
        compose run --rm "${SERVICE}" claude "$@"
        ;;
    codex)
        compose run --rm "${SERVICE}" codex "$@"
        ;;
    shell|sh|bash)
        compose run --rm "${SERVICE}" bash "$@"
        ;;
    run|exec)
        [ $# -gt 0 ] || die "'run' needs a command, e.g. ./dev.sh run cargo test"
        compose run --rm "${SERVICE}" "$@"
        ;;
    build)
        compose build "$@"
        ;;
    rebuild)
        note "rebuilding from scratch (this re-downloads the Rust toolchain)"
        compose build --no-cache --pull "$@"
        ;;
    down|stop)
        compose down --remove-orphans
        ;;
    clean)
        printf 'This deletes the cargo cache, container build output, shell history\n'
        printf 'and your Claude Code login for this project. Continue? [y/N] '
        read -r reply || reply=""
        case "${reply}" in
            [yY]|[yY][eE][sS]) compose down --volumes --remove-orphans ;;
            *) note "aborted" ;;
        esac
        ;;
    *)
        die "unknown command '${command}'. Try ./dev.sh help"
        ;;
esac
