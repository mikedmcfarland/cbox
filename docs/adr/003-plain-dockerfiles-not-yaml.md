# ADR 003: Plain Dockerfiles for image layers, not YAML configs

## Status
Accepted

## Context
The image build system needs to support composable layers: a base image, personal environment, and shareable project tooling. Early designs used YAML configs with fields like `packages:` and `build:` that would be compiled into Dockerfiles.

## Decision
All image layers are plain Dockerfiles using `ARG BASE_IMAGE` / `FROM ${BASE_IMAGE}` for composition. `cbox.yaml` describes the chain and metadata but never generates Dockerfiles.

## Alternatives considered

**YAML-to-Dockerfile generation**: Template YAML with `packages:`, `build:`, `tools:` fields compiled to Dockerfiles. Rejected because:

- Users care about *how* things are installed, not just *what*. `packages: [python3]` hides whether it's `apt-get`, `pyenv`, or `conda`.
- The YAML becomes a DSL that reimplements Dockerfile poorly — every Docker feature would need a YAML equivalent.
- Dockerfiles are already the standard, well-documented, and everyone knows them.
- Docker's native build caching works directly — no translation layer to debug.

**Docker Compose for image definitions**: Too verbose for this use case, and compose is about runtime orchestration not build composition.

## Consequences
- Users write Dockerfiles directly — full Docker power, no abstraction leakage.
- `ARG BASE_IMAGE` / `FROM ${BASE_IMAGE}` makes layers composable without hardcoding parents. `cbox build` wires the arg through the chain.
- Shareable: a teammate uses the same layer Dockerfile with their own environment image as the base.
- `cbox.yaml` stays simple — just paths to Dockerfiles and metadata (tier, repo, credentials).
