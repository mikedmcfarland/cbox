# ADR 015: In-repo project config (`.cbox/cbox.yaml`) with two-source merge

## Status
Accepted

## Context

We want two patterns to coexist:

1. **Project-owned cbox content.** A Rust project (cbox itself, for
   example) has a `rust` layer and a `cbox-dev` tier description that
   are obvious project assets — they belong in the repo, versioned with
   the code, working for anyone who clones it. A single global config
   file means the project's dev environment isn't actually checked in.
2. **Personal customization of project content.** A user who has their
   own `rust` layer (with extra tooling, dotfiles, sccache, whatever)
   wants to substitute it for the project's `rust` layer without
   forking the project's cbox.yaml.

These pull in opposite directions: projects want to *define*, users
want to *customize*. A single-source config can't express both.

The OAuth-on-the-`.claude`-volume capability from ADR 014 puts further
pressure here: the natural `cbox-dev` tier definition lists *no*
credentials (OAuth covers Anthropic auth via the named volume), and
that minimal, code-free definition is exactly the kind of thing that
ought to live next to the code.

## Decision

cbox loads up to **two config sources** and merges them.

### Sources

- **Personal source**: `$CBOX_CONFIG` if set; otherwise
  `$HOME/.config/cbox/cbox.yaml` if it exists.
- **Project source**: starting from `current_dir()`, walk up looking
  for a `.cbox/cbox.yaml`. **Deepest match wins** (single project
  source — not multi-source). Walk halts at filesystem root.

If neither source exists, cbox errors as today.

The walk has no special-case for `$HOME` or `.git`. The `.cbox/`
directory name is the opt-in signal. A `~/.cbox/cbox.yaml` is picked
up if present; users who don't want that don't create it.

### Per-source schema relaxations

Per-source configs are parsed individually; cross-references close
only after merge. To allow that:

- `environment` becomes `Option<ExpandedPath>` in the per-source
  struct (it's required *after* merge, not per source).
- `Config::validate()` runs only on the merged Config, not per source.
  This permits a project tier to reference a personal layer (and vice
  versa) — once merged, layers are a single namespace.

### Merge rules

| Field | Rule | Rationale |
|---|---|---|
| `layers` | Map union; **personal wins** on conflict. | Layers are content I run on my machine. The user retains final say. |
| `environment` | **Personal wins** if set; else inherit project's. Required post-merge. | Same principle as `layers` — the base shell/dotfiles image is content the user runs. Project's `environment` acts as the first-time-user fallback so a fresh clone with no personal config still builds. |
| `tiers` | Map union; **project wins** on conflict. | Project authors define how tiers compose. |
| `projects` | Map union; **project wins** on conflict. | A project's own repo+tier binding is authoritative inside that project. |
| `credentials` | Map union; **project wins** on conflict. | Credential entries bundle name + source; splitting them out for finer-grained merge is a separate concern. Project-wins for now; revisit if/when credential composition becomes painful. |
| `default_tier` | Project wins if set; else inherit personal. | Entering a project's context implies its defaults. |
| `default_layers` | **Set-union** — project first, personal appended, dedup'd preserving first-seen order. | Lets personal contribute a permanent `dotfiles`-style layer that applies on top of any project's defaults. |

The principle: **project wins on *composition* (which tiers exist,
how they're assembled, what credentials they expect, what the project's
defaults are); personal wins on *content* (the actual layer and
environment Dockerfiles that run on the user's machine).**

**No shadow warning** is emitted. Conflicts are intentional — the
personal-wins inversion on `layers` exists precisely to support
shadowing — and stderr noise on a working-as-designed feature
trains users to ignore it.

### Path resolution timing

Per the existing convention (src/config.rs:239-251), relative paths
in `layers.*`, `environment`, and credential mount sources are
resolved against the cbox.yaml's parent dir at parse time. This ADR
**extends that pass to cover `projects.*.repo`** so an in-repo config
can express:

```yaml
projects:
  cbox:
    repo: .          # resolved to the repo root, on any machine
    tier: cbox-dev
```

Detection: a `repo:` string starting with `.`, `..`, `~`, or `/` is
treated as a path (tilde-expanded, then canonicalized against the
config file's parent). Anything else (`git@…`, `https://…`,
`ssh://…`, bare `foo`) is left verbatim — `git clone` handles those.

After parse-time resolution, every `repo:` is either an absolute
filesystem path or a non-path URL string. `workspace.rs` needs no
changes — `git clone` accepts absolute local paths.

## Consequences

- **Project-checked-in dev environments.** A repo can ship a
  `.cbox/cbox.yaml` (plus any `.cbox/layers/` and
  `.cbox/tiers/<tier>/settings.json`) that anyone with cbox can use,
  with no personal-config prerequisite.
- **Customization without forking.** A user defining their own
  `rust` layer (or their own `environment` Dockerfile) in personal
  config automatically substitutes it into any project tier that
  references them. The "personal wins on content" inversion is
  asymmetric with the rest of the merge, but principled: tiers are
  the project's composition recipe; layers and environment are
  user-machine content.
- **Personal additions still apply on top.** Set-union on
  `default_layers` means a personal `dotfiles` layer (or any other
  personal layer added to `default_layers`) applies to every tier —
  including project-defined ones like `cbox-dev` — without needing
  to shadow anything.
- **`.cbox/` becomes load-bearing vocabulary.** Glossary picks up
  `project config`, `personal config`, `.cbox/`.
- **Validation surface grows post-merge.** Cross-source dangling
  references (a tier from one source pointing at a layer from the
  other that doesn't exist) now error post-merge with the same
  message as today.
- **Cache lifecycle for cargo (and other layer-managed state) is
  unchanged by this ADR.** The `.claude` named volume (ADR 014)
  remains the only cbox-owned per-tier volume. Cargo cache lives in
  the container writable layer; survives `tier stop/resume`,
  `cleanup`, and `cbox build` (build doesn't touch existing
  containers); dies on `backend.destroy` + recreate. A follow-up ADR
  will introduce **layer-declared named volumes** so a layer's
  Dockerfile (or sidecar) can declare its own cache mounts and the
  `rust` layer can opt into `/home/cbox/.cargo` persistence.

## Alternatives considered

- **Single config, with `include:` directives.** Less invasive but
  pushes complexity into the YAML and conflates "I want to extend"
  with "this content lives in this project." Project-checked-in
  configs without an `include:` from a personal file would still need
  the user to opt in once. Rejected.
- **Multi-source walk** (collect every `.cbox/cbox.yaml` ancestor and
  merge all). Tempting for layered org/workspace/project setups but
  the mental model gets fuzzy fast ("which `.cbox/` contributed
  which layer?"). Deepest-wins keeps two sources, two names, two
  semantics. Rejected for v1; revisit if real demand appears.
- **Uniform project-wins for `layers`.** Consistent with the other
  maps but kills the customization story — a user couldn't tweak a
  single layer without copying the whole project config. Rejected.
- **Project source defaults to filesystem walk; personal source
  removed entirely.** Forces every install to have a `.cbox/` in
  some ancestor (e.g., `~/.cbox/cbox.yaml`). Workable but pushes
  personal preference into a "project" slot, muddying the vocabulary.
  Rejected.
- **Shadow warning on every conflict.** Considered; rejected because
  the most common shadow (personal `layers.*` overriding project's)
  is exactly the intended use case. Warning would be noise on a
  feature working as designed.

## References

- ADR 002 (per-tier instances — the trust-boundary model that scopes
  what "personal vs project" can mean).
- ADR 011 (backend abstraction — `Config` is the schema fed to the
  backend; merge happens at the same boundary).
- ADR 014 (claude state volume — the OAuth-without-Anthropic-key flow
  this enables).
- src/config.rs `resolve` pass (the path-resolution pass extended
  here for `repo:`).
