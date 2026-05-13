# CLAUDE.md

Project-specific guidance for Claude Code working in this repo.

## Project

cbox — Rust binary that runs Claude Code sessions in isolated Docker
containers. Standalone tool (ADR 008); user config lives at
`~/.config/cbox/`. Design and rationale: `docs/plans/`, `docs/`, `examples/`.

## Workflow

- **Branch per phase.** One feature branch per implementation phase from
  `docs/plans/` (`phase-1-foundation`, `phase-2-core-lifecycle`, ...).
  Open a PR per phase against `main`.
- **Incremental commits.** Inside a phase, commit per logical unit
  (scaffold, CLI, config parsing, backend trait, etc.) — not one giant
  end-of-phase commit.
- **Conventional Commits** for messages. Types in active use:
  `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `build`, `ci`.
  Optional scope in parens (e.g., `feat(cli): ...`,
  `feat(backend): ...`). Subject in imperative, lowercase, no trailing
  period. Body explains *why* when the code can't.
- **Never co-author commits.** Commits are authored by Michael
  McFarland <mikedmcfarland@gmail.com> (set as repo-local git config).
  Never add `Co-Authored-By` trailers.
- **PR template.** `.github/pull_request_template.md` is the source of
  truth — fill in TL;DR, What changed, How to test, Why.

## Inner loop

```sh
just check        # cargo check (~1-3s incremental)
just lint         # clippy with -D warnings
just test         # cargo test (no Docker)
just integration  # cargo test -- --ignored  (requires Docker + base image)
```

Steps 1-3 cover ~90% of iteration. Docker only enters when touching
the `Backend` trait impl, image build pipeline, or session machinery.

## Code organisation

```text
src/
  main.rs              entrypoint; #[tokio::main(flavor = "current_thread")]
  cli.rs               clap derive definitions
  commands/<verb>.rs   one handler per CLI verb
  config.rs            cbox.yaml serde structs + path expansion
  backend/mod.rs       async Backend trait, TierEndpoint, TierRunConfig
  backend/local_docker.rs  bollard-backed implementation
  build.rs             image build pipeline (lives outside the trait — ADR 011)
base/                  base image Dockerfile + entrypoint + supervisord
examples/full-setup/   real multi-tier sample config
```

## Async

Async is the contract (ADR 011 `Backend` trait is async). Command
handlers and the trait are `async fn`; the runtime is `current_thread`
tokio set up in `main.rs`. Don't smuggle in synchronous I/O at hot
paths; use `tokio::fs` / `tokio::process` when interacting with the
filesystem or subprocesses inside async fns.

## Things to keep in sync

- `docs/plans/` — design spec; update when an architectural decision shifts.
- `docs/adr/` — add a new ADR for any non-trivial design change rather
  than editing an old one (unless it's a clarification).
- `docs/glossary.md` — vocabulary; add a row when a new noun/verb
  becomes load-bearing.
- `examples/full-setup/cbox.yaml` — keep parseable as the corpus for
  config-parsing tests.
