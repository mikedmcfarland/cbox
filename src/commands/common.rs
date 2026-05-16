//! Shared setup helpers used by command handlers that bring up a tier
//! and run something inside it.
//!
//! Both `attach::run` and `run::run` need to resolve a tier from CLI
//! flags + project + config, and translate the tier's config into a
//! `TierRunConfig` the backend accepts. Keeping them here means tier-
//! resolution rules stay in one place — there's only one truth-table
//! to update when, e.g., we add per-project default flags.

use anyhow::{Context, Result, bail};

use crate::backend::{Mount, MountSource, TierRunConfig};
use crate::build::tier_image_tag;
use crate::config::{Config, CredentialConfig, TierConfig};
use crate::credentials::CredentialResolver;
use crate::keys::{AUTHORIZED_KEYS_ENV, KeyPair};
use crate::workspace::{ProjectSource, tier_workspace_mount};

/// Where Claude Code reads its managed (admin/global) settings on Linux.
/// Bind-mounting tier `settings:` here makes the tier's sandbox/permissions
/// config the source of truth — a tier image or session cannot override it
/// without bind-mount privileges they don't have.
pub const MANAGED_SETTINGS_TARGET: &str = "/etc/claude-code/settings.json";

/// Mount target for the per-tier `.claude` named volume. Holds Claude's
/// `.claude.json` (preferences, feature flags, MCP configs, OAuth tokens)
/// and the OAuth credential store for MCPs that authenticate
/// interactively. Persisted in a named volume so `cbox build <tier>`
/// rebuilds don't wipe accumulated state.
pub const CLAUDE_STATE_TARGET: &str = "/home/cbox/.claude";

/// Name of the named volume backing [`CLAUDE_STATE_TARGET`] for one tier.
/// Per-tier so the dev tier can't read the auto tier's tokens by virtue
/// of sharing a volume — matches the per-tier trust boundary from
/// ADR 012.
pub fn claude_volume_name(tier: &str) -> String {
    format!("cbox-tier-{tier}-claude")
}

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

/// Build the `TierRunConfig` for `Backend::ensure_running`.
///
/// Wires up the tier's trust boundary (ADR 012):
/// - Injects the host keypair via `CBOX_AUTHORIZED_KEYS`.
/// - Resolves env-shaped credentials through `resolver` and injects them
///   as container env vars.
/// - Adds bind mounts for mount-shaped credentials, the tier workspace,
///   and (if configured) the tier's managed `settings.json`.
///
/// The full credential set lives in `cfg.credentials`; `tier_cfg.credentials`
/// is just the list of names this tier opts into.
pub async fn build_run_config(
    tier: &str,
    cfg: &Config,
    tier_cfg: &TierConfig,
    keypair: &KeyPair,
    resolver: &dyn CredentialResolver,
) -> Result<TierRunConfig> {
    let mut env = vec![(AUTHORIZED_KEYS_ENV.to_string(), keypair.public_key.clone())];
    let mut mounts = vec![
        tier_workspace_mount(tier)?,
        // Per-tier .claude state on a named volume so MCP tokens
        // registered via init.d (and onboarding/feature-flag state)
        // survive image rebuilds. See ADR 014.
        Mount {
            source: MountSource::Volume(claude_volume_name(tier)),
            target: CLAUDE_STATE_TARGET.into(),
            read_only: false,
        },
    ];

    for cred_name in &tier_cfg.credentials {
        let cred = cfg.credentials.get(cred_name).with_context(|| {
            format!("tier {tier:?} references undefined credential {cred_name:?}")
        })?;
        match cred {
            CredentialConfig::Env { env_var, source } => {
                let value = resolver
                    .resolve_env(source)
                    .await
                    .with_context(|| format!("resolve credential {cred_name:?}"))?;
                env.push((env_var.clone(), value));
            }
            CredentialConfig::Mount { mount } => {
                mounts.push(Mount {
                    source: MountSource::HostPath(mount.host_path.clone()),
                    target: mount.container_path.clone(),
                    // Default to read-only unless the user explicitly
                    // opted out via the `:rw` suffix. Anything else (e.g.
                    // `:ro`, or no option) keeps the safer default.
                    read_only: mount.options.as_deref() != Some("rw"),
                });
            }
        }
    }

    if let Some(settings) = &tier_cfg.settings {
        mounts.push(Mount {
            source: MountSource::HostPath(settings.as_path().to_path_buf()),
            target: MANAGED_SETTINGS_TARGET.into(),
            read_only: true,
        });
    }

    Ok(TierRunConfig {
        image: tier_image_tag(tier),
        env,
        network_mode: tier_cfg.network,
        // DinD + bubblewrap at full strength require --privileged.
        privileged: true,
        mounts,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serial_test::serial;
    use tempfile::TempDir;

    use super::*;
    use crate::credentials::StaticResolver;
    use crate::keys::KeyPair;

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

    /// RAII guard for tests that mutate HOME. Other HOME-mutating tests in
    /// this crate use the same `home` serial lock so the runtime won't
    /// race here.
    struct HomeGuard(Option<std::ffi::OsString>);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: the `home` serial lock around every HOME-mutating
            // test ensures no other thread observes HOME during this
            // scope.
            unsafe {
                match self.0.take() {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    fn set_home(path: &std::path::Path) -> HomeGuard {
        let prev = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", path) }
        HomeGuard(prev)
    }

    fn dummy_keypair() -> KeyPair {
        KeyPair {
            private_key_path: PathBuf::from("/dev/null"),
            public_key: "ssh-ed25519 AAAA cbox".to_string(),
        }
    }

    fn cfg_with_creds_and_settings(settings_path: &std::path::Path) -> Config {
        let yaml = format!(
            r#"
environment: /tmp/env
layers:
  c: /tmp/c
credentials:
  anthropic-key:
    env_var: ANTHROPIC_API_KEY
    source: op://Vault/Anthropic/credential
  gcp-viewer:
    mount: /tmp/gcloud:/home/cbox/.config/gcloud:ro
  scratch:
    mount: /tmp/scratch:/scratch:rw
tiers:
  dev:
    layers: [c]
    credentials: [anthropic-key, gcp-viewer, scratch]
    settings: {settings}
"#,
            settings = settings_path.display(),
        );
        let cfg: Config = serde_yaml_bw::from_str(&yaml).expect("yaml");
        cfg.validate().expect("validate");
        cfg
    }

    #[tokio::test]
    #[serial(home)]
    async fn build_run_config_injects_env_credentials_and_mounts() {
        let tmp = TempDir::new().unwrap();
        let _home = set_home(tmp.path());

        let settings_file = tmp.path().join("settings.json");
        tokio::fs::write(&settings_file, r#"{"sandbox":{}}"#)
            .await
            .unwrap();

        let cfg = cfg_with_creds_and_settings(&settings_file);
        let tier_cfg = cfg.tiers["dev"].clone();
        let resolver =
            StaticResolver::new().with("op://Vault/Anthropic/credential", "sk-test-value");

        let run = build_run_config("dev", &cfg, &tier_cfg, &dummy_keypair(), &resolver)
            .await
            .expect("build run config");

        // Env: authorized_keys plus the resolved Anthropic key.
        let env: std::collections::HashMap<_, _> = run.env.iter().cloned().collect();
        assert_eq!(
            env.get(AUTHORIZED_KEYS_ENV).map(String::as_str),
            Some("ssh-ed25519 AAAA cbox")
        );
        assert_eq!(
            env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-test-value")
        );

        // Mounts: workspace (rw), gcp-viewer (ro), scratch (rw), settings (ro).
        let by_target: std::collections::HashMap<_, _> =
            run.mounts.iter().map(|m| (m.target.clone(), m)).collect();
        let workspace = by_target
            .get(std::path::Path::new("/workspace"))
            .expect("workspace mount");
        assert!(!workspace.read_only);

        let gcloud = by_target
            .get(std::path::Path::new("/home/cbox/.config/gcloud"))
            .expect("gcp-viewer mount");
        assert!(gcloud.read_only, "ro option should propagate");

        let scratch = by_target
            .get(std::path::Path::new("/scratch"))
            .expect("scratch mount");
        assert!(!scratch.read_only, "rw option should propagate");

        let settings = by_target
            .get(std::path::Path::new(MANAGED_SETTINGS_TARGET))
            .expect("settings.json mount");
        assert!(settings.read_only, "settings.json must be read-only");
        match &settings.source {
            MountSource::HostPath(p) => assert_eq!(p, &settings_file),
            _ => panic!("settings mount should be a host path"),
        }

        // The per-tier .claude state lives on a named volume named for
        // its tier; rebuilding the image must not wipe it.
        let claude_state = by_target
            .get(std::path::Path::new(CLAUDE_STATE_TARGET))
            .expect(".claude state mount");
        assert!(!claude_state.read_only, ".claude must be writable");
        match &claude_state.source {
            MountSource::Volume(name) => assert_eq!(name, "cbox-tier-dev-claude"),
            _ => panic!(".claude mount should be a named volume"),
        }
    }

    #[tokio::test]
    #[serial(home)]
    async fn build_run_config_omits_settings_mount_when_not_configured() {
        let tmp = TempDir::new().unwrap();
        let _home = set_home(tmp.path());

        let yaml = r#"
environment: /tmp/env
layers:
  c: /tmp/c
tiers:
  dev:
    layers: [c]
"#;
        let cfg: Config = serde_yaml_bw::from_str(yaml).unwrap();
        cfg.validate().unwrap();
        let tier_cfg = cfg.tiers["dev"].clone();

        let run = build_run_config(
            "dev",
            &cfg,
            &tier_cfg,
            &dummy_keypair(),
            &StaticResolver::new(),
        )
        .await
        .unwrap();

        assert!(
            !run.mounts
                .iter()
                .any(|m| m.target == std::path::Path::new(MANAGED_SETTINGS_TARGET)),
            "no tier `settings:` ⇒ no managed settings mount"
        );
    }

    #[tokio::test]
    #[serial(home)]
    async fn build_run_config_propagates_resolver_error() {
        let tmp = TempDir::new().unwrap();
        let _home = set_home(tmp.path());

        let settings_file = tmp.path().join("settings.json");
        tokio::fs::write(&settings_file, "{}").await.unwrap();

        let cfg = cfg_with_creds_and_settings(&settings_file);
        let tier_cfg = cfg.tiers["dev"].clone();
        // Resolver missing the Anthropic source ⇒ error.
        let resolver = StaticResolver::new();

        let err = build_run_config("dev", &cfg, &tier_cfg, &dummy_keypair(), &resolver)
            .await
            .expect_err("missing source should error");
        let msg = err.to_string();
        assert!(
            msg.contains("anthropic-key") || msg.contains("resolve credential"),
            "{msg}"
        );
    }
}
