#!/usr/bin/env bash
# cbox base entrypoint.
#
# Order of operations (plan.md §Base image):
#   1. Materialise /home/cbox/.ssh/authorized_keys from $CBOX_AUTHORIZED_KEYS
#      (set by cbox at container create time) — done *before* the sudo
#      re-exec because sudo's default env_keep strips the env var.
#   2. Generate sshd host keys on first boot.
#   3. Ensure /run/cbox exists and is owned by the cbox user.
#   4. exec into supervisord (or whatever was passed as CMD), which keeps
#      dockerd + sshd alive for the lifetime of the container and runs
#      /cbox/init.d/*.sh as a one-shot once dockerd is reachable.
#
# dockerd itself is started by supervisord, not here. That keeps the
# "supervised by PID 1" invariant — if dockerd dies it gets restarted.
# init.d scripts live in the cbox-init supervisord program so they can
# depend on dockerd being up (some scripts will run `docker ...`).

set -euo pipefail

# authorized_keys must be written *before* the sudo re-exec: sudo's
# default env_keep policy strips arbitrary env vars even with -E, so the
# env var doesn't survive the re-exec. cbox owns /home/cbox/.ssh, so it
# can write authorized_keys without root anyway.
if [ -n "${CBOX_AUTHORIZED_KEYS:-}" ]; then
    install -d -m 0700 "/home/$(id -un)/.ssh"
    printf '%s\n' "$CBOX_AUTHORIZED_KEYS" > "/home/$(id -un)/.ssh/authorized_keys"
    chmod 0600 "/home/$(id -un)/.ssh/authorized_keys"
fi
unset CBOX_AUTHORIZED_KEYS

# Layers (environment, language layers) typically end with `USER cbox`
# so subsequent COPY/RUN don't accumulate root-owned files. supervisord
# needs root to manage processes and bind sshd on :22, so re-exec via
# sudo if we landed here as a non-root user. cbox has NOPASSWD sudo
# (see base/Dockerfile).
if [ "$(id -u)" -ne 0 ]; then
    exec sudo /usr/local/bin/cbox-entrypoint "$@"
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

# 3. Hand off to CMD (supervisord by default). supervisord runs dockerd,
# sshd, and the cbox-init one-shot (which gates /cbox/init.d/*.sh on
# dockerd readiness).
exec "$@"
