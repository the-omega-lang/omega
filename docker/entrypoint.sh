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

exec "$@"
