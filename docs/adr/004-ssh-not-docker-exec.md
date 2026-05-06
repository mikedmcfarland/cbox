# ADR 004: SSH into containers, not docker exec

## Status
Accepted

## Context
Sessions need a way to connect to the tier instance for interactive use. The two main options are `docker exec` and SSH.

## Decision
Containers run sshd. `cbox attach` connects via SSH.

## Alternatives considered

**docker exec**: Simpler setup — no sshd needed. Rejected because:

- Only works locally (requires Docker socket access on the calling machine).
- tmux sessions started via `docker exec` die when the exec process disconnects — no persistent sessions.
- No VSCode Remote-SSH, no `scp`, no standard SSH tooling.
- Future remote orchestration would require a different access method anyway.

## Consequences
- Containers include sshd in the base image. SSH keys are injected at build or runtime.
- tmux runs inside the container and survives detach — reconnect anytime.
- `cbox ssh-config` auto-manages `~/.ssh/cbox_hosts` so `ssh cbox-<tier>` works for external tools (VSCode Remote-SSH, scp).
- Users add `Include cbox_hosts` to `~/.ssh/config` once.
- Remote access is possible in the future without design changes. See ADR 011 for the backend abstraction that enables this.
