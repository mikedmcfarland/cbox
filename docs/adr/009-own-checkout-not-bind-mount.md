# ADR 009: Own git checkout per session, not bind-mounting host working tree

## Status
Accepted

## Context
Sessions need access to project code. The simplest approach is bind-mounting the host project directory into the container. But this exposes the host filesystem to the container.

## Decision
Sessions always clone from git remotes (or create worktrees) inside the container. The host working tree is never mounted. The container's workspace is exposed to the host at `~/.cbox/workspaces/<name>/` for editor access.

## Alternatives considered

**Bind-mount host project directory**: `docker run -v ~/projects/my-app:/workspace`. Simple, immediate access. Rejected because:

- Defeats the isolation purpose — the container can modify your local checkout, delete files, etc.
- Not much safer than running Claude directly on the host.
- Port of the host filesystem is directly accessible.

**Bind-mount read-only**: Host files visible but not writable from the container. Rejected because Claude needs to write code — read-only is too restrictive.

## Consequences
- `cbox my-fix` (run from the project directory) reads `git remote get-url origin` and clones inside the container. A path can also be passed explicitly as a convenience, but is never mounted.
- Multiple sessions on the same repo use separate clones or git worktrees — no interference.
- Host can browse/edit files at `~/.cbox/workspaces/<name>/` (mounted from container).
- Repos with build-time setup (dependencies, etc.) can be pre-cloned in a project Dockerfile layer for fast startup.
