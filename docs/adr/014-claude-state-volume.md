# ADR 014: Persist `/home/cbox/.claude` on a per-tier named volume

## Status
Accepted

## Context

Claude Code on Linux accumulates state under `/home/cbox/.claude/`:

- `.claude.json` (preferences, feature flags, registered MCP servers,
  onboarding state, OAuth-MCP tokens).
- `.credentials.json` (Anthropic OAuth token, when used — we currently
  bypass this with `ANTHROPIC_API_KEY`).
- Various MCP-server-specific files.

If this directory lives only on the container's writable layer:

- `cbox build <tier>` (a normal flow — adding a layer, updating the
  base image, bumping `claude-code`) destroys it. The user re-runs
  `cbox auth <tier>` for every OAuth MCP, and every init.d script
  re-registers its MCP from scratch.
- An accidental `cbox tier destroy` (and eventually `cbox tier rm`)
  wipes it without warning.

This was called out as an open question in the v1 plan:

> **Container upgrades**: When rebuilding a tier image, `.claude.json`
> state should survive. Likely a Docker volume at `/home/cbox/.claude/`.

## Decision

Mount a **per-tier Docker named volume** at `/home/cbox/.claude/`:

- Volume name: `cbox-tier-<tier>-claude`.
- Mount target: `/home/cbox/.claude`.
- Auto-created by Docker on first use (named volumes without a
  pre-existing volume are materialised on container start).
- Lives in the Docker volume store (`/var/lib/docker/volumes/` on
  Linux, equivalent for the macOS LinuxKit VM), separate from cbox's
  workspace bind mount at `~/.cbox/workspaces/<tier>/`.

The volume is wired up in `commands::common::build_run_config` so all
tier-instance bring-ups (`cbox <name>`, `cbox run`, `cbox auth`,
`cbox tier resume`) see the same persistent `.claude` directory.

### Per-tier, not shared

One volume per tier — not one volume globally — for the same reason
each tier has its own container, its own credentials, and its own
workspace directory: the trust boundary in v1 is the tier. A shared
`.claude` volume would let the `dev` tier read the `auto` tier's
OAuth tokens just by mounting the same volume; per-tier preserves the
ADR 002 / ADR 012 model.

### Why not a bind mount

A bind mount under `~/.cbox/state/<tier>/.claude/` would also work, and
would let the host inspect `.claude.json` with normal tooling. We
chose the named volume because:

- The contents are container-owned. UIDs/GIDs/permissions are set by
  the in-container Claude Code process; host-side editing or
  permission shifts would corrupt the state. A named volume keeps
  ownership inside the Docker boundary.
- The path is opaque to users by design — `.claude.json` is not a
  config file the user is expected to edit. cbox.yaml + tier
  settings.json are.
- `docker volume inspect` and `docker run --rm -v cbox-tier-dev-claude:/x
  alpine ...` provide adequate escape hatches when inspection or
  surgery is needed.

If a concrete inspection workflow appears (e.g. a `cbox state edit`
verb), revisit.

## Consequences

- MCP tokens registered via init.d scripts survive image rebuilds.
- OAuth MCP tokens obtained via `cbox auth <tier>` survive image
  rebuilds.
- Claude Code's onboarding / first-run UI only fires once per tier
  per host, not once per image build.
- `cbox tier rm` (when it lands) must offer a `--keep-state` /
  `--also-state` choice or document the volume's persistence
  explicitly — otherwise users expect "delete the tier" to actually
  delete it.
- The volume is **part of the trust boundary** (ADR 012). On the
  local backend this is fine — both the volume and the workspace bind
  mount live on the user's machine. On a remote backend the volume
  lives on remote disk, and the same encryption-at-rest expectations
  apply.
- Glossary: "tier state" now has a concrete location — the
  `cbox-tier-<tier>-claude` volume — rather than just describing
  accumulated state abstractly.

## Alternatives considered

- **No persistence** (the v1-pre state). Rejected: rebuilds are a
  normal flow; losing state per rebuild makes MCP setup feel
  ceremonial.
- **One shared `cbox-claude` volume across tiers**. Rejected: breaks
  per-tier isolation; see "Per-tier, not shared" above.
- **Bind mount under `~/.cbox/`**. Rejected for host-side ownership
  collisions; see "Why not a bind mount."
- **Mount the full `/home/cbox/`**. Tempting (covers dotfiles for
  shell history, vim, etc.) but conflates "Claude state" with "shell
  environment." The environment image (ADR layer model) owns
  shell/editor config; mixing them defeats `cbox build`-driven
  reproducibility for everything except `.claude`. Rejected.

## References

- Plan §"Open questions for implementation" (resolved here).
- ADR 002 (per-tier instances — same isolation argument).
- ADR 006 (no `.claude.json` host bind — host file would leak across
  tiers).
- ADR 012 (credentials / trust boundary — `.claude` is part of the
  per-tier trust boundary).
- ADR 013 (init.d delivery — script effects persist here).
