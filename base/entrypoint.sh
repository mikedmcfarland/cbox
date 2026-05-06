#!/usr/bin/env bash
# cbox base entrypoint.
#
# Order of operations (plan.md §Base image):
#   1. Generate sshd host keys on first boot.
#   2. Ensure /run/cbox exists and is owned by the cbox user.
#   3. Run /cbox/init.d/*.sh (idempotent setup: MCP registration, etc.)
#   4. exec into supervisord (or whatever was passed as CMD), which keeps
#      dockerd + sshd alive for the lifetime of the container.
#
# dockerd itself is started by supervisord, not here. That keeps the
# "supervised by PID 1" invariant — if dockerd dies it gets restarted.

set -euo pipefail

# Layers (environment, language layers) typically end with `USER cbox`
# so subsequent COPY/RUN don't accumulate root-owned files. supervisord
# needs root to manage processes and bind sshd on :22, so re-exec via
# sudo if we landed here as a non-root user. cbox has NOPASSWD sudo
# (see base/Dockerfile).
if [ "$(id -u)" -ne 0 ]; then
    exec sudo -E /usr/local/bin/cbox-entrypoint "$@"
fi

# 1. sshd host keys
if [ ! -f /etc/ssh/ssh_host_ed25519_key ]; then
    ssh-keygen -A >/dev/null
fi

# 2. Session socket directory. Idempotent — Dockerfile creates it but a
# bind-mount or volume could shadow it.
mkdir -p /run/cbox
chown cbox:cbox /run/cbox
chmod 0775 /run/cbox

# 3. Run init scripts. Sorted, *.sh only, fail fast on errors.
if [ -d /cbox/init.d ]; then
    for script in /cbox/init.d/*.sh; do
        [ -e "$script" ] || continue
        echo "cbox: running $script" >&2
        # shellcheck disable=SC1090
        bash "$script"
    done
fi

# 4. Hand off to CMD (supervisord by default).
exec "$@"
