//! `cbox build [tier]` — assemble tier images.
//!
//! Reads `cbox.yaml`, then for each tier (or just the named tier) runs
//! the three-stage pipeline from [`crate::build`]: base → environment
//! → layers. Image building lives outside the [`Backend`] trait per
//! ADR 011 but uses the same bollard client.
//!
//! [`Backend`]: crate::backend::Backend

use anyhow::{Context, Result};

use crate::backend::local_docker::LocalDockerBackend;
use crate::build::{ImageBuilder, TierBuildPlan, resolve_base_dir};
use crate::config::Config;

pub async fn run(tier: Option<String>, no_cache: bool) -> Result<()> {
    let cfg_path = Config::default_path()?;
    let cfg = Config::load(&cfg_path)
        .with_context(|| format!("load config from {}", cfg_path.display()))?;

    let base_dir = resolve_base_dir()?;

    let tiers: Vec<String> = match tier {
        Some(t) => {
            if !cfg.tiers.contains_key(&t) {
                anyhow::bail!("tier {t:?} is not defined in cbox.yaml");
            }
            vec![t]
        }
        None => cfg.tiers.keys().cloned().collect(),
    };

    let backend = LocalDockerBackend::new()?;
    let builder = ImageBuilder::new(backend.docker(), no_cache);

    // Base + environment are shared across tiers. Build them once at the
    // top of the run; bollard's layer cache makes a no-op rebuild cheap
    // when nothing has changed.
    builder.build_base(&base_dir).await?;
    builder.build_environment(cfg.environment.as_path()).await?;

    for t in &tiers {
        let plan = TierBuildPlan::from_config(&cfg, t)?;
        builder.build_tier(&plan.tier, &plan.layers).await?;
    }

    eprintln!("==> built {} tier image(s)", tiers.len());
    Ok(())
}
