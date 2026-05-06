# ADR 002: One container per tier, not per session

## Status
Accepted

## Context
The original design created a new Docker container for each Claude Code session. This provided strong inter-session isolation but had significant downsides around state management and resource usage.

## Decision
Run one long-lived tier instance per tier. Sessions are separate dtach (interactive) or tmux (autonomous) sockets inside the tier instance — see ADR 005.

## Alternatives considered

**Per-session containers**: Each `cbox <name>` would create a fresh container. Provides full isolation between sessions. Rejected because:

- **Claude Code state loss**: `~/.claude.json` contains feature flags, preferences, editor mode, OAuth tokens, onboarding state, and accumulated usage data. A fresh container loses all of this every time.
- **OAuth re-auth**: MCP servers using OAuth (Notion, Linear, Slack) require a browser auth flow. In ephemeral containers this means re-authing every session, or building complex token extraction/mounting.
- **Resource cost**: Each container runs its own Docker daemon (DinD), sshd, and supervisord. Multiple concurrent sessions on the same project multiply this overhead.
- **Unnecessary isolation**: The actual threat model is protecting the host machine, not isolating sessions from each other. Sessions at the same trust level share the same credentials and permissions anyway.

**Hybrid (configurable per-session or per-tier)**: Adds complexity for a use case that hasn't materialized. Deferred.

## Consequences
- Sessions in the same tier can see each other's processes and files (mitigated by separate git workspaces).
- Shared DinD means docker-compose stacks need `COMPOSE_PROJECT_NAME` and dynamic port publishing to avoid conflicts.
- `.claude.json` accumulates state naturally — no mounting, no auth volumes, no token extraction.
- OAuth MCPs work after a one-time `cbox auth <tier>` setup.
- Container pause/resume is simple since there are few containers.
