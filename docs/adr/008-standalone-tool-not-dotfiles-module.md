# ADR 008: Standalone Rust binary, not a dotfiles module

## Status
Accepted (revised — originally Python, switched to Rust)

## Context
cbox started as a potential dotfiles module (like the existing `claude/`, `tmux/`, `git/` modules). The question is whether it should be part of the dotfiles repo or a standalone tool.

## Decision
cbox is a standalone Rust binary installable via `cargo install --path .`. User configuration lives at `~/.config/cbox/` and can optionally be managed by dotfiles/stow.

## Alternatives considered

**Dotfiles module (bash scripts)**: Follows existing dotfiles patterns — bin/ scripts, shrc/ env, install/ setup. Rejected because:

- cbox is a general-purpose tool, not personal config. Others should be able to use it.
- The complexity warrants a proper project structure with tests, packaging, and dependencies (bollard, serde_yaml, etc.).
- A bash CLI at this complexity becomes hard to maintain and test.

## Consequences
- cbox repo ships: Rust binary (CLI + Docker interaction via bollard), base Dockerfile, examples.
- User config (`~/.config/cbox/`) ships separately — in dotfiles, or wherever the user keeps config.
- Templates, tier settings, and environment Dockerfiles are user-authored — cbox has no opinions about what goes in them.
- Installation via `cargo install --path .` or prebuilt binary.
- Dotfiles users stow `~/.config/cbox/` like any other config directory.
