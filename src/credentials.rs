//! Credential resolution (ADR 012).
//!
//! Env-shaped credentials in `cbox.yaml` carry a `source:` URI (e.g.
//! `op://Vault/Item/field`) that must be resolved to a plaintext value on
//! the host before injection into a tier container. The resolver trait
//! exists so production code can shell out to `op read` while tests can
//! substitute an in-memory map without touching the user's 1Password
//! vaults.
//!
//! Mount-shaped credentials need no resolution — their host path is
//! parsed at config-load time and bind-mounted as-is.

use std::process::Stdio;
use std::time::Duration;

#[cfg(test)]
use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use tokio::process::Command;
use tokio::time::timeout;

/// Upper bound on a single `op read` invocation. The 1Password CLI can
/// block indefinitely if the vault is locked or biometric/interactive
/// auth is required; without a ceiling, session bring-up hangs with no
/// hint of why. 20s is generous for an unlocked, cached session and
/// short enough that a stuck CLI surfaces fast.
const OP_READ_TIMEOUT: Duration = Duration::from_secs(20);

/// Resolve a credential `source:` URI into a plaintext value.
///
/// Implementations should be cheap to call once per session bring-up; if
/// resolution becomes expensive (e.g. an interactive `op` unlock), callers
/// will feel it as session-start latency. Caching is intentionally not
/// part of this trait — secret lifetime management lives elsewhere.
#[async_trait]
pub trait CredentialResolver: Send + Sync {
    async fn resolve_env(&self, source: &str) -> Result<String>;
}

/// Resolver that shells out to the 1Password CLI (`op read <source>`).
/// Accepts any `op://...` URI; rejects other schemes so a typo in
/// `cbox.yaml` fails loud rather than silently echoing the literal.
pub struct OnePasswordResolver;

#[async_trait]
impl CredentialResolver for OnePasswordResolver {
    async fn resolve_env(&self, source: &str) -> Result<String> {
        if !source.starts_with("op://") {
            bail!(
                "credential source {source:?} is not an op:// URI; \
                 only 1Password sources are supported in v1"
            );
        }
        let output = timeout(
            OP_READ_TIMEOUT,
            Command::new("op")
                .args(["read", "--no-newline", source])
                .stdin(Stdio::null())
                .output(),
        )
        .await
        .with_context(|| {
            format!(
                "`op read {source}` timed out after {}s \
                 (is 1Password unlocked and signed in?)",
                OP_READ_TIMEOUT.as_secs()
            )
        })?
        .context("invoke `op read` (is the 1Password CLI installed and on PATH?)")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "`op read {source}` exited with {}: {}",
                output.status,
                stderr.trim()
            );
        }
        let value = String::from_utf8(output.stdout)
            .with_context(|| format!("`op read {source}` returned non-UTF-8 output"))?;
        Ok(value)
    }
}

/// In-memory resolver for tests. Maps `source:` URIs to fixed values; any
/// source not in the map errors. Construct with [`StaticResolver::new`]
/// or the `From<[(K, V); N]>` impl.
#[cfg(test)]
pub struct StaticResolver {
    values: HashMap<String, String>,
}

#[cfg(test)]
impl StaticResolver {
    pub fn new() -> Self {
        Self {
            values: HashMap::new(),
        }
    }

    pub fn with(mut self, source: impl Into<String>, value: impl Into<String>) -> Self {
        self.values.insert(source.into(), value.into());
        self
    }
}

#[cfg(test)]
#[async_trait]
impl CredentialResolver for StaticResolver {
    async fn resolve_env(&self, source: &str) -> Result<String> {
        self.values
            .get(source)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no test value for credential source {source:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_resolver_returns_stored_value() {
        let r = StaticResolver::new().with("op://Vault/Item/field", "secret");
        let v = r.resolve_env("op://Vault/Item/field").await.unwrap();
        assert_eq!(v, "secret");
    }

    #[tokio::test]
    async fn static_resolver_errors_for_unknown_source() {
        let r = StaticResolver::new();
        let err = r.resolve_env("op://nope").await.unwrap_err().to_string();
        assert!(err.contains("no test value"), "{err}");
    }

    #[tokio::test]
    async fn one_password_rejects_non_op_uri() {
        let err = OnePasswordResolver
            .resolve_env("env:FOO")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("op://"), "{err}");
    }
}
