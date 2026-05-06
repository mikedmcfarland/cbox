# ADR 001: Docker + DinD over Tart macOS VMs

## Status
Accepted

## Context
We need an isolation layer for running Claude Code sessions. The two main options are Docker containers (Linux) and Tart macOS VMs. Tart provides full macOS with native tooling support (Hammerspoon, brew, etc.) but is resource-heavy (~4-8GB per VM, minutes to start). Docker is lighter (~200-500MB, seconds to start) but runs Linux only.

## Decision
Use Docker with Docker-in-Docker (DinD) as the primary isolation layer.

## Alternatives considered

**Tart macOS VMs**: Full macOS experience, no tooling gaps. Rejected as default because resource cost is too high for multiple concurrent sessions, and most Claude Code work is terminal/code editing that doesn't require macOS. Could be added as a future backend.

**Docker microVM sandboxes** (Docker's official sandbox product): Strong isolation via microVMs. Rejected because it's too abstracted — we want CLI-native UX and control over the container environment.

**Claude Code native sandbox** (bubblewrap/seatbelt): Near-zero overhead, built into Claude. Rejected as the sole isolation layer because it doesn't provide full filesystem isolation or its own Docker daemon. We do use it as defense-in-depth inside our containers.

**Colima / Lima**: Lightweight Linux VM for Docker on macOS. Not a direct competitor — this is what runs Docker Desktop. Orthogonal to our design.

## Consequences
- Sessions run Linux, not macOS. Personal tooling (editor, shell, aliases) is provisioned via a Dockerfile.
- DinD requires `--privileged` containers.
- Project docker-compose stacks work inside sessions without conflicting with the host Docker daemon.
- Tart remains a viable future addition for cases requiring macOS.
