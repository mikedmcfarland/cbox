# ADR 011: Backend abstraction for compute providers

## Status
Accepted

## Context
cbox sessions run inside long-lived per-tier instances. The initial implementation targets local Docker, but the same model applies to remote compute: GCE VMs with Container-Optimized OS, GitHub Codespaces, or other providers. Several prior decisions already support this — SSH-based access (ADR 004), own git checkouts (ADR 009), and stateless tracking (ADR 010) all work over the network without modification.

The question is where to draw the abstraction boundary so that (a) the local Docker implementation stays simple, (b) adding a remote backend later doesn't require rearchitecting, and (c) we don't over-engineer for backends that may never exist.

## Decision
Introduce a Backend trait that owns compute lifecycle and SSH connectivity. Everything else — image building, session management, credential resolution, workspace access — lives above the trait and is backend-agnostic.

Backends are named resources in `cbox.yaml`, referenced by tiers. A `local` Docker backend is implicit and requires zero configuration.

### The trait

The Backend provides two things:

1. **Instance lifecycle**: ensure a tier's instance is running, pause it, stop it, destroy it.
2. **SSH endpoint**: how to reach the running instance (host, port, proxy command).

```rust
#[async_trait]
pub trait Backend: Send + Sync {
    /// Ensure the tier's instance is running. Start or resume as needed.
    /// Credentials (resolved by the caller) are passed as env vars for
    /// injection at container start.
    async fn ensure_running(
        &self,
        tier: &str,
        config: &TierRunConfig,
    ) -> Result<TierEndpoint>;

    /// Pause the tier (preserves state, minimal/zero cost).
    /// Local Docker: docker pause (instant). GCE: VM stop (~30s restart).
    async fn pause(&self, tier: &str) -> Result<()>;

    /// Stop the tier entirely (same as pause for some backends).
    async fn stop(&self, tier: &str) -> Result<()>;

    /// Destroy the tier and its resources. State is lost.
    async fn destroy(&self, tier: &str) -> Result<()>;

    /// Current state of a tier's instance.
    async fn tier_state(&self, tier: &str) -> Result<TierState>;

    /// List all tiers managed by this backend.
    async fn list_tiers(&self) -> Result<Vec<TierInfo>>;

    /// SSH connectivity for a running tier. None if not running.
    async fn endpoint(&self, tier: &str) -> Result<Option<TierEndpoint>>;
}

pub struct TierEndpoint {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Additional SSH options (e.g., ProxyCommand for IAP tunneling)
    pub ssh_options: Vec<(String, String)>,
}

/// Passed to ensure_running with everything the backend needs to start compute.
pub struct TierRunConfig {
    pub image: String,              // pre-built image tag or registry URI
    pub env: Vec<(String, String)>, // resolved credentials as env vars
    pub network_mode: NetworkMode,  // none, bridge
    pub privileged: bool,
    pub mounts: Vec<Mount>,         // volumes, settings.json, etc.
}
```

### What the trait does NOT own

- **Image building**: always local Docker (`cbox build` chains Dockerfiles via bollard). If a remote backend needs the image, that's a distribution concern handled separately — not a trait method.
- **Session management**: always SSH + dtach/tmux, using the `TierEndpoint` returned by the backend. The `session.rs` module is backend-agnostic.
- **Credential resolution**: always runs on the host (`op read`, env vars). Resolved values are passed to the backend in `TierRunConfig.env`.
- **Workspace access**: how you edit files in a session (local mount, VS Code Remote-SSH, SSHFS) is orthogonal to where the compute runs. Handled separately, potentially differently per backend.
- **Host tmux integration**: interactive sessions run inline in the invoking pane (ADR 016); `cbox auth <tier>` opens a one-shot host tmux window for the OAuth handoff when invoked inside tmux. Both paths use SSH commands derived from `TierEndpoint`. Backend-agnostic.
- **Docker/compose availability inside the session**: an implicit postcondition of `ensure_running`, not an explicit capability. How it's achieved (DinD with `--privileged`, Docker-outside-of-Docker, VM-native Docker) is a backend implementation detail.

### Per-tier long-lived model

All backends must support the per-tier long-lived container model (ADR 002). Specifically:

- State (`.claude.json`, workspaces, Docker image cache) persists across pause/resume.
- `pause()` preserves state at minimal/zero cost. `destroy()` is the only operation that loses state.
- Multiple sessions share a tier's instance.

This works for all three target backends: local Docker has container pause, GCE has VM stop with persistent disk, Codespaces has workspace stop with persistent filesystem.

What varies is the latency/cost profile:

| | Pause | Resume | Idle cost |
|---|---|---|---|
| Local Docker | Instant | Instant | Zero |
| GCE | ~5s | ~30-45s | Disk only (~$2/mo) |
| Codespaces | ~5s | ~30-60s | Storage charges |

Backends may expose an `idle_timeout` config to account for this — local Docker auto-pauses immediately on last session destroy, while GCE might wait 15-30 minutes.

### Configuration

Backends are named at the top level of `cbox.yaml` and referenced by tiers:

```yaml
# Implicit — no config needed for local-only usage:
# all tiers default to the built-in "local" Docker backend.

tiers:
  sandbox: { ... }
  dev: { ... }

# Explicit — when adding remote backends:
backends:
  local:
    type: docker
  cloud:
    type: gce
    project: my-gcp-project
    zone: us-central1-a
    registry: us-central1-docker.pkg.dev/my-gcp-project/cbox
    use_iap: true
    machine_type: e2-standard-4
    disk_size_gb: 50
    idle_timeout: 30m

tiers:
  sandbox:
    backend: local    # explicit, but same as the default
    ...
  power:
    backend: cloud    # runs on GCE
    ...
```

When no `backends:` section exists and no tier specifies `backend:`, everything uses an implicit local Docker backend. The backend concept is invisible until you need it.

## Alternatives considered

**Global backend (one backend for all tiers)**: Simpler config but prevents mixing local and remote tiers. The per-tier reference adds minimal complexity and avoids a future migration.

**Backend as a tier property with inline config**: Duplicates backend settings (GCP project, zone, registry) when multiple tiers share a backend. Named backends factor this out.

**Capability-based trait (supports_privileged, supports_persistence, etc.)**: Adds negotiation complexity for capabilities that all target backends actually support. Deferred until a backend that genuinely lacks a capability materializes.

**Backend owns image building**: Image building is always local Docker (bollard). Bundling it into the trait would force remote backends to reimplement it or delegate back to local Docker. Keeping it separate is simpler.

## Consequences
- `docker.rs` in the planned repo structure becomes `backend/mod.rs` (trait) + `backend/local_docker.rs` (implementation).
- The local Docker backend is the only implementation. Adding GCE or Codespaces means adding a file, not restructuring.
- `session.rs`, `tmux.rs`, `credentials.rs` never import backend-specific types.
- `ssh.rs` generates config from `TierEndpoint` — different backends produce different SSH options (e.g., IAP ProxyCommand) but the generator is the same.
- Config parsing gains an optional `backends:` section and an optional `backend:` field on tiers, both with sensible defaults.
- See also: ADR 004 (SSH enables remote access), ADR 009 (own checkouts avoid host mounts), ADR 010 (stateless tracking works over SSH).
