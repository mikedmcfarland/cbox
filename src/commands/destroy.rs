//! `cbox destroy <name>` — kill a session.
//!
//! Removes the dtach socket inside its tier instance, kills the host
//! tmux windows associated with the session, and (optionally) deletes
//! the on-host workspace. If the destroy leaves a tier with no live
//! sessions, the tier instance auto-pauses (plan §Container lifecycle).

use anyhow::{Context, Result};

use crate::backend::Backend;
use crate::backend::TierState;
use crate::backend::local_docker::LocalDockerBackend;
use crate::config::Config;
use crate::keys::ensure_keypair;
use crate::session::{destroy as destroy_session, is_alive, list_active};
use crate::ssh::SshConn;
use crate::tmux;
use crate::workspace::{session_dir, tier_workspace_dir};

pub async fn run(name: String, workspace: bool) -> Result<()> {
    let cfg_path = Config::default_path()?;
    let cfg = Config::load(&cfg_path)
        .with_context(|| format!("load config from {}", cfg_path.display()))?;

    let keypair = tokio::task::spawn_blocking(ensure_keypair)
        .await
        .context("join ensure_keypair task")??;
    let backend = LocalDockerBackend::new()?;

    // Sessions can live in any tier; Phase 2 scans each running tier for
    // the socket. Phase 3's `cbox list` will keep an index we can query
    // directly.
    let mut killed_in: Option<String> = None;
    for tier_name in cfg.tiers.keys() {
        if backend.tier_state(tier_name).await? != TierState::Running {
            continue;
        }
        let Some(endpoint) = backend.endpoint(tier_name).await? else {
            continue;
        };
        let ssh = SshConn {
            endpoint,
            identity_file: keypair.private_key_path.clone(),
        };
        if !is_alive(&ssh, &name).await? {
            continue;
        }

        destroy_session(&ssh, &name)
            .await
            .with_context(|| format!("destroy session {name:?} in tier {tier_name:?}"))?;
        killed_in = Some(tier_name.clone());

        if list_active(&ssh).await?.is_empty() {
            backend
                .pause(tier_name)
                .await
                .with_context(|| format!("auto-pause tier {tier_name:?} after last session"))?;
        }
        break;
    }

    // Wipe host tmux windows regardless — user may have killed the
    // remote socket manually and just wants the windows cleaned up.
    tmux::kill_session_windows(&name).await?;

    if workspace {
        remove_workspace_for(&cfg, &name).await?;
    }

    if killed_in.is_none() {
        eprintln!("==> no live session {name:?} found; cleaned up local state");
    }
    Ok(())
}

async fn remove_workspace_for(cfg: &Config, name: &str) -> Result<()> {
    let mut removed_any = false;
    for tier_name in cfg.tiers.keys() {
        let dir = session_dir(tier_name, name)?;
        if tokio::fs::try_exists(&dir)
            .await
            .with_context(|| format!("stat workspace {}", dir.display()))?
        {
            tokio::fs::remove_dir_all(&dir)
                .await
                .with_context(|| format!("remove workspace {}", dir.display()))?;
            removed_any = true;
        }
    }
    if !removed_any {
        let first_tier = cfg.tiers.keys().next().map(String::as_str).unwrap_or("");
        eprintln!(
            "==> no workspace found for {name:?} under {}",
            tier_workspace_dir(first_tier)?.display(),
        );
    }
    Ok(())
}
