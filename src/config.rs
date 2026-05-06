//! Parsing and validation of `cbox.yaml`.
//!
//! Path fields ([`environment`], `layers.*`, host side of `credentials.*.mount`,
//! `tiers.*.settings`) are tilde-expanded at deserialisation. Cross-reference
//! validation (tier → layer, tier → credential, tier → backend) lives in
//! [`Config::validate`].

use std::collections::BTreeMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Default location of the user's cbox.yaml.
pub const DEFAULT_CONFIG_RELPATH: &str = ".config/cbox/cbox.yaml";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub environment: ExpandedPath,

    #[serde(default)]
    pub default_layers: Vec<String>,

    pub default_tier: Option<String>,

    #[serde(default)]
    pub layers: BTreeMap<String, ExpandedPath>,

    #[serde(default)]
    pub projects: BTreeMap<String, ProjectConfig>,

    #[serde(default)]
    pub credentials: BTreeMap<String, CredentialConfig>,

    pub tiers: BTreeMap<String, TierConfig>,

    #[serde(default)]
    pub backends: BTreeMap<String, BackendConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub repo: String,
    pub tier: Option<String>,
}

/// Two shapes per ADR 012: env credentials inject a value as an env var,
/// mount credentials bind-mount a host path. Distinguished by which fields
/// are present in YAML.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum CredentialConfig {
    Env { env_var: String, source: String },
    Mount { mount: MountSpec },
}

/// `host_path:container_path[:options]`.
#[derive(Debug, Clone)]
pub struct MountSpec {
    pub host_path: PathBuf,
    pub container_path: PathBuf,
    pub options: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierConfig {
    #[serde(default)]
    pub layers: Vec<String>,

    #[serde(default)]
    pub network: NetworkMode,

    #[serde(default)]
    pub credentials: Vec<String>,

    #[serde(rename = "dangerously-skip-permissions", default)]
    pub dangerously_skip_permissions: bool,

    pub settings: Option<ExpandedPath>,

    pub backend: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    #[default]
    Bridge,
    None,
}

/// Backend configuration. The `type` field selects the implementation; the
/// remaining fields are kept as raw YAML so unknown/unimplemented backend
/// types still parse. Validation per backend type happens when the backend
/// is constructed.
#[derive(Debug, Clone, Deserialize)]
pub struct BackendConfig {
    #[serde(rename = "type")]
    pub kind: String,

    #[serde(flatten)]
    pub extra: serde_yaml_ng::Value,
}

/// A path that is `~`-expanded at deserialisation against the user's home dir.
#[derive(Debug, Clone)]
pub struct ExpandedPath(pub PathBuf);

impl ExpandedPath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path(self) -> PathBuf {
        self.0
    }
}

impl Deref for ExpandedPath {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ExpandedPath {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(d)?;
        Ok(ExpandedPath(expand_path(&raw)))
    }
}

impl<'de> Deserialize<'de> for MountSpec {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(d)?;
        parse_mount(&raw).map_err(serde::de::Error::custom)
    }
}

fn expand_path(raw: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(raw).into_owned())
}

fn parse_mount(raw: &str) -> Result<MountSpec, String> {
    let mut parts = raw.splitn(3, ':');
    let host = parts
        .next()
        .ok_or_else(|| format!("mount string is empty: {raw:?}"))?;
    let container = parts
        .next()
        .ok_or_else(|| format!("mount string missing container path: {raw:?}"))?;
    let options = parts.next().map(str::to_string);

    if host.is_empty() || container.is_empty() {
        return Err(format!("mount string has empty host or container path: {raw:?}"));
    }

    Ok(MountSpec {
        host_path: expand_path(host),
        container_path: PathBuf::from(container),
        options,
    })
}

impl Config {
    /// Load and validate the config at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read {}", path.display()))?;
        let cfg: Config = serde_yaml_ng::from_str(&raw)
            .with_context(|| format!("parse {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Resolve the default config path (`$HOME/.config/cbox/cbox.yaml`).
    pub fn default_path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
        Ok(PathBuf::from(home).join(DEFAULT_CONFIG_RELPATH))
    }

    /// Cross-reference checks: tiers refer to declared layers, credentials,
    /// and backends; default_tier and default_layers exist; project tiers
    /// exist.
    pub fn validate(&self) -> Result<()> {
        for (tier_name, tier) in &self.tiers {
            for layer in &tier.layers {
                if !self.layers.contains_key(layer) {
                    bail!("tier {tier_name:?} references undefined layer {layer:?}");
                }
            }
            for cred in &tier.credentials {
                if !self.credentials.contains_key(cred) {
                    bail!("tier {tier_name:?} references undefined credential {cred:?}");
                }
            }
            if let Some(b) = &tier.backend {
                // The implicit "local" backend is always available.
                if b != "local" && !self.backends.contains_key(b) {
                    bail!("tier {tier_name:?} references undefined backend {b:?}");
                }
            }
        }

        for layer in &self.default_layers {
            if !self.layers.contains_key(layer) {
                bail!("default_layers references undefined layer {layer:?}");
            }
        }

        if let Some(t) = &self.default_tier
            && !self.tiers.contains_key(t)
        {
            bail!("default_tier {t:?} is not defined under tiers:");
        }

        for (proj_name, proj) in &self.projects {
            if let Some(t) = &proj.tier
                && !self.tiers.contains_key(t)
            {
                bail!("project {proj_name:?} references undefined tier {t:?}");
            }
        }

        Ok(())
    }

    /// Effective layers for a tier: `default_layers` followed by the tier's
    /// own `layers`, deduplicated, preserving first-seen order.
    pub fn effective_layers(&self, tier_name: &str) -> Result<Vec<String>> {
        let tier = self
            .tiers
            .get(tier_name)
            .with_context(|| format!("tier {tier_name:?} not defined"))?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for l in self.default_layers.iter().chain(tier.layers.iter()) {
            if seen.insert(l.clone()) {
                out.push(l.clone());
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_path() -> PathBuf {
        // tests run from the crate root
        PathBuf::from("examples/full-setup/cbox.yaml")
    }

    #[test]
    fn parses_full_setup_example() {
        let cfg = Config::load(&example_path()).expect("load full-setup example");

        assert_eq!(cfg.default_layers, vec!["claude".to_string()]);
        assert_eq!(cfg.default_tier.as_deref(), Some("dev"));

        assert!(cfg.layers.contains_key("claude"));
        assert!(cfg.layers.contains_key("python"));
        assert!(cfg.layers.contains_key("node"));

        assert!(cfg.tiers.contains_key("auto"));
        assert!(cfg.tiers.contains_key("dev"));
        assert!(cfg.tiers.contains_key("power"));

        let auto = &cfg.tiers["auto"];
        assert_eq!(auto.network, NetworkMode::None);
        assert!(auto.dangerously_skip_permissions);

        let dev = &cfg.tiers["dev"];
        assert_eq!(dev.network, NetworkMode::Bridge);
        assert!(!dev.dangerously_skip_permissions);

        // Mount credential parses into host:container[:opts] with host expanded.
        match &cfg.credentials["gcp-viewer"] {
            CredentialConfig::Mount { mount } => {
                assert_eq!(
                    mount.container_path,
                    PathBuf::from("/home/cbox/.config/gcloud")
                );
                assert_eq!(mount.options.as_deref(), Some("ro"));
                assert!(!mount.host_path.starts_with("~"), "tilde should be expanded");
            }
            _ => panic!("gcp-viewer should be a mount credential"),
        }

        // Env credential.
        match &cfg.credentials["anthropic-key"] {
            CredentialConfig::Env { env_var, source } => {
                assert_eq!(env_var, "ANTHROPIC_API_KEY");
                assert!(source.starts_with("op://"));
            }
            _ => panic!("anthropic-key should be an env credential"),
        }
    }

    #[test]
    fn tilde_expansion_works() {
        let raw = "~/foo/bar";
        let expanded = expand_path(raw);
        assert!(!expanded.starts_with("~"));
        assert!(expanded.ends_with("foo/bar"));
    }

    #[test]
    fn mount_parses_with_options() {
        let m = parse_mount("/etc/foo:/bar:ro").unwrap();
        assert_eq!(m.host_path, PathBuf::from("/etc/foo"));
        assert_eq!(m.container_path, PathBuf::from("/bar"));
        assert_eq!(m.options.as_deref(), Some("ro"));
    }

    #[test]
    fn mount_parses_without_options() {
        let m = parse_mount("/etc/foo:/bar").unwrap();
        assert!(m.options.is_none());
    }

    #[test]
    fn mount_rejects_missing_parts() {
        assert!(parse_mount("/etc/foo").is_err());
        assert!(parse_mount(":/bar").is_err());
        assert!(parse_mount("/etc/foo:").is_err());
    }

    #[test]
    fn validate_rejects_undefined_layer_reference() {
        let yaml = r#"
environment: /tmp/env
layers:
  python: /tmp/python
tiers:
  bad:
    layers: [python, node]
"#;
        let err = serde_yaml_ng::from_str::<Config>(yaml)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("undefined layer"), "{err}");
    }

    #[test]
    fn validate_rejects_undefined_credential() {
        let yaml = r#"
environment: /tmp/env
layers:
  c: /tmp/c
tiers:
  t:
    layers: [c]
    credentials: [missing]
"#;
        let err = serde_yaml_ng::from_str::<Config>(yaml)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("undefined credential"), "{err}");
    }

    #[test]
    fn effective_layers_dedupes_and_orders() {
        let yaml = r#"
environment: /tmp/env
default_layers: [claude, python]
layers:
  claude: /tmp/claude
  python: /tmp/python
  node: /tmp/node
tiers:
  dev:
    layers: [python, node]
"#;
        let cfg: Config = serde_yaml_ng::from_str(yaml).unwrap();
        cfg.validate().unwrap();
        assert_eq!(
            cfg.effective_layers("dev").unwrap(),
            vec!["claude".to_string(), "python".to_string(), "node".to_string()]
        );
    }
}
