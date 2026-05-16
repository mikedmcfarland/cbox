//! `cbox tier {stop,pause,resume}` — explicit tier-instance lifecycle.
//!
//! Auto-pause on the last `destroy` is handled in `commands/destroy.rs`;
//! these handlers cover the operator-initiated paths the plan calls out
//! under §Container lifecycle.

use anyhow::{Context, Result};

use crate::backend::Backend;
use crate::backend::TierState;
use crate::backend::local_docker::LocalDockerBackend;
use crate::cli::TierOp;
use crate::commands::common::build_run_config;
use crate::config::Config;
use crate::credentials::OnePasswordResolver;
use crate::keys::ensure_keypair;

pub async fn run(op: TierOp) -> Result<()> {
    match op {
        TierOp::Stop { tier } => stop(&tier).await,
        TierOp::Pause { tier } => pause(&tier).await,
        TierOp::Resume { tier } => resume(&tier).await,
        // `cbox list` already prints the tier table; keep `tier list` out
        // of scope so we don't ship two near-identical commands.
        TierOp::List => anyhow::bail!("`cbox tier list` is not implemented; use `cbox list`"),
    }
}

async fn stop(tier: &str) -> Result<()> {
    let cfg = load_config_and_validate_tier(tier).await?;
    let _ = cfg;
    let backend = LocalDockerBackend::new()?;
    match backend.tier_state(tier).await? {
        TierState::NotCreated => {
            eprintln!("==> tier {tier:?} not created; nothing to stop");
            return Ok(());
        }
        TierState::Stopped => {
            eprintln!("==> tier {tier:?} already stopped");
            return Ok(());
        }
        TierState::Running | TierState::Paused => {}
    }
    backend
        .stop(tier)
        .await
        .with_context(|| format!("stop tier {tier:?}"))?;
    eprintln!("==> tier {tier:?} stopped");
    Ok(())
}

async fn pause(tier: &str) -> Result<()> {
    let _ = load_config_and_validate_tier(tier).await?;
    let backend = LocalDockerBackend::new()?;
    match backend.tier_state(tier).await? {
        TierState::Running => {}
        TierState::Paused => {
            eprintln!("==> tier {tier:?} already paused");
            return Ok(());
        }
        TierState::Stopped => anyhow::bail!("tier {tier:?} is stopped; nothing to pause"),
        TierState::NotCreated => anyhow::bail!("tier {tier:?} is not created; nothing to pause"),
    }
    backend
        .pause(tier)
        .await
        .with_context(|| format!("pause tier {tier:?}"))?;
    eprintln!("==> tier {tier:?} paused");
    Ok(())
}

async fn resume(tier: &str) -> Result<()> {
    let cfg = load_config_and_validate_tier(tier).await?;

    // Resume goes through `ensure_running` rather than a dedicated
    // `unpause` because the user's intent is "I want this tier
    // available." If it was paused we unpause; if it was stopped we
    // start; if it was never created we create it. The cost: we resolve
    // credentials even when just unpausing — but that's the same path
    // `cbox <name>` takes on every invocation, so the surface area is
    // already paid for.
    let tier_cfg = cfg
        .tiers
        .get(tier)
        .with_context(|| format!("tier {tier:?} not defined"))?
        .clone();
    let keypair = tokio::task::spawn_blocking(ensure_keypair)
        .await
        .context("join ensure_keypair task")??;
    let resolver = OnePasswordResolver;
    let run_cfg = build_run_config(tier, &cfg, &tier_cfg, &keypair, &resolver).await?;

    let backend = LocalDockerBackend::new()?;
    let endpoint = backend
        .ensure_running(tier, &run_cfg)
        .await
        .with_context(|| format!("resume tier {tier:?}"))?;
    eprintln!(
        "==> tier {tier:?} running at {}:{}",
        endpoint.host, endpoint.port
    );
    Ok(())
}

async fn load_config_and_validate_tier(tier: &str) -> Result<Config> {
    let cfg_path = Config::default_path()?;
    let cfg = Config::load_async(cfg_path).await?;
    if !cfg.tiers.contains_key(tier) {
        anyhow::bail!("tier {tier:?} is not defined in cbox.yaml");
    }
    Ok(cfg)
}
