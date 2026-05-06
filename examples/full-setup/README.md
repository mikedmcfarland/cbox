# Example: full multi-tier setup

A real-world cbox configuration with three tiers (`auto`, `dev`, `power`),
shareable layers, OAuth MCPs, and dotfiles symlinked from the host.

Copy this directory to `~/.config/cbox/` and adjust paths and credential
sources to match your environment.

## What's here

    cbox.yaml                       # top-level config
    environment/
      Dockerfile                    # builds on cbox-base
      zshrc                         # placeholder; symlink your real one
    layers/
      claude/Dockerfile             # installs claude-code via npm
      python/Dockerfile             # python + poetry
      node/Dockerfile               # node + npm
    tiers/
      auto/settings.json            # autonomous: no network, no tool restrictions
      dev/settings.json             # day-to-day: domain-allowlisted, scoped tools
      power/settings.json           # privileged: read-write, broader tools

## Tier roles

| Tier | Network | Credentials | Use case |
|---|---|---|---|
| `auto` | none | anthropic-key | Walk-away autonomous runs (`cbox run`). No prompts, no exfiltration risk. |
| `dev` | bridge + allowlist | anthropic-key, github-ro | Daily interactive work. Scoped tool access. |
| `power` | bridge + allowlist | anthropic-key, github-rw, gcp-viewer | Interactive sessions where you're watching. Broader tools. |

The `auto` tier is the interesting one — `dangerously-skip-permissions:
true` with `network: none`. Claude can do anything inside the tier
instance (no prompts), but it has no network, no GitHub write token, no
cloud credentials. It can modify files, run tests, iterate on code — and
can't reach anything outside.

## Customizing

- Replace `op://Vault/...` paths in `cbox.yaml` with your own 1Password
  references (or switch credential sources to plain env vars).
- Replace the symlink targets in `environment/` with paths to your real
  dotfiles.
- Adjust `tiers/dev/settings.json` allowed domains and tool patterns to
  match your project's needs.
