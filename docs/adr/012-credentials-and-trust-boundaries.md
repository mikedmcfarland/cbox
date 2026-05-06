# ADR 012: Credential handling and trust boundaries

## Status
Accepted (local backend); remote backend portion is **provisional** pending verification — see Open questions.

## Context

cbox sessions need credentials: the Anthropic API key, MCP server tokens (GitHub PAT, etc.), OAuth tokens for OAuth-based MCP servers (Notion, Linear, Slack), and ad-hoc tokens used by tools inside the container (gh, gcloud, scripts).

Several places these can live:

- **Host-side stores**: 1Password (`op://...`), macOS Keychain, env vars on the host shell.
- **Container-side**: env vars at process start, files on the tier volume (`.claude.json`, `~/.claude/.credentials.json`, `~/.config/gh/`, etc.).
- **In-flight**: passed at container start, fetched on demand from the host.

Claude Code on Linux differs from macOS: it does **not** use a system keyring. Per the [auth docs](https://code.claude.com/docs/en/authentication.md):

- Anthropic OAuth account token → `~/.claude/.credentials.json`, plaintext, mode `0600`. Path overridable via `CLAUDE_CONFIG_DIR`.
- `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN` env vars bypass the on-disk file entirely.
- `apiKeyHelper` config — a script Claude Code calls dynamically to fetch the key (analogous to a git credential helper). Bypasses on-disk storage.
- Token-based MCPs (`claude mcp add ... --header`) embed the header in `.claude.json`.
- OAuth-based MCP token storage location is **undocumented**. Likely `.claude.json` or `.credentials.json`; needs verification.

The threat model from the plan is host protection — controlling what credentials and network access a Claude session has. Inter-session isolation within a tier is explicitly not a goal. The trust boundary today is the tier volume on the host.

## Decision

### Local backend trust model

The tier volume is the trust boundary. Anything inside the container is reachable by anyone with access to that volume on the host.

- Tier volume path is `~/.cbox/volumes/<tier>/`, created with mode `0700`, owned by the user.
- The Anthropic API key is injected as `ANTHROPIC_API_KEY` env at container start (resolved on host via `op read`). Claude Code's `.credentials.json` is never written.
- Token-based MCPs are registered via init.d scripts using tier env vars. The resulting header is persisted by Claude Code into `.claude.json` on the tier volume — accepted, since the tier volume is the trust boundary.
- OAuth-based MCPs auth interactively via `cbox auth <tier>`. Tokens persist on the tier volume — accepted for the same reason.
- No host-mounted `~/.claude.json` (per ADR 006).

This is the model for v1 and is sufficient for local-only usage.

### Remote backend trust model (provisional)

Remote backends (GCE, Codespaces) break the local assumption: the tier volume now lives on a disk that isn't fully under user control. Two layers of mitigation:

**1. Don't write the Anthropic credential to remote disk at all.** Use Claude Code's `apiKeyHelper` config pointing at a small script in the container. The script fetches the key from the host on demand:

- Host runs a tiny credential responder (Unix socket).
- `cbox` reverse-forwards that socket into the remote container over SSH (`ssh -R /run/cbox/host-creds.sock:...`).
- `apiKeyHelper` script in the container does `nc -U /run/cbox/host-creds.sock` (or similar) and prints the key on stdout.
- The key is held only in the requesting Claude Code process's memory, never on the remote disk or in the container's persistent env.

**2. Encrypt the remote tier volume at rest, restrict IAM.** MCP tokens are still going to land in `.claude.json` on the remote disk — Claude Code controls that and there's no clean hook to redirect it. Treat the encrypted volume as the trust boundary on remote, equivalent to the local volume on the laptop.

**3. OAuth MCPs on remote are an open question.** Where exactly Claude Code on Linux stores OAuth-acquired MCP tokens is undocumented. Until verified, OAuth-MCP-using tiers should default to the local backend; tiers running remote should stick to token-based MCPs (where we control the token source) or accept the encrypted-disk trust boundary.

### Why not build a general-purpose credential broker

The general "credential broker daemon with a defined protocol" idea was considered and rejected as overengineering for v1:

- For the Anthropic key, `apiKeyHelper` is the official hook — there's nothing to design.
- For MCP tokens, Claude Code controls the storage and offers no helper-style hook. A broker can't plug in.
- For ad-hoc tools (git, gh, gcloud), each already has its own credential mechanism (`git credential` helpers, `gh auth login`, ADC). These can be wired up individually if/when needed; they don't require a unified broker.

What's left is "fetch the Anthropic key from the host on demand," which is just an `apiKeyHelper` script plus a forwarded socket. Calling that a broker dignifies it more than the design warrants.

## Alternatives considered

**Mount `~/.claude.json` or `~/.claude/.credentials.json` from host into containers.** Rejected per ADR 006 — host file contains tokens for MCPs the tier shouldn't see, and references host-only paths.

**Encrypt `.claude.json` at rest with a host-held key, decrypt to tmpfs at session start.** Possible but complex: requires intercepting Claude Code's writes (it expects a real file) and re-encrypting on session end. Defer until there's a concrete remote backend that needs it and disk encryption is judged insufficient.

**Re-auth OAuth MCPs every session (no persistence).** Defeats the persistent-tier-container design (ADR 002). Rejected.

**Keep all credentials in env vars, never on disk.** Works for token-based MCPs and the Anthropic key, but Claude Code's OAuth MCP flow writes tokens to disk itself — we can't force env-only storage without forking. Rejected.

## Consequences

- Plan §"Auth" reflects the local model explicitly: env injection for the Anthropic key, init.d scripts for token MCPs, persistent storage on the tier volume for OAuth MCPs.
- Tier volume permissions are specified (`0700`, user-owned) so the trust boundary is auditable.
- `apiKeyHelper`-over-forwarded-socket is the planned remote story for the Anthropic key. No code in v1; the design is captured here so the local implementation doesn't paint into a corner.
- Remote backend support is gated on resolving the OAuth-MCP-storage open question.
- See also: ADR 002 (per-tier instances), ADR 006 (don't mount `.claude.json`), ADR 011 (backend abstraction).

## Open questions

- **Where does Claude Code on Linux persist OAuth-acquired MCP tokens?** `.claude.json`? `.credentials.json`? A separate per-MCP file? Verify by running an OAuth MCP login (e.g., Notion) inside a Linux container and inspecting the filesystem before/after. Result determines whether remote OAuth MCPs need a separate mitigation or just inherit the encrypted-disk trust boundary.
- **Does `apiKeyHelper` work with `claude` invoked headlessly (e.g., `cbox run`)?** Verify before committing to it as the remote-Anthropic-key strategy.
