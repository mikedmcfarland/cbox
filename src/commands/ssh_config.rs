//! `cbox ssh-config` — regenerate `~/.ssh/cbox_hosts`.
//!
//! Editors (VS Code Remote-SSH, Cursor, Zed) expect tier hosts to be
//! reachable by name — `ssh cbox-dev` rather than
//! `ssh -p 47239 -i ... cbox@127.0.0.1`. This command writes one
//! `Host cbox-<tier>` stanza per tier the local backend currently has
//! running, so the user only has to drop a single `Include ~/.ssh/cbox_hosts`
//! line into `~/.ssh/config` once.
//!
//! The file is overwritten every time: tier ports change on every
//! container restart (Docker assigns a new dynamic host port), so the
//! stanza set is meaningless once we're past the next `tier stop`.
//! Paused / not-created tiers are skipped — there's no port to advertise.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::backend::Backend;
use crate::backend::local_docker::LocalDockerBackend;
use crate::config::Config;
use crate::keys::ensure_keypair;

pub const CBOX_HOSTS_RELPATH: &str = ".ssh/cbox_hosts";

pub async fn run() -> Result<()> {
    let cfg = Config::load_async().await?;
    let keypair = tokio::task::spawn_blocking(ensure_keypair)
        .await
        .context("join ensure_keypair task")??;
    let backend = LocalDockerBackend::new()?;

    let mut stanzas = Vec::new();
    let mut skipped = Vec::new();
    for tier in cfg.tiers.keys() {
        if let Some(endpoint) = backend
            .endpoint(tier)
            .await
            .with_context(|| format!("endpoint for tier {tier:?}"))?
        {
            stanzas.push(render_stanza(tier, &endpoint, &keypair.private_key_path));
        } else {
            skipped.push(tier.clone());
        }
    }

    let body = render_file(&stanzas);
    let dest = cbox_hosts_path()?;
    write_atomic(&dest, &body).await?;
    eprintln!(
        "==> wrote {} ({} host{})",
        dest.display(),
        stanzas.len(),
        s(stanzas.len())
    );
    if !skipped.is_empty() {
        eprintln!("==> skipped (not running): {}", skipped.join(", "));
    }
    eprintln!(
        "==> add this to ~/.ssh/config if not already present:\n    Include {}",
        dest.display()
    );
    Ok(())
}

fn s(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

pub fn cbox_hosts_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(CBOX_HOSTS_RELPATH))
}

fn render_stanza(
    tier: &str,
    endpoint: &crate::backend::TierEndpoint,
    identity_file: &Path,
) -> String {
    // The trust boundary is the loopback interface (see crate::ssh
    // for the rationale), so `known_hosts` is intentionally /dev/null
    // and host-key checking is off. `IdentitiesOnly` keeps any agent-
    // loaded keys out of the way — the cbox key is the only one that
    // works.
    let mut s = String::new();
    s.push_str(&format!("Host cbox-{tier}\n"));
    s.push_str(&format!("    HostName {}\n", endpoint.host));
    s.push_str(&format!("    Port {}\n", endpoint.port));
    s.push_str(&format!("    User {}\n", endpoint.user));
    s.push_str(&format!("    IdentityFile {}\n", identity_file.display()));
    s.push_str("    IdentitiesOnly yes\n");
    s.push_str("    StrictHostKeyChecking no\n");
    s.push_str("    UserKnownHostsFile /dev/null\n");
    s.push_str("    LogLevel ERROR\n");
    for (k, v) in &endpoint.ssh_options {
        s.push_str(&format!("    {k} {v}\n"));
    }
    s
}

fn render_file(stanzas: &[String]) -> String {
    let mut out = String::new();
    out.push_str(
        "# Managed by `cbox ssh-config` — do not edit by hand.\n\
         # Regenerate whenever a tier restarts (the dynamic port changes).\n\
         # Include this from ~/.ssh/config:\n\
         #     Include ~/.ssh/cbox_hosts\n",
    );
    for stanza in stanzas {
        out.push('\n');
        out.push_str(stanza);
    }
    if stanzas.is_empty() {
        out.push_str("\n# No cbox tiers are currently running.\n");
    }
    out
}

async fn write_atomic(dest: &Path, body: &str) -> Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = dest.with_extension("tmp");
    tokio::fs::write(&tmp, body)
        .await
        .with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(&tmp, perms)
            .await
            .with_context(|| format!("chmod {}", tmp.display()))?;
    }
    tokio::fs::rename(&tmp, dest)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::backend::TierEndpoint;

    fn endpoint(port: u16) -> TierEndpoint {
        TierEndpoint {
            host: "127.0.0.1".into(),
            port,
            user: "cbox".into(),
            ssh_options: Vec::new(),
        }
    }

    #[test]
    fn stanza_includes_required_fields() {
        let s = render_stanza("dev", &endpoint(47239), &PathBuf::from("/k/id"));
        assert!(s.contains("Host cbox-dev"), "{s}");
        assert!(s.contains("HostName 127.0.0.1"), "{s}");
        assert!(s.contains("Port 47239"), "{s}");
        assert!(s.contains("User cbox"), "{s}");
        assert!(s.contains("IdentityFile /k/id"), "{s}");
        assert!(s.contains("IdentitiesOnly yes"), "{s}");
        assert!(s.contains("StrictHostKeyChecking no"), "{s}");
    }

    #[test]
    fn stanza_passes_through_extra_ssh_options() {
        let mut ep = endpoint(443);
        ep.ssh_options
            .push(("ProxyCommand".into(), "nc proxy 22".into()));
        let s = render_stanza("power", &ep, &PathBuf::from("/k"));
        assert!(s.contains("ProxyCommand nc proxy 22"), "{s}");
    }

    #[test]
    fn render_file_warns_when_no_tiers_running() {
        let s = render_file(&[]);
        assert!(s.contains("Managed by `cbox ssh-config`"), "{s}");
        assert!(s.contains("No cbox tiers are currently running"), "{s}");
    }

    #[test]
    fn render_file_concatenates_stanzas_with_blank_lines() {
        let a = render_stanza("a", &endpoint(1), &PathBuf::from("/k"));
        let b = render_stanza("b", &endpoint(2), &PathBuf::from("/k"));
        let s = render_file(&[a, b]);
        assert!(s.contains("Host cbox-a"), "{s}");
        assert!(s.contains("Host cbox-b"), "{s}");
        // A blank line precedes each stanza so OpenSSH parses them separately.
        assert!(s.contains("\n\nHost cbox-a"), "{s}");
        assert!(s.contains("\n\nHost cbox-b"), "{s}");
    }
}
