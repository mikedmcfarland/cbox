//! `cbox cleanup` — stop tier instances with no live sessions.
//!
//! Symmetric to the auto-pause behaviour in `cbox destroy` (plan
//! §Container lifecycle), but uses `stop` rather than `pause`: cleanup
//! is for "I'm done for the day," not "free up CPU between attaches."
//!
//! Only tiers that the backend reports as `Running` are inspected.
//! Paused tiers are left alone — by definition they aren't burning
//! CPU and their session state isn't reachable without unpausing.

use anyhow::{Context, Result};

use crate::backend::Backend;
use crate::backend::TierState;
use crate::backend::local_docker::LocalDockerBackend;
use crate::config::Config;
use crate::keys::ensure_keypair;
use crate::session::list_active;
use crate::ssh::SshConn;

pub async fn run() -> Result<()> {
    let cfg_path = Config::default_path()?;
    let cfg = Config::load_async(cfg_path).await?;

    let backend = LocalDockerBackend::new()?;

    // Defer key generation: a cleanup that finds no running tiers
    // shouldn't fabricate ~/.cbox/keys as a side effect.
    let mut keypair = None;

    let mut stopped = Vec::new();
    let mut kept_busy = Vec::new();
    for tier in cfg.tiers.keys() {
        if backend.tier_state(tier).await? != TierState::Running {
            continue;
        }
        let Some(endpoint) = backend.endpoint(tier).await? else {
            continue;
        };
        if keypair.is_none() {
            keypair = Some(
                tokio::task::spawn_blocking(ensure_keypair)
                    .await
                    .context("join ensure_keypair task")??,
            );
        }
        let ssh = SshConn {
            endpoint,
            identity_file: keypair.as_ref().unwrap().private_key_path.clone(),
        };
        let sessions = list_active(&ssh)
            .await
            .with_context(|| format!("list sessions in tier {tier:?}"))?;
        if sessions.is_empty() {
            backend
                .stop(tier)
                .await
                .with_context(|| format!("stop tier {tier:?}"))?;
            stopped.push(tier.clone());
        } else {
            kept_busy.push((tier.clone(), sessions));
        }
    }

    if stopped.is_empty() && kept_busy.is_empty() {
        eprintln!("==> no running tiers");
        return Ok(());
    }
    for tier in &stopped {
        eprintln!("==> stopped {tier:?}");
    }
    for (tier, sessions) in &kept_busy {
        eprintln!(
            "==> keeping {tier:?} (live sessions: {})",
            sessions.join(", ")
        );
    }
    Ok(())
}
