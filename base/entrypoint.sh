#!/usr/bin/env bash
# cbox base entrypoint.
#
# Order of operations (plan.md §Base image):
#   1. Generate sshd host keys on first boot.
#   2. Ensure /run/cbox exists and is owned by the cbox user.
#   3. Materialise /home/cbox/.ssh/authorized_keys from $CBOX_AUTHORIZED_KEYS
#      (set by cbox at container create time).
#   4. exec into supervisord (or whatever was passed as CMD), which keeps
#      dockerd + sshd alive for the lifetime of the container and runs
#      /cbox/init.d/*.sh as a one-shot once dockerd is reachable.
#
# dockerd itself is started by supervisord, not here. That keeps the
# "supervised by PID 1" invariant — if dockerd dies it gets restarted.
# init.d scripts live in the cbox-init supervisord program so they can
# depend on dockerd being up (some scripts will run `docker ...`).

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

# 3. authorized_keys for the cbox user. Written from $CBOX_AUTHORIZED_KEYS,
# then unset before exec so it doesn't linger in supervisord's environment
# (visible to anyone with sudo inside the container).
if [ -n "${CBOX_AUTHORIZED_KEYS:-}" ]; then
    install -d -m 0700 -o cbox -g cbox /home/cbox/.ssh
    printf '%s\n' "$CBOX_AUTHORIZED_KEYS" > /home/cbox/.ssh/authorized_keys
    chown cbox:cbox /home/cbox/.ssh/authorized_keys
    chmod 0600 /home/cbox/.ssh/authorized_keys
fi
unset CBOX_AUTHORIZED_KEYS

# 4. Hand off to CMD (supervisord by default). supervisord runs dockerd,
# sshd, and the cbox-init one-shot (which gates /cbox/init.d/*.sh on
# dockerd readiness).
exec "$@"
