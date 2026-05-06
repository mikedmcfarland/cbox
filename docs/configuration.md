# Configuration

Full configuration reference. For vocabulary, see the
[glossary](glossary.md). For a real setup you can copy, see
[examples/full-setup/](../examples/full-setup/).

## Layout

All cbox config lives at `~/.config/cbox/`:

    ~/.config/cbox/
      cbox.yaml
      environment/
        Dockerfile, zshrc, gitconfig, tmux.conf
      layers/
        claude/Dockerfile
        python/Dockerfile
        node/Dockerfile
      tiers/
        auto/settings.json
        dev/settings.json
        power/settings.json

### Path resolution

Path fields in `cbox.yaml` (`environment:`, `layers.*`, mount host paths,
`tiers.*.settings`) accept absolute paths, `~` paths, or paths relative
to the yaml file. Relative paths make a config tree relocatable —
`examples/full-setup/cbox.yaml` builds from any working directory
without reaching into `$HOME`.

### Overrides

| Variable | Effect |
|---|---|
| `CBOX_CONFIG` | Path to `cbox.yaml`. Overrides the default `~/.config/cbox/cbox.yaml`. |
| `CBOX_BASE_DIR` | Path to the cbox `base/` directory (the foundation image source). Defaults to `./base`, then `<bin>/base`, then `<bin>/../share/cbox/base`. |

## cbox.yaml

Top-level fields:

| Field | Description |
|---|---|
| `environment:` | Path to the environment Dockerfile directory. |
| `default_layers:` | Layers applied to every tier unless overridden. |
| `default_tier:` | Tier used when `--tier` is omitted. |
| `layers:` | Map of layer name → layer directory. |
| `projects:` | Map of project name → `(repo URL, default tier)`. Optional. |
| `credentials:` | Map of credential name → declaration. |
| `tiers:` | Map of tier name → tier configuration. |
| `backends:` | Map of backend name → backend config. Omit for local-only. |

## Environment

The environment is your personal Dockerfile, built once on top of
`cbox-base`. Holds dotfiles, shell, editor. Symlink to your real
dotfiles — Docker follows symlinks in the build context:

    environment/
      Dockerfile
      zshrc              # container-specific
      tmux.conf  → ~/dotfiles/tmux/.tmux.conf
      gitconfig  → ~/dotfiles/git-personal/.gitconfig

Example `Dockerfile`:

```dockerfile
FROM cbox-base
USER root
RUN apt-get update && apt-get install -y \
    zsh emacs-nox ripgrep fzf direnv locales \
    && locale-gen en_US.UTF-8
USER cbox
COPY --chown=cbox:cbox zshrc /home/cbox/.zshrc
COPY --chown=cbox:cbox gitconfig /home/cbox/.gitconfig
COPY --chown=cbox:cbox tmux.conf /home/cbox/.tmux.conf
```

## Layers

Layers are shareable Dockerfile fragments. Each layer's Dockerfile begins
with `ARG BASE_IMAGE` / `FROM ${BASE_IMAGE}` so it stacks on whatever
came before:

```dockerfile
# ~/.config/cbox/layers/python/Dockerfile
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
RUN apt-get update && apt-get install -y python3 python3-venv \
    && pip install poetry
```

Tiers stack layers in order. With these layers defined:

```yaml
layers:
  claude: ~/.config/cbox/layers/claude
  python: ~/.config/cbox/layers/python
  node: ~/.config/cbox/layers/node
```

A tier with `layers: [python, node]` builds:

    cbox-base → environment → python → node

Docker caches each independently. See [ADR 003](adr/003-plain-dockerfiles-not-yaml.md).

## Tiers

Tiers combine three mechanisms:

| Mechanism | Field | Effect |
|---|---|---|
| Docker network | `network:` | `bridge` (with allowlist) or `none` (no network) |
| Credentials | `credentials:` | Which tokens/mounts are available |
| Claude settings | `settings:` | Tool permissions and domain allowlists, in Claude Code's `settings.json` format |

Example tier definitions:

```yaml
tiers:
  auto:
    layers: [claude]
    network: none
    credentials: [anthropic-key]
    dangerously-skip-permissions: true
    settings: ~/.config/cbox/tiers/auto/settings.json
  dev:
    layers: [python, node]
    network: bridge
    credentials: [anthropic-key, github-ro]
    settings: ~/.config/cbox/tiers/dev/settings.json
```

### Tier settings.json

Mounted into the tier instance. Configures Claude sandbox (bubblewrap),
permissions, and allowed domains. Format is Claude Code's native
`settings.json`.

**Auto tier** — no network, no tool restrictions (the tier instance IS
the sandbox):

```json
{
  "sandbox": {
    "allowUnsandboxedCommands": false
  }
}
```

**Dev tier** — domain-allowlisted, scoped tool access:

```json
{
  "sandbox": {
    "network": {
      "allowedDomains": [
        "api.anthropic.com", "github.com", "*.githubusercontent.com",
        "pypi.org", "registry.npmjs.org"
      ]
    },
    "allowUnsandboxedCommands": false
  },
  "permissions": {
    "allow": [
      "Read", "Edit", "Write", "Glob", "Grep",
      "Bash(git *)", "Bash(python *)", "Bash(pytest *)", "Bash(poetry *)",
      "Bash(make test:*)", "Bash(make lint)", "Bash(make format)",
      "Bash(docker compose *)",
      "mcp__github__pull_request_read", "mcp__github__list_issues"
    ],
    "deny": ["Bash(gcloud secrets *)"]
  }
}
```

## Projects

Optional shorthand for repos used often:

```yaml
projects:
  my-app:
    repo: git@github.com:org/my-app.git
    tier: dev
```

Then `cbox my-fix my-app` resolves repo and tier automatically. If the
project argument doesn't match a key in `projects:`, cbox treats it as
a filesystem path.

## Credentials

Two shapes:

| Shape | Fields | Mechanism |
|---|---|---|
| **env credential** | `env_var`, `source` | Host-resolves `source`, exposes as env var inside the tier instance |
| **mount credential** | `mount` | Bind-mounts a host path into the tier instance |

```yaml
credentials:
  anthropic-key:
    env_var: ANTHROPIC_API_KEY
    source: op://Vault/Anthropic Key/credential
  github-ro:
    env_var: GITHUB_TOKEN
    source: op://Vault/GitHub PAT Read/credential
  gcp-viewer:
    mount: ~/.config/gcloud:/home/cbox/.config/gcloud:ro
```

Tiers reference credentials by name in `tier.credentials: [...]`. See
[ADR 012](adr/012-credentials-and-trust-boundaries.md) for the trust
model.

## MCP servers

| Kind | Setup |
|---|---|
| **Token MCP** (GitHub) | init.d script in a layer reads a credential env var and runs `claude mcp add ... --header`. Header persists in `.claude.json` on the tier volume. |
| **OAuth MCP** (Notion, Linear, Slack) | Run `cbox auth <tier>` once. Browser flow; tokens persist on the tier volume. The tier instance is long-lived, so you auth once. |

Example init.d script:

```bash
# layers/github/init.d/10-mcp-github.sh
[ -n "$GITHUB_TOKEN" ] && claude mcp add github \
    https://api.githubcopilot.com/mcp/ -t http -s user \
    -H "Authorization: Bearer $GITHUB_TOKEN"
```

## Docker-in-Docker

Sessions in a tier share one Docker daemon. cbox sets
`COMPOSE_PROJECT_NAME` to the session name automatically. For multiple
compose stacks on the same project, use **dynamic port publishing** to
avoid host port collisions:

```yaml
ports:
  - "5432"      # Docker picks the host port; not "5432:5432"
```

Discover the actual port at runtime:

```bash
DB_PORT=$(docker compose port postgres 5432 | cut -d: -f2)
```

Different projects in the same tier usually don't conflict (different
services use different ports). Fixed mappings are fine for them.

## Backends

cbox uses a `local` Docker backend by default. To declare named backends:

```yaml
backends:
  local:
    type: docker
  cloud:
    type: gce
    project: my-gcp-project
    zone: us-central1-a
    registry: us-central1-docker.pkg.dev/my-gcp-project/cbox

tiers:
  auto:  { backend: local, ... }
  power: { backend: cloud, ... }
```

When `backends:` is omitted, all tiers use an implicit `local` Docker
backend. See [ADR 011](adr/011-backend-abstraction.md).
