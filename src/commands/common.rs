//! Shared setup helpers used by command handlers that bring up a tier
//! and run something inside it.
//!
//! Both `attach::run` and `run::run` need to resolve a tier from CLI
//! flags + project + config, and translate the tier's config into a
//! `TierRunConfig` the backend accepts. Keeping them here means tier-
//! resolution rules stay in one place — there's only one truth-table
//! to update when, e.g., we add per-project default flags.

use anyhow::{Result, bail};

use crate::backend::TierRunConfig;
use crate::build::tier_image_tag;
use crate::config::{Config, TierConfig};
use crate::keys::{AUTHORIZED_KEYS_ENV, KeyPair};
use crate::workspace::{ProjectSource, tier_workspace_mount};

/// Pick a tier for this session. Precedence: CLI override > project's
/// `tier:` field > `default_tier` from cbox.yaml.
pub fn resolve_tier(
    cfg: &Config,
    project: &ProjectSource,
    cli_override: Option<&str>,
) -> Result<String> {
    if let Some(t) = cli_override {
        if !cfg.tiers.contains_key(t) {
            bail!("tier {t:?} is not defined in cbox.yaml");
        }
        return Ok(t.to_string());
    }
    if let ProjectSource::Configured { tier: Some(t), .. } = project
        && cfg.tiers.contains_key(t)
    {
        return Ok(t.clone());
    }
    if let Some(t) = &cfg.default_tier {
        return Ok(t.clone());
    }
    bail!("no tier specified: pass --tier or set default_tier in cbox.yaml")
}

/// Build the `TierRunConfig` for `Backend::ensure_running`. Injects the
/// host keypair via `CBOX_AUTHORIZED_KEYS` and bind-mounts the tier's
/// per-host workspace directory.
pub fn build_run_config(
    tier: &str,
    tier_cfg: &TierConfig,
    keypair: &KeyPair,
) -> Result<TierRunConfig> {
    let workspace_mount = tier_workspace_mount(tier)?;
    Ok(TierRunConfig {
        image: tier_image_tag(tier),
        env: vec![(AUTHORIZED_KEYS_ENV.to_string(), keypair.public_key.clone())],
        network_mode: tier_cfg.network,
        // DinD + bubblewrap at full strength require --privileged.
        privileged: true,
        mounts: vec![workspace_mount],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_config(default_tier: Option<&str>) -> Config {
        let yaml = format!(
            r#"
environment: /tmp/env
{default_tier}
layers:
  c: /tmp/c
tiers:
  dev:
    layers: [c]
  power:
    layers: [c]
projects:
  app:
    repo: /tmp/app
    tier: power
"#,
            default_tier = default_tier
                .map(|t| format!("default_tier: {t}\n"))
                .unwrap_or_default(),
        );
        serde_yaml_bw::from_str(&yaml).expect("yaml")
    }

    #[test]
    fn resolve_tier_prefers_cli_override() {
        let cfg = synthetic_config(Some("dev"));
        let proj = ProjectSource::Configured {
            name: "app".into(),
            repo: "/tmp/app".into(),
            tier: Some("power".into()),
        };
        let t = resolve_tier(&cfg, &proj, Some("dev")).unwrap();
        assert_eq!(t, "dev");
    }

    #[test]
    fn resolve_tier_falls_back_to_project_tier() {
        let cfg = synthetic_config(Some("dev"));
        let proj = ProjectSource::Configured {
            name: "app".into(),
            repo: "/tmp/app".into(),
            tier: Some("power".into()),
        };
        let t = resolve_tier(&cfg, &proj, None).unwrap();
        assert_eq!(t, "power");
    }

    #[test]
    fn resolve_tier_falls_back_to_default_for_path_projects() {
        let cfg = synthetic_config(Some("dev"));
        let proj = ProjectSource::Path("/tmp/something".into());
        let t = resolve_tier(&cfg, &proj, None).unwrap();
        assert_eq!(t, "dev");
    }

    #[test]
    fn resolve_tier_errors_without_default_or_override() {
        let cfg = synthetic_config(None);
        let proj = ProjectSource::Path("/tmp/something".into());
        assert!(resolve_tier(&cfg, &proj, None).is_err());
    }

    #[test]
    fn resolve_tier_rejects_unknown_override() {
        let cfg = synthetic_config(Some("dev"));
        let proj = ProjectSource::Path("/tmp/x".into());
        assert!(resolve_tier(&cfg, &proj, Some("nope")).is_err());
    }
}
