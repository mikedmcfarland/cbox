# ADR 007: User-defined security tiers with native Claude settings.json

## Status
Accepted

## Context
Sessions need different levels of access — some should have no network (autonomous sandbox), others need GitHub read access, others need full write + cloud credentials. The question is how to define these tiers and how to express permissions.

## Decision
Tiers are user-defined named security contexts in `cbox.yaml`. Tool permissions and network sandboxing use Claude Code's native `settings.json` format per tier — not a cbox-specific permission syntax.

## Alternatives considered

**Hardcoded tiers (minimal/standard/full)**: Simpler but inflexible. Users might want one tier or ten, with custom names. Rejected.

**cbox-specific permission syntax**: A cbox.yaml section with `allowed-tools:`, `allowed-domains:`, etc. Rejected because:

- Reinvents Claude Code's existing permission system.
- Users would need to learn a new syntax.
- Would fall behind as Claude Code adds new permission features.
- Config field names would diverge from the underlying flags they control.

**Docker-level network sandboxing (iptables, proxy)**: Implementing domain allowlists via Docker network rules or a squid proxy inside the container. Rejected because Claude Code's bubblewrap sandbox already does this at the OS level — works in `--privileged` containers at full strength. `allowUnsandboxedCommands: false` prevents escape.

## Consequences
- Each tier has a `settings:` field pointing to a standard Claude Code `settings.json`.
- Users reuse existing knowledge — same format as any Claude Code project config.
- Domain allowlists, tool permissions with glob patterns, MCP access control — all native.
- Config field names in `cbox.yaml` match underlying flags: `network:` → `docker run --network`, `dangerously-skip-permissions:` → `claude --dangerously-skip-permissions`.
- Credential definitions are separate from tiers and referenced by name, allowing reuse across tiers.
