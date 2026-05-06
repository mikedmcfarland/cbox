# ADR 006: Don't mount ~/.claude.json from host

## Status
Accepted

## Context
`~/.claude.json` on the host contains MCP server configs (with embedded tokens like GitHub PATs), Claude Code preferences, OAuth account state, feature flags, and project-specific data. Mounting it into containers would provide a familiar environment but has security and correctness issues.

## Decision
Don't mount `~/.claude.json`. Each tier instance maintains its own `.claude.json` that accumulates state naturally. MCP servers are registered via init.d scripts using injected credentials.

## Alternatives considered

**Mount ~/.claude.json read-only**: Provides all preferences and MCP configs immediately. Rejected because:

- Contains embedded secrets (e.g., GitHub PAT in MCP header config). A prompt injection could read the file and exfiltrate tokens, even if the tier's settings.json blocks those MCP tools.
- All-or-nothing — you give every MCP config to every tier, defeating per-tier credential scoping.
- References host-only binaries for stdio MCPs (e.g., `gt mcp`, `npx` paths that don't exist in the container).
- Per-project state includes host-specific paths.

**Mount a filtered version per tier**: Generate a subset of `.claude.json` with only the relevant MCPs and no secrets. Rejected as too complex — the file format isn't designed for partial extraction, and it's fragile as Claude Code evolves.

## Consequences
- Token-based MCPs (GitHub) are registered via init.d scripts using tier env vars — clean per-tier scoping.
- OAuth-based MCPs (Notion, Linear) auth interactively in the persistent tier instance — one-time `cbox auth <tier>`.
- Preferences (editor mode, etc.) are set once per tier instance and persist.
- Feature flags will be fetched fresh by Claude Code on first run — may differ from host until cache populates.
