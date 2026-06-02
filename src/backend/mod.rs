//! Backend abstraction (ADR 011).
//!
//! A [`Backend`] owns tier-instance compute lifecycle and SSH connectivity.
//! Image building, session management, credential resolution, and workspace
//! access live above this trait and are backend-agnostic.
//!
//! The implicit `local` backend (Docker) is the only implementation today;
//! see [`local_docker`].

// `remove_instance` and `list_tiers` are part of the trait surface but not yet
// reached by a command path; revisit when a structured `cbox list --json`
// lands.
#![allow(dead_code)]

pub mod local_docker;

use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;

use crate::config::NetworkMode;

/// Compute provider for tier instances.
///
/// Implementations are referenced by name from `cbox.yaml` (e.g.
/// `backends.local.type = docker`). The trait is `async` because remote
/// backends (GCE, Codespaces) involve genuine network I/O; the local Docker
/// backend uses bollard, which is also async-native.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Ensure the tier's instance is running. Start or resume as needed.
    /// Credentials (already resolved on the host) are passed via
    /// `config.env`.
    async fn ensure_running(&self, tier: &str, config: &TierRunConfig) -> Result<TierEndpoint>;

    /// Pause the tier instance (preserve state at near-zero cost).
    /// Local Docker: `docker pause` (instant). GCE: VM stop (~30s restart).
    async fn pause(&self, tier: &str) -> Result<()>;

    /// Stop the tier instance entirely. Same as pause for some backends.
    async fn stop(&self, tier: &str) -> Result<()>;

    /// Remove just the tier instance (container/VM). Tier state stores
    /// (claude state volume, workspaces, tier image) are left intact —
    /// the instance can be recreated by [`ensure_running`] with the same
    /// state. Idempotent: succeeds if no instance exists.
    ///
    /// This is the building block both `reset` and `tier_destroy` use to
    /// guarantee the instance is gone before they touch state stores.
    /// Callers wanting full teardown should prefer `reset` (instance +
    /// claude state volume) or `tier_destroy` (reset + tier image).
    async fn remove_instance(&self, tier: &str) -> Result<()>;

    /// Wipe tier state: ensure the instance is gone, then remove the
    /// per-tier claude state volume (ADR 014). Tier image is preserved
    /// so the next `ensure_running` recreates an instance with a fresh
    /// `.claude` directory.
    ///
    /// Use cases: recover from a corrupted/misowned volume, switch
    /// Anthropic accounts (force re-OAuth), reset MCP credentials.
    async fn reset(&self, tier: &str) -> Result<()>;

    /// Full teardown: `reset` + remove the tier image and any host-side
    /// session workspaces under this tier. After this, the tier is in
    /// its un-built state — `cbox build <tier>` is required to use it
    /// again. Idempotent on each step (missing resources are not errors).
    async fn tier_destroy(&self, tier: &str) -> Result<()>;

    /// Current state of a tier's instance.
    async fn tier_state(&self, tier: &str) -> Result<TierState>;

    /// Enumerate tier instances managed by this backend.
    async fn list_tiers(&self) -> Result<Vec<TierInfo>>;

    /// SSH connectivity for a running tier. `None` if not running.
    async fn endpoint(&self, tier: &str) -> Result<Option<TierEndpoint>>;
}

/// SSH connectivity for a running tier instance.
#[derive(Debug, Clone)]
pub struct TierEndpoint {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Extra SSH options (e.g., a `ProxyCommand` for IAP tunneling).
    pub ssh_options: Vec<(String, String)>,
}

/// Lifecycle state of a tier instance, as the backend sees it.
///
/// "Idle" (running with no alive sessions) is a predicate derived above
/// the trait by inspecting session sockets in `/run/cbox/`; it is not a
/// `TierState` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierState {
    NotCreated,
    Running,
    Paused,
    Stopped,
}

/// Snapshot of one tier instance for `cbox tier list` / `cbox list`.
#[derive(Debug, Clone)]
pub struct TierInfo {
    pub tier: String,
    pub state: TierState,
    pub backend: String,
}

/// Everything a backend needs to start a tier instance.
#[derive(Debug, Clone)]
pub struct TierRunConfig {
    /// Pre-built image tag (or registry URI for remote backends).
    pub image: String,
    /// Resolved credentials, ready to inject as env vars.
    pub env: Vec<(String, String)>,
    pub network_mode: NetworkMode,
    pub privileged: bool,
    pub mounts: Vec<Mount>,
}

/// A bind mount or named volume for a tier instance.
#[derive(Debug, Clone)]
pub struct Mount {
    pub source: MountSource,
    pub target: PathBuf,
    pub read_only: bool,
}

#[derive(Debug, Clone)]
pub enum MountSource {
    /// Host path bind-mounted into the container.
    HostPath(PathBuf),
    /// Named Docker volume (or backend equivalent).
    Volume(String),
}
