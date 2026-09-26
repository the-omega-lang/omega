#!/usr/bin/env bash
#
# Omega development environment.
#
# Everything needed to build and hack on Omega -- the pinned Rust toolchain,
# cc/as/ld, just, Claude Code, Codex CLI, omp and opencode -- lives in an
# Alpine container defined by docker/Dockerfile. This script is the only entry
# point you need:
#
#   ./dev.sh              start Claude Code inside the container
#   ./dev.sh codex        start Codex CLI inside the container
#   ./dev.sh omp          start omp (oh-my-pi) inside the container
#   ./dev.sh opencode     start opencode inside the container
#   ./dev.sh shell        interactive shell inside the container
#   ./dev.sh run cargo t  run any command inside the container
#   ./dev.sh update-agents  update the agent CLIs (no image rebuild)
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
  omp [args...]      Start omp (oh-my-pi) in the container.
  opencode [args...] Start opencode in the container.
  shell              Open an interactive bash shell in the container.
  run <cmd...>       Run one command in the container, e.g.
                     ${DIM}./dev.sh run cargo test${RESET}
                     ${DIM}./dev.sh run just build-exe${RESET}
  update-agents [agent...]
                     Update the agent CLIs to their current release, or just
                     the ones named. They live in volumes, not in the image,
                     so this needs no rebuild. ${DIM}(installs happen on first
                     use anyway; this is how you move an already-installed
                     one forward, or roll it back with a version below)${RESET}
  build              Build the image if it is missing or out of date.
  rebuild            Rebuild the image from scratch, pulling a fresh base
                     image. This is the Rust/LLVM toolchain only; for the
                     agents use ${BOLD}update-agents${RESET}.
  down               Remove any leftover containers (volumes are kept).
  clean              Remove containers ${BOLD}and all volumes${RESET}: build cache,
                     cargo cache, shell history, the agents and their logins.
  help               Show this message.

${BOLD}Resource limits${RESET} ${DIM}(override via the environment)${RESET}
  OMEGA_CPU_PERCENT     ${OMEGA_CPU_PERCENT:-50}
                        Share of the machine's CPUs the container may use, so
                        a busy agent leaves the host responsive. Set it to
                        ${DIM}off${RESET} for no limit.

  ${DIM}e.g. OMEGA_CPU_PERCENT=50 ./dev.sh${RESET}

${BOLD}Toolchain pins${RESET} ${DIM}(override via the environment, then rebuild)${RESET}
  ALPINE_VERSION        ${ALPINE_VERSION:-3.23}
  RUST_VERSION          ${RUST_VERSION:-1.94.1}
  LLVM_VERSION          ${LLVM_VERSION:-21}

  ${DIM}e.g. RUST_VERSION=1.95.0 ./dev.sh rebuild${RESET}

${BOLD}Agent versions${RESET} ${DIM}(override via the environment, then update-agents)${RESET}
  CLAUDE_CODE_VERSION   ${CLAUDE_CODE_VERSION:-latest (newest release)}
  CODEX_VERSION         ${CODEX_VERSION:-latest (default channel)}
  OMP_VERSION           ${OMP_VERSION:-latest (default channel)}
  OPENCODE_VERSION      ${OPENCODE_VERSION:-latest (default channel)}

  ${DIM}Unset means the newest release. Set one to hold or roll back, e.g.
  CLAUDE_CODE_VERSION=2.1.220 ./dev.sh update-agents claude${RESET}

The repo is bind-mounted at /workspace, so edits inside and outside the
container are the same files. Build output goes to a Docker volume instead of
the host's target/, so container (musl) and host (glibc) builds never collide.
The agents live in volumes too, so they update independently of the image --
and the first run of one installs it.
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

# Cap how much of the machine a session may take: an agent running a full
# build should not leave the host unusable. The knob is a percentage, which is
# how you think about it; compose wants an absolute number of cores.
cpu_percent="${OMEGA_CPU_PERCENT:-50}"
case "${cpu_percent}" in
    off|none|unlimited)
        OMEGA_CPUS=0        # 0 means "no limit" to the Docker daemon.
        ;;
    *)
        [[ "${cpu_percent}" =~ ^([0-9]+|[0-9]*\.[0-9]+)$ ]] \
            || die "OMEGA_CPU_PERCENT must be a number or 'off' (got '${cpu_percent}')"

        # Ask the daemon, not the host: on macOS and Windows the containers run
        # in a VM that was given only part of the machine.
        host_cpus="$(docker info --format '{{.NCPU}}' 2>/dev/null || true)"
        case "${host_cpus}" in
            ''|0|*[!0-9]*) host_cpus="$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1)" ;;
        esac

        # Docker's own floor for a cpu limit is 0.01, so never round below it.
        OMEGA_CPUS="$(awk -v n="${host_cpus}" -v p="${cpu_percent}" 'BEGIN {
            if (p <= 0 || p > 100) exit 1
            cpus = n * p / 100
            printf "%.2f", (cpus < 0.01 ? 0.01 : cpus)
        }')" || die "OMEGA_CPU_PERCENT must be between 0 and 100, or 'off' (got '${cpu_percent}')"
        ;;
esac
export OMEGA_CPUS

case "${command}" in
    claude)
        # `run` builds the image on first use, and --rm keeps things tidy:
        # all state that should survive lives in the named volumes.
        compose run --rm "${SERVICE}" claude "$@"
        ;;
    codex)
        compose run --rm "${SERVICE}" codex "$@"
        ;;
    omp)
        compose run --rm "${SERVICE}" omp "$@"
        ;;
    opencode|oc)
        compose run --rm "${SERVICE}" opencode "$@"
        ;;
    shell|sh|bash)
        compose run --rm "${SERVICE}" bash "$@"
        ;;
    run|exec)
        [ $# -gt 0 ] || die "'run' needs a command, e.g. ./dev.sh run cargo test"
        compose run --rm "${SERVICE}" "$@"
        ;;
    update-agents|update)
        compose run --rm "${SERVICE}" \
            bash /workspace/docker/install-agents.sh "$@"
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
        printf 'This deletes the cargo cache, container build output, shell history,\n'
        printf 'the four agent CLIs and your Claude Code, Codex, omp and opencode\n'
        printf 'logins for this project. The agents reinstall on next use.\n'
        printf 'Continue? [y/N] '
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
