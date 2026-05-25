# Glossary

Canonical vocabulary for cbox. Organized by family. ADR references point
to deeper rationale.

## Core hierarchy

cbox forms a four-level runtime hierarchy:

    backend  >  tier  >  session  >  workspace

Layers, environment, credentials, and settings are *tier inputs* — they
compose into a tier rather than appearing in the runtime tree. Projects
are cbox.yaml shorthand for `(repo, tier)`.

## Backend

| Term | Definition |
|---|---|
| **backend** | Named compute provider for tier instances. Declared in cbox.yaml's `backends:` map, referenced by tiers via `backend:`. Owns compute lifecycle and SSH connectivity; doesn't own image building, sessions, credentials, or workspaces. See ADR 011. |
| **backend name** | User-chosen identifier in cbox.yaml (`local`, `cloud`). Tiers reference backends by name. |
| **backend type** | Implementation kind (`docker`, `gce`, `codespaces`). The `type:` field on a backend declaration. Multiple backends can share a type with different config. |
| **local backend** | Backend whose type runs compute on the user's machine. Currently only `docker`. |
| **remote backend** | Backend whose type runs compute off the user's machine (`gce`, `codespaces`). Provisional trust model — see ADR 012. |
| **default backend** | Implicit `local` Docker backend present when cbox.yaml has no `backends:` section. The name `local` is reserved. |

## Tier

| Term | Definition |
|---|---|
| **tier** | Named, declarative security configuration: `(layers, environment, credentials, network, settings, backend)`. Defined in cbox.yaml. Logical entity; tiers don't run — tier instances do. Tier names are user-chosen. |
| **tier image** | Docker image built from `base → environment → layers` for a tier. Produced by `cbox build <tier>`. One per (tier × backend). |
| **tier instance** | Runtime instantiation of a tier on a backend. One per (tier × backend). Backend-neutral noun: covers Docker containers, GCE VMs, Codespaces workspaces. Has lifecycle states. |
| **tier volume** | Host-side persistent storage at `~/.cbox/volumes/<tier>/`. Mode `0700`, user-owned. The local **trust boundary**. See ADR 012. |
| **tier endpoint** | SSH connectivity for a running tier instance: host, port, user, ssh_options. Code type: `TierEndpoint`. See ADR 011. |
| **tier state** | Persistent contents of a tier that accumulate across sessions. Survives instance pause/stop/resume. Rebuild survival is per-store: the **claude state volume** persists `/home/cbox/.claude` (`.claude.json`, MCP configs, OAuth MCP tokens — ADR 014); the **session workspace** bind mount persists `/workspace/`; the inner Docker daemon's state and image cache survive on the container's writable layer. Other paths under `/home/cbox/` (shell history, scratch files) are *not* guaranteed across image rebuilds unless covered by an explicit volume. Lost only on tier destroy. See ADR 002, ADR 006, ADR 012, ADR 014. |
| **claude state volume** | Per-tier Docker named volume `cbox-tier-<tier>-claude` mounted at `/home/cbox/.claude` in the tier instance. Carries `.claude.json`, MCP server registrations, and OAuth MCP tokens. Survives `cbox build <tier>`. See ADR 014. |
| **tier settings** | Claude Code `settings.json` file mounted into the tier instance. Configures **Claude sandbox**, **permissions**, **allowed domains**. Referenced via the tier's `settings:` field in cbox.yaml. |
| **agent** | The command a session launches inside the tier. Per-tier via `tiers.<name>.agent.{command, autonomous_args}` in cbox.yaml; defaults to Claude Code (`command: claude`, `autonomous_args: [-p]`). Exists so per-tier mocks are possible in tests and so non-Claude agents can be wired in without code changes. Intentionally low-key — Claude Code remains the default and the only blessed agent. |

### Tier instance lifecycle

States apply to the **tier instance**, not the tier. *Idle* is a
predicate over a running instance, not a state.

| Term | Definition |
|---|---|
| **running** | Instance is up; sessions can connect. |
| **paused** | State preserved at near-zero cost. Resumable. Local Docker: `docker pause`. GCE: VM stop with persistent disk. Codespaces: workspace stop. |
| **stopped** | Instance fully shut down. Slower restart, frees more resources. Some backends collapse `paused` and `stopped`. |
| **idle** | Predicate, not a state: a *running* instance with no alive sessions. Cause of auto-pause. |
| **auto-pause** | Automatic transition from idle to paused. Immediate for local Docker; deferred by `idle_timeout` for backends with non-trivial pause cost. |
| **auto-resume** | Automatic transition from paused to running when a session needs it. |
| **idle_timeout** | Backend-config field: how long a tier instance may be idle before auto-pause. Enforced by the backend using its native scheduling mechanism (e.g., GCE instance schedules), not by cbox. cbox runs no daemon. Local Docker defaults to `0`. |

## Session

| Term | Definition |
|---|---|
| **session** | Logical unit of work, identified by `<name>`, scoped to one tier. Has exactly one socket and one workspace; has a window only if interactive. Stateless tracking (ADR 010): the socket *is* the session's existence. |
| **session socket** | dtach socket at `/run/cbox/<name>.sock` inside the tier instance, used by both interactive (`dtach -A -z`) and autonomous (`dtach -n`) sessions. Presence determines aliveness. |
| **session workspace** | Session's working directory at `/workspace/<name>/` inside the tier instance. cbox owns the checkout (clone for the first session in a repo, worktree for subsequent sessions in the same tier — see ADR 009). Persists by default after session destroy. Local backend: also accessible on the host at `~/.cbox/workspaces/<name>/` for editor access. Remote backends: access via Remote-SSH, SSHFS, or rsync-on-demand — no host-side path. |
| **session window** | Host tmux window for an interactive session, named `cbox:<name>`. Autonomous sessions have no window. |
| **session kind** | `interactive` or `autonomous`. Surfaced in `cbox list` output. |

### Session lifecycle

Lifecycle (alive/destroyed) and connection (attached/detached) are
orthogonal. Connection applies to interactive sessions only.

| Term | Definition |
|---|---|
| **alive** | Session's socket exists. Reconnectable. |
| **destroyed** | Session's socket is gone. Unrecoverable. Workspace persists unless explicitly removed. |
| **attached** | At least one SSH client is connected to the dtach socket. Interactive only. |
| **detached** | No SSH clients connected; dtach (and the **agent**) still running. Interactive only. |

### Session kinds

| Term | Definition |
|---|---|
| **interactive session** | Default kind. Uses dtach for inner persistence; appears as a host tmux window. User attaches/detaches freely. |
| **autonomous session** | Created by `cbox run`. Uses dtach in detached mode (`dtach -n`) — same socket layout as an interactive session, no host tmux window. Spawns the tier's **agent** with the prompt as a positional argument; returns immediately. Inspectable via SSH and attachable via `cbox <name>`. See ADR 005. |

## Build inputs

The build chain is ordered: **base image → environment → layers**. Base
is fixed, environment is one-per-user, layers are many-per-tier.

| Term | Definition |
|---|---|
| **base image** / **cbox-base** | Foundation Docker image cbox ships. Provides DinD, sshd, dtach, tmux, bubblewrap, supervisord, the `cbox` user. Users reference as `FROM cbox-base` in their environment but don't extend it directly. One per cbox version. |
| **environment** | Dockerfile that builds on `cbox-base`. Holds dotfiles, shell, editor. Exactly one per *merged* config — declared by either personal or project config; **personal wins** on conflict (ADR 015), with project's value as the first-time-user fallback. Most commonly lives at `~/.config/cbox/environment/`. Distinct from generic English "dev environment." |
| **layer** (cbox layer) | Named, composable, shareable Dockerfile fragment beginning with `ARG BASE_IMAGE` / `FROM ${BASE_IMAGE}`. Multiple layers stack per tier in user-specified order. Declared in cbox.yaml's `layers:` map, referenced from `tiers.<name>.layers: [...]`. **Personal wins** on name conflict during config merge so users can substitute project layers with their own (ADR 015). Distinct from a **Docker image layer** (filesystem delta produced by a Dockerfile instruction); when ambiguous, qualify as "cbox layer." See ADR 003. |

## Credentials

| Term | Definition |
|---|---|
| **credential** | Named declaration in cbox.yaml's `credentials:` map describing how to obtain a sensitive value and how to expose it to a tier. Resolved on the host at tier-instance startup. Two shapes (env, mount). See ADR 012. |
| **env credential** | Credential shape: host-resolves a value via `source:` and exposes it as an env var inside the tier instance. Fields: `env_var`, `source`. |
| **mount credential** | Credential shape: bind-mounts a host path into the tier instance. Field: `mount`. |
| **credential reference** | Entry in `tier.credentials: [...]` that pulls a declared credential into a specific tier. Tiers reference; they don't redefine. |
| **credential source** | Host-side origin of an env credential's value: a 1Password reference (`op://...`), a host env var, or a literal. The `source:` field. Mount credentials don't have a source — the path *is* the source. |

## MCP

| Term | Definition |
|---|---|
| **MCP / MCP server** | Model Context Protocol server. Claude Code's term for an external server providing tools, resources, or prompts. cbox adopts verbatim; see Claude Code docs. |
| **init.d script** | Idempotent `*.sh` under `/cbox/init.d/` in a tier instance. `cbox-init` runs all of them once per container start, after dockerd is reachable. Delivered by layers — each layer's Dockerfile `COPY init.d/ /cbox/init.d/`. Typical use is **token MCP registration**. See ADR 013. |
| **token MCP registration** | cbox mechanism for registering token-authenticated MCPs at tier-instance startup. An **init.d script** reads a credential env var and runs `claude mcp add ... --header`. Resulting header persists in `.claude.json` on the **claude state volume**. |
| **OAuth MCP registration** | cbox mechanism for OAuth-authenticated MCPs. User runs `cbox auth <tier>` once; Claude Code stores resulting tokens on the **claude state volume**. *Not* declared in cbox.yaml — managed imperatively. OAuth MCP state is **tier state**, not a **credential**. |

## Trust and security

| Term | Definition |
|---|---|
| **trust boundary** | Surface beyond which cbox's protection guarantees end. Local backend: the tier volume on host (`0700`, user-owned). Remote backend (provisional, ADR 012): the encrypted disk holding the tier volume. |
| **threat model: host protection** | cbox protects the host from sessions. cbox does *not* protect sessions from each other within a tier — explicit non-goal. See ADR 002. |
| **Claude sandbox** | Bubblewrap-based sandbox configured via Claude Code's `settings.json` `sandbox:` key. Domain allowlisting and filesystem restrictions. Owned by Claude Code, not cbox; the term is adopted verbatim. Distinct from any cbox tier whose name happens to be "sandbox." |
| **permissions** | Claude Code's `settings.json` `permissions:` block (allow/deny tool patterns, MCP access). cbox doesn't redefine. |
| **allowed domains** / **allowlist** | Claude Code's `settings.json` `sandbox.network.allowedDomains`. Adopted verbatim. |
| **network mode** | `tier.network:` field in cbox.yaml. Values: `bridge` (Docker default; **Claude sandbox** can additionally allowlist within), `none` (no network at all). |
| **privileged** | Docker `--privileged` flag. Required for DinD. Always set on tier instances on the Docker backend. |

## Configuration

cbox loads up to two config sources and merges them (ADR 015). Both
sources use the same `cbox.yaml` schema; `validate()` runs only on the
merged result.

| Term | Definition |
|---|---|
| **personal config** | The user's global config: `$CBOX_CONFIG` if set, else `~/.config/cbox/cbox.yaml`. Carries personal preferences, credentials, and any layers/tiers not tied to a specific project. |
| **project config** | A repo-checked-in `.cbox/cbox.yaml` discovered by walking up from `current_dir()`. Deepest match wins (single project source). Carries the project's layers, tiers, and project entries. |
| **`.cbox/`** | Directory at any ancestor of CWD containing the **project config** plus optional sibling assets: `.cbox/layers/<name>/`, `.cbox/tiers/<tier>/settings.json`, etc. The directory name is the opt-in signal for project discovery. |
| **config merge** | The combination of personal + project into a single Config. Project wins on *composition* fields (`tiers`, `projects`, `credentials`, `default_tier`); **personal wins on *content* fields** (`layers`, `environment`); `default_layers` is set-union with project entries first so personal layers stack on top. See ADR 015. |

## Project

| Term | Definition |
|---|---|
| **project** | Named cbox.yaml shorthand mapping a project name to `(repo, default tier)`. Declared in `projects:`, referenced as the second positional argument to `cbox <name>` and `cbox run`. |
| **project name** | Key in `projects:`. Used as a CLI argument. If the argument doesn't match a project name, cbox treats it as a filesystem path. |
| **project repo** | The `projects.<name>.repo` field. Passed verbatim to `git clone` (URL or local path). Relative paths (`.`, `./foo`, `~/foo`) are resolved against the cbox.yaml's parent dir at parse time, so an in-repo project config can use `repo: .` to mean "this repo, here." See ADR 015. |
| **cbox-dev tier** | The in-repo `cbox-dev` tier shipped in cbox's own `.cbox/cbox.yaml`. Composes the `rust` layer for Rust development. No credentials listed — Anthropic auth comes from the **claude state volume** via `cbox auth cbox-dev`. The canonical dogfood tier. |

## Verbs

Each verb maps to one lifecycle action on one noun. `cbox tier <op>`
nests tier-instance operations under a subcommand; bare `cbox <op>`
operates on sessions.

| Verb | Definition |
|---|---|
| **create-or-attach** | Action of `cbox <name>` (no verb). First invocation creates a new alive session; subsequent invocations open an additional shell into the existing session. Describes the call's outcome, not the session lifecycle. |
| **run** | `cbox run <name> [project-or-path] "<prompt>"`. Creates an autonomous session and detaches. |
| **exec** | `cbox exec <name> <cmd>`. Runs a one-off command in an existing alive session's workspace. Doesn't create or destroy. |
| **destroy** | `cbox destroy <name>`. Destroys the session: removes **session socket** and **session window**. **Session workspace** persists by default. The colloquial "kill" is not cbox vocabulary. |
| **build** | `cbox build [tier]`. Builds the tier image (the build chain). |
| **auth** | `cbox auth <tier>`. One-time interactive **OAuth MCP registration** for a tier. Scoped to OAuth MCPs only. |
| **list** | `cbox list`. Enumerates sessions and tier instances. |
| **cleanup** | `cbox cleanup`. Stops idle tier instances (no live sessions). Operator convenience; not part of normal lifecycle. Distinct from **destroy** — stopped tiers keep their **claude state volume** and their workspaces, and resume via `cbox <name>` or `cbox tier resume`. |
| **`cbox tier <op>`** | Tier-instance-level operations: `stop`, `pause`, `resume`. Distinct from session-level verbs at top level. `cbox list` covers the inspection role. |
| **ssh-config** | `cbox ssh-config`. Regenerates `~/.ssh/cbox_hosts` with one `Host cbox-<tier>` stanza per running tier (loopback + current dynamic port + identity file). Users `Include` it once from `~/.ssh/config` so editors and shells can `ssh cbox-<tier>`. Idempotent and overwriting — the host port changes on every tier restart. |

### Action override flags

Override one invocation's action on `cbox <name>`. Not modes — no
persistent state on the session.

| Flag | Definition |
|---|---|
| **`--shell`** | First invocation opens a shell instead of starting the tier's **agent**. |
| **`--claude`** | Subsequent invocation starts another instance of the tier's **agent** instead of opening a shell. Named historically; works for any agent. |
| **`--attach`** | Invocation reattaches to the session's existing dtach instead of creating a new connection. |

## Non-goals

Codified to prevent re-litigating in future development.

| Non-goal | Rationale |
|---|---|
| **Inter-session isolation within a tier** | Sessions in the same tier share the tier volume, Docker daemon, `.claude.json`, and credentials. Not protected from each other. See ADR 002. |
| **Runtime cbox settings** | cbox.yaml is *configuration*, not settings. The word "settings" in cbox refers exclusively to a tier's mounted Claude Code `settings.json`. |
| **Declarative OAuth MCP registration** | OAuth flows require human interaction; cbox uses imperative `cbox auth` rather than a cbox.yaml declaration. May revisit if Claude Code grows scriptable OAuth. |
