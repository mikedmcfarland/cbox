# cbox

Run Claude Code sessions in isolated containers with configured credentials
and permissions — then work with them as panes and windows in your normal
tmux session. Same keybindings, same workflow, different status bar color.
Each tier is a Docker container with its own network, tokens, and tool
restrictions. You get hands-off autonomous runs without risking your host,
and interactive sessions that feel like home.

## Prerequisites

- Rust ≥ 1.85 (edition 2024) — `rustup` recommended
- [`just`](https://github.com/casey/just)
- A running Docker daemon that supports `--privileged` containers
  (Docker Desktop, Colima, or OrbStack on macOS; native daemon on Linux).
  Tier instances run DinD inside privileged containers — see
  [ADR 001](docs/adr/001-docker-over-tart.md).

## Install

    cargo install --path .

Or build and copy the binary:

    cargo build --release
    cp target/release/cbox ~/.local/bin/

## Quick start

    # First time: copy the sample config to ~/.config/cbox/, or skip the
    # copy and point CBOX_CONFIG at the in-tree example to try it out:
    #     export CBOX_CONFIG=$PWD/examples/full-setup/cbox.yaml

    # Build your dev tier image (one-time; produces ~1.9 GB of layered
    # images: cbox-base → cbox-environment → per-layer stages → tier image)
    cbox build dev

    # Start a session from your project directory
    cd ~/projects/my-app
    cbox my-fix

    # You're in a host tmux window running Claude inside the tier instance.
    # ctrl-b d to detach (dtach keeps Claude alive). Reconnect anytime.

    # Open a shell in the same workspace (session already exists):
    cbox my-fix

    # Or let Claude work autonomously — no prompts, no network, no risk:
    cd ~/projects/my-app
    cbox run fix-tests --tier auto "fix the failing tests"

    cbox list              # see all sessions
    cbox destroy my-fix    # clean up

## How it works

Each tier runs as a long-running **tier instance** — a Docker container on
the local backend. Interactive sessions use `dtach` for inner persistence
and appear as host tmux windows. Autonomous sessions use container tmux
(headless).

    Host tmux
    ├── Window 1 (tm: project): local shells
    ├── Window 2 (cbox: auth-fix): SSH → dtach → /workspace/auth-fix
    ├── Window 3 (cbox: experiment): SSH → dtach → /workspace/experiment
    │
    │   cbox-dev container
    │   ├── dtach: auth-fix.sock     → /workspace/auth-fix
    │   ├── dtach: experiment.sock   → /workspace/experiment
    │   └── shared: .claude.json, Docker daemon, auth
    └── cbox-auto container
        └── tmux: fix-tests.sock   → /workspace/fix-tests (autonomous)

Sessions in a tier share everything except their git workspace. The trust
boundary is between the tier instance and your host — not between sessions.
See [ADR 002](docs/adr/002-per-tier-not-per-session-containers.md) for why.

Vocabulary (tier, tier instance, session kind, tier state, ...) is in the
[glossary](docs/glossary.md).

## Configuration

cbox config lives at `~/.config/cbox/`. A minimal `cbox.yaml`:

```yaml
environment: ~/.config/cbox/environment

default_layers: [claude]
default_tier: dev

layers:
  claude: ~/.config/cbox/layers/claude

credentials:
  anthropic-key:
    env_var: ANTHROPIC_API_KEY
    source: op://Vault/Anthropic Key/credential

tiers:
  dev:
    layers: [claude]
    network: bridge
    credentials: [anthropic-key]
    settings: ~/.config/cbox/tiers/dev/settings.json
```

Build and go:

    cbox build dev
    cd ~/projects/my-repo && cbox my-session

### Anthropic auth

Two ways to log Claude Code into Anthropic, both handled by the
primitives above — no extra cbox config:

- **API key.** Declare an `ANTHROPIC_API_KEY` env credential and list
  it in the tier's `credentials:` (the example above). cbox resolves
  and injects it at container start.
- **OAuth (subscription).** Omit the Anthropic env credential and run
  `cbox auth <tier>` once; complete the login interactively. Claude
  Code stores the token under `/home/cbox/.claude/`, which lives on a
  per-tier Docker volume and survives `cbox build`. Used by the
  in-repo `cbox-dev` tier.

Claude Code itself prefers `ANTHROPIC_API_KEY` over the stored OAuth
token, so a tier that lists both will use the env var.

For the full configuration model — environment Dockerfile, layers,
tier `settings.json`, projects, credential shapes, MCP setup,
Docker-in-Docker compose notes — see
[docs/configuration.md](docs/configuration.md). For a real multi-tier
setup you can copy, see [examples/full-setup/](examples/full-setup/).

## CLI reference

| Command | Description |
|---------|-------------|
| `cbox <name> [project]` | Create or attach. New: Claude in dtach + host tmux window. Existing: shell in same workspace. `--shell`/`--claude`/`--attach` to override. |
| `cbox run <name> [project] <prompt>` | Autonomous session (container tmux, headless), detach. |
| `cbox exec <name> <cmd>` | One-off command in the session's workspace. |
| `cbox auth <tier>` | Set up OAuth MCPs (one-time). |
| `cbox list` | List all sessions and tier instances. |
| `cbox destroy <name>` | Destroy session. Workspace persists by default. |
| `cbox build [tier]` | Build tier image. |
| `cbox tier <op> <tier>` | Tier-instance operations: `stop`, `pause`, `resume`, `list`. |
| `cbox cleanup` | Stop idle tier instances. |
| `cbox ssh-config` | Update `~/.ssh/cbox_hosts`. |
| `cbox completions <shell>` | Print shell completions. |

`cbox <name>` is idempotent: first call creates the session and starts
Claude in dtach; subsequent calls open a shell in the same workspace.
Tier instances auto-start and auto-pause (see
[ADR 010](docs/adr/010-stateless-tracking.md)).

## Host tmux integration

Interactive sessions live inside host tmux as panes and windows — same
keybindings, same copy mode, same everything. `dtach` provides inner
persistence (survives SSH drops) with no keybinding footprint. Autonomous
sessions (`cbox run`) use container tmux internally; inspect with
`ssh <tier> tmux -S /run/cbox/<name>.sock attach`. Clipboard works via
OSC 52 over SSH (host tmux needs `set -g allow-passthrough on`). Configure
the cbox window's `default-command` if you want pane splits to auto-SSH
into the tier instance.

## Dotfiles integration

Your `~/.config/cbox/` directory is just config files and Dockerfiles —
manage it however you manage your other dotfiles (stow, chezmoi, bare
git repo). Symlink your real shell/editor configs into the
`environment/` directory; Docker follows symlinks in the build context.

## Backends

A backend controls where tier instances run. cbox ships with a local
Docker backend that requires no configuration — it's the implicit default.
Future backends will run tiers on remote compute (GCE, Codespaces) while
keeping the same CLI, session model, and tiers. See
[ADR 011](docs/adr/011-backend-abstraction.md).

## Hacking on cbox

The inner loop is `cargo` + `just`; Docker only enters when touching the
backend or build pipeline.

    just check        # cargo check (~1-3s incremental)
    just lint         # clippy with -D warnings
    just test         # unit tests (no Docker)
    just base         # build the cbox-base image
    just example-tier # cbox build dev against examples/full-setup/cbox.yaml
    just integration  # cargo test -- --ignored (DinD smoke; needs example-tier)

`CBOX_CONFIG` overrides the config path; `CBOX_BASE_DIR` overrides the
location of the `base/` Dockerfile directory. See
[CLAUDE.md](CLAUDE.md) for repo conventions and code layout.

## Roadmap

**Remote backends.** GCE with Container-Optimized OS (DinD on privileged
containers, IAP tunneling for SSH, persistent disks for state). GitHub
Codespaces (SSH via `gh cs ssh`, Docker via devcontainer features). The
session model and tiers work identically regardless of where compute
runs; tradeoff is resume latency (~30-45s vs instant Docker unpause).

**Workspace access for remote backends.** Remote tiers have no local
filesystem mount. Options: VS Code Remote-SSH (works with
`cbox ssh-config`), SSHFS auto-mount, rsync-on-demand. Independent of
the backend abstraction.

## Documentation

- **[Glossary](docs/glossary.md)** — canonical vocabulary
- **[Configuration](docs/configuration.md)** — full configuration reference
- **[Plans](docs/plans/)** — design spec and rationale
- **[ADRs](docs/adr/)** — architectural decisions
- **[Examples](examples/)** — copy-able starter configs
