//! The SSH keypair cbox uses to log in to its tier instances.
//!
//! Stored under `~/.cbox/keys/id_ed25519{,.pub}` with directory mode `0700`.
//! The keypair is per-host, not per-tier — every cbox-managed container
//! accepts the same public key. Generated lazily on first use via
//! `ssh-keygen` (expected to be on `PATH`; ships with macOS and every
//! desktop Linux).

// Wired by the attach command + backend in later Phase 2 commits.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Filename for the private key inside the keys directory.
const KEY_FILE_NAME: &str = "id_ed25519";

/// Environment variable the base image's entrypoint reads to populate
/// `/home/cbox/.ssh/authorized_keys`. See `base/entrypoint.sh`.
pub const AUTHORIZED_KEYS_ENV: &str = "CBOX_AUTHORIZED_KEYS";

#[derive(Debug, Clone)]
pub struct KeyPair {
    pub private_key_path: PathBuf,
    /// Public key in `ssh-ed25519 AAAA... cbox` form (single line, no trailing newline).
    pub public_key: String,
}

/// Return the cbox keypair, generating it on first run.
pub fn ensure_keypair() -> Result<KeyPair> {
    let dir = state_dir()?.join("keys");
    let private = dir.join(KEY_FILE_NAME);
    let public = dir.join(format!("{KEY_FILE_NAME}.pub"));

    if !private.exists() {
        ensure_dir_0700(&dir)?;
        let status = Command::new("ssh-keygen")
            .arg("-t")
            .arg("ed25519")
            .arg("-f")
            .arg(&private)
            .arg("-N")
            .arg("")
            .arg("-C")
            .arg("cbox")
            .arg("-q")
            .status()
            .context("invoke ssh-keygen")?;
        if !status.success() {
            bail!("ssh-keygen exited with {status}");
        }
    }

    let public_key = std::fs::read_to_string(&public)
        .with_context(|| format!("read {}", public.display()))?
        .trim()
        .to_string();

    Ok(KeyPair {
        private_key_path: private,
        public_key,
    })
}

fn state_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".cbox"))
}

fn ensure_dir_0700(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 0700 {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a keypair into a temp HOME and verify a public key comes
    /// back. Skipped when `ssh-keygen` isn't on PATH (rare, but keeps
    /// CI portable).
    #[test]
    fn ensure_keypair_generates_on_first_call() {
        // Probe ssh-keygen availability via a cheap `-V` invocation.
        if Command::new("ssh-keygen").arg("-?").output().is_err() {
            eprintln!("skip: ssh-keygen not on PATH");
            return;
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        // Snapshot/restore HOME so other tests aren't affected.
        let prev_home = std::env::var_os("HOME");
        // SAFETY: tests run single-threaded under `cargo test` by default;
        // no other thread observes HOME during this scope.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }
        let result = ensure_keypair();
        // Second call must be idempotent — same key.
        let again = ensure_keypair();
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }

        let kp = result.expect("first call");
        let again = again.expect("second call");
        assert!(kp.public_key.starts_with("ssh-ed25519 "), "{}", kp.public_key);
        assert_eq!(kp.public_key, again.public_key);
        assert!(kp.private_key_path.exists());
    }
}
