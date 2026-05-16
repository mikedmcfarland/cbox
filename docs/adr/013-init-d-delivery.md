# ADR 013: init.d scripts ship inside layers

## Status
Accepted

## Context

cbox needs a way to register MCP servers (and run other one-time
container setup) at tier-instance startup. The base image already
provides the mechanism: `/usr/local/bin/cbox-init` is run as a
supervisord one-shot after dockerd is reachable, and it executes every
`*.sh` under `/cbox/init.d/`. The open question (plan §"Open questions
for implementation") was *how the scripts get there*:

1. **Baked into layers**: layer Dockerfiles `COPY` an `init.d/`
   subdirectory into `/cbox/init.d/`.
2. **Mounted at runtime**: cbox bind-mounts a host directory into
   `/cbox/init.d/` per-tier.
3. **Both**: bake the defaults, allow a runtime override.

Each MCP server has natural layer affinity — the GitHub MCP needs the
`claude` CLI, the Python MCP would need the python layer, and so on.
Bundling the script with the thing it depends on keeps the layer
self-contained.

## Decision

**Bake init.d scripts into layers.**

Convention: any cbox layer directory may contain an `init.d/`
subdirectory; the layer's Dockerfile is expected to

```dockerfile
COPY init.d/ /cbox/init.d/
RUN chmod 0755 /cbox/init.d/*.sh
```

The build pipeline already passes each layer directory verbatim as a
Docker build context (see `build.rs::pack_context`), so no code change
in cbox proper is required — the convention is a layer-author contract.

Scripts must remain **idempotent**. `cbox-init` re-runs every script on
every container start; an `init.d/10-mcp-github.sh` that calls
`claude mcp add github ...` unconditionally would either fail on the
second start (server already registered) or hammer Claude's config
with duplicates. The convention is: check, short-circuit, then act.

## Why not mount at runtime

- **Discoverability** suffers: the script delivering an MCP setup
  isn't visible from `cbox.yaml` or the layer alone — you have to know
  about a parallel host directory.
- **Tier-image reproducibility** suffers: `cbox build dev` no longer
  produces a self-contained image; equivalent containers on two hosts
  would behave differently if the runtime mount diverged.
- **Layer affinity is lost**: an MCP script that depends on a
  language toolchain lives next to the bind mount, not next to the
  layer that installs the toolchain. Refactors get harder.
- **No identified use case** for divergent runtime scripts within a
  tier — the runtime knobs that vary per host (tokens, paths) are
  already handled by env credentials and mount credentials. The
  script logic is the same; only its inputs vary.

## Why not both

Two mechanisms means two failure modes and two doc surfaces. The bake
path covers everything the mount path could (write the script as a
layer file), with one fewer concept to teach. If a future need for
runtime overrides appears, this ADR can be revisited.

## Consequences

- `examples/full-setup/layers/claude/init.d/10-mcp-github.sh` is the
  canonical example: it checks `$GITHUB_TOKEN`, short-circuits if the
  `github` server is already registered, otherwise calls
  `claude mcp add`.
- Layer authors own MCP script delivery for the things their layer
  installs. The cbox CLI does not enumerate, list, or validate
  init.d scripts.
- MCP-server state (the resulting `.claude.json` entry, OAuth tokens)
  lives on the per-tier `.claude` named volume (ADR 014), so even
  though scripts re-run on every container start, their effects
  persist across image rebuilds.
- Removing an MCP script from a layer doesn't unregister the MCP —
  you also need `claude mcp remove ...` inside the tier (or destroy
  the `.claude` volume). Documented in the example script.

## Alternatives considered

- **Single global host directory** (e.g. `~/.config/cbox/init.d/`):
  loses tier-specificity, applies to all tiers indiscriminately.
- **`init.d:` field on tier config**: would let one tier opt into a
  layer-shipped script and another opt out. Rejected for v1: layer
  scripts are scoped narrowly enough (each gated on its credential
  env var, e.g. the GitHub script no-ops without `$GITHUB_TOKEN`)
  that simple presence is sufficient. Re-evaluate if scripts get
  expensive.

## References

- Plan §"Auth" (token-based MCP example).
- Plan §"Open questions for implementation" (resolved here).
- ADR 003 (plain Dockerfiles, not YAML).
- ADR 014 (per-tier `.claude` named volume — where script effects
  persist).
