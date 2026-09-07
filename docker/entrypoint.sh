#!/usr/bin/env bash
# Container entrypoint: reconcile the few things that can only be known at run
# time, then hand over to the requested command.
set -euo pipefail

# The repo is bind-mounted from the host. If the host uid does not match the
# container user (the image is built with your uid, so it normally does), git
# would otherwise refuse to operate on a "dubious ownership" repository.
git config --global --add safe.directory /workspace 2>/dev/null || true

# Carry the host's git identity in, so commits made inside the container are
# not authored by "dev@<container id>". Re-applied on every start because the
# container's home directory is not persisted -- only the volumes are.
if [ -n "${GIT_USER_NAME:-}" ]; then
    git config --global user.name "${GIT_USER_NAME}"
fi
if [ -n "${GIT_USER_EMAIL:-}" ]; then
    git config --global user.email "${GIT_USER_EMAIL}"
fi

# Codex keeps its binary payload under $CODEX_HOME/packages, but $CODEX_HOME
# is a named volume and Docker seeds one only while it is empty -- so a volume
# created by an older image would keep serving that old codex no matter what
# `./dev.sh rebuild` installs. The image owns the payload instead (see
# OMEGA_CODEX_PACKAGES in docker/Dockerfile); point the volume at it, which
# both fixes the pin and drops the stale copy an existing volume still holds.
# Only the binaries are touched here: credentials, sessions, skills and
# plugins are elsewhere under $CODEX_HOME and are left alone.
if [ -n "${CODEX_HOME:-}" ] && [ -d "${OMEGA_CODEX_PACKAGES:-}/packages" ]; then
    if [ ! -L "${CODEX_HOME}/packages" ]; then
        rm -rf "${CODEX_HOME}/packages"
    fi
    ln -sfn "${OMEGA_CODEX_PACKAGES}/packages" "${CODEX_HOME}/packages"
fi

exec "$@"
