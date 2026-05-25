//! Parsing and validation of `cbox.yaml`.
//!
//! Path fields ([`environment`], `layers.*`, host side of `credentials.*.mount`,
//! `tiers.*.settings`) are tilde-expanded at deserialisation. Cross-reference
//! validation (tier → layer, tier → credential, tier → backend) lives in
//! [`Config::validate`].

// Phase 1 foundation: many config fields are deserialized for validation
// but not yet read by consumers (sessions, credential resolution).
// Drop this when Phase 2+ wires them up.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Default location of the user's cbox.yaml.
pub const DEFAULT_CONFIG_RELPATH: &str = ".config/cbox/cbox.yaml";

/// Environment variable that overrides the default config path. Useful for
/// pointing at `examples/full-setup/cbox.yaml` for end-to-end validation.
pub const CONFIG_ENV: &str = "CBOX_CONFIG";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Build context for the environment image. Optional *per source* so a
    /// project's `.cbox/cbox.yaml` and a personal config can each omit it
    /// and inherit the other's; required after merge. Read absolute via
    /// [`Config::environment_dir`].
    #[serde(default)]
    pub environment: Option<ExpandedPath>,

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

    /// Which agent command to launch in sessions. Defaults to `claude` /
    /// `claude -p <prompt>`. Per-tier so tests can substitute a mock and
    /// non-Claude agents can be tried out without a cbox.yaml-global
    /// commitment.
    #[serde(default)]
    pub agent: AgentConfig,
}

/// Agent command shape. Defaults match Claude Code; override per-tier
/// to point at a different binary or to inject a mock for tests.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Command run by the interactive session (`cbox <name>`). Resolved
    /// against the container's `PATH` under `bash -l`, so anything on
    /// the tier image's PATH works.
    #[serde(default = "default_agent_command")]
    pub command: String,

    /// Arguments inserted between [`Self::command`] and the user's
    /// prompt for `cbox run`. Default `["-p"]` produces
    /// `claude -p '<prompt>'`; for an agent that takes the prompt after
    /// a different flag, override here (e.g. `["--message"]` for aider).
    #[serde(default = "default_autonomous_args")]
    pub autonomous_args: Vec<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            command: default_agent_command(),
            autonomous_args: default_autonomous_args(),
        }
    }
}

fn default_agent_command() -> String {
    "claude".to_string()
}

fn default_autonomous_args() -> Vec<String> {
    vec!["-p".to_string()]
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
    pub extra: serde_yaml_bw::Value,
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

fn resolve(p: &mut PathBuf, base: &Path) {
    if !p.is_absolute() {
        *p = base.join(&*p);
    }
}

/// True when a `repo:` value should be treated as a filesystem path rather
/// than a git URL. Path-shaped values start with `.`, `/`, or `~` (covering
/// `.`, `..`, `./x`, `../x`, `/abs`, `~/home`); everything else (`git@…`,
/// `https://…`, a bare `foo`) is a remote URL and is left verbatim.
fn repo_is_path(s: &str) -> bool {
    s.starts_with(['.', '/', '~'])
}

/// Resolve a path-shaped `repo:` against `base` (the cbox.yaml's parent
/// dir), tilde-expanding first and collapsing `.`/`..` via `canonicalize`
/// when the target exists. Non-path values pass through unchanged. Mirrors
/// the layer/environment resolution so a project entry can point at a repo
/// relative to its config file (`repo: ..` from `.cbox/cbox.yaml` is the
/// enclosing repo) instead of hardcoding an absolute machine path.
fn resolve_repo(repo: &mut String, base: &Path) {
    if !repo_is_path(repo) {
        return;
    }
    let mut p = PathBuf::from(shellexpand::tilde(repo.as_str()).into_owned());
    if !p.is_absolute() {
        p = base.join(p);
    }
    // Canonicalize to collapse `..`/`.` when the path exists; otherwise keep
    // the lexically-joined path so resolution stays infallible. A missing
    // repo then surfaces at clone time with git's own error.
    let resolved = std::fs::canonicalize(&p).unwrap_or(p);
    *repo = resolved.to_string_lossy().into_owned();
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
        return Err(format!(
            "mount string has empty host or container path: {raw:?}"
        ));
    }

    Ok(MountSpec {
        host_path: expand_path(host),
        container_path: PathBuf::from(container),
        options,
    })
}

impl Config {
    /// Discover, merge, and validate the effective config from up to two
    /// sources (ADR 015):
    ///
    /// - **personal**: `$CBOX_CONFIG`, else `$HOME/.config/cbox/cbox.yaml`
    ///   if it exists;
    /// - **project**: the nearest `.cbox/cbox.yaml` walking up from the
    ///   current directory.
    ///
    /// Each source is parsed and has its relative paths resolved against
    /// its own parent dir; [`Self::merge`] combines them; validation runs
    /// once on the result. Errors if neither source exists.
    pub fn load() -> Result<Self> {
        let personal = Self::load_personal()?;
        let project = Self::load_project()?;
        let merged = match (personal, project) {
            (Some(p), Some(j)) => Self::merge(p, j),
            (Some(p), None) => p,
            (None, Some(j)) => j,
            (None, None) => bail!(
                "no cbox config found (looked for $CBOX_CONFIG, ~/{}, and a .cbox/cbox.yaml \
                 in the current directory or any ancestor)",
                DEFAULT_CONFIG_RELPATH
            ),
        };
        merged.validate()?;
        Ok(merged)
    }

    /// Async wrapper around [`Config::load`] for use from tokio handlers
    /// (current-thread runtime). `load` does synchronous filesystem I/O,
    /// so call sites in async fns must offload it via `spawn_blocking`.
    pub async fn load_async() -> Result<Self> {
        tokio::task::spawn_blocking(Config::load)
            .await
            .context("join Config::load task")?
    }

    /// Read, parse, and resolve relative paths for a single config file —
    /// **without** validating. Validation is deferred to post-merge
    /// ([`Self::load`]) so a per-source file may reference content that
    /// only the other source defines.
    fn load_unvalidated(path: &Path) -> Result<Self> {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let mut cfg: Config =
            serde_yaml_bw::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        cfg.resolve_relative_paths(base);
        Ok(cfg)
    }

    /// Load and validate a single config file. Used for tests and direct
    /// `examples/full-setup/cbox.yaml` validation; the production entry
    /// point is [`Self::load`].
    pub fn load_file(path: &Path) -> Result<Self> {
        let cfg = Self::load_unvalidated(path)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// The personal config source: `$CBOX_CONFIG` (required to exist if
    /// set), else `$HOME/.config/cbox/cbox.yaml` if present. `None` when
    /// no personal source applies.
    pub fn load_personal() -> Result<Option<Self>> {
        if let Some(p) = std::env::var_os(CONFIG_ENV) {
            let path = PathBuf::from(p);
            let cfg = Self::load_unvalidated(&path)
                .with_context(|| format!("load $CBOX_CONFIG ({})", path.display()))?;
            return Ok(Some(cfg));
        }
        let Some(home) = std::env::var_os("HOME") else {
            return Ok(None);
        };
        let path = PathBuf::from(home).join(DEFAULT_CONFIG_RELPATH);
        if !path.is_file() {
            return Ok(None);
        }
        let cfg =
            Self::load_unvalidated(&path).with_context(|| format!("load {}", path.display()))?;
        Ok(Some(cfg))
    }

    /// The project config source: the nearest `.cbox/cbox.yaml` found by
    /// walking up from `current_dir()`. Deepest match wins; the walk halts
    /// at the filesystem root. `None` when no ancestor carries one.
    pub fn load_project() -> Result<Option<Self>> {
        let start = std::env::current_dir().context("determine current directory")?;
        match Self::find_project_config(&start) {
            Some(path) => {
                let cfg = Self::load_unvalidated(&path)
                    .with_context(|| format!("load {}", path.display()))?;
                Ok(Some(cfg))
            }
            None => Ok(None),
        }
    }

    /// Walk up from `start` returning the nearest `.cbox/cbox.yaml`
    /// (deepest match wins); halts at the filesystem root.
    fn find_project_config(start: &Path) -> Option<PathBuf> {
        let mut dir = start;
        loop {
            let candidate = dir.join(".cbox").join("cbox.yaml");
            if candidate.is_file() {
                return Some(candidate);
            }
            dir = dir.parent()?;
        }
    }

    /// Combine a personal and a project config per ADR 015. The guiding
    /// principle: **project wins on composition** (which tiers/projects
    /// exist, how they're assembled, what credentials/backends they
    /// expect); **personal wins on content** (the `layers` and
    /// `environment` Dockerfiles that run on the user's machine).
    pub fn merge(personal: Self, project: Self) -> Self {
        // layers: personal wins — start from project, let personal overwrite.
        let mut layers = project.layers;
        layers.extend(personal.layers);

        // tiers / projects / credentials / backends: project wins.
        // (backends are referenced by `tier.backend`, so they belong with
        // the composition group even though ADR 015's table omits them.)
        let mut tiers = personal.tiers;
        tiers.extend(project.tiers);
        let mut projects = personal.projects;
        projects.extend(project.projects);
        let mut credentials = personal.credentials;
        credentials.extend(project.credentials);
        let mut backends = personal.backends;
        backends.extend(project.backends);

        // environment: personal wins if set, else inherit project's.
        let environment = personal.environment.or(project.environment);

        // default_tier: project wins if set, else personal's.
        let default_tier = project.default_tier.or(personal.default_tier);

        // default_layers: set-union, project first then personal, dedup'd
        // preserving first-seen order.
        let mut seen = std::collections::HashSet::new();
        let mut default_layers = Vec::new();
        for l in project
            .default_layers
            .into_iter()
            .chain(personal.default_layers)
        {
            if seen.insert(l.clone()) {
                default_layers.push(l);
            }
        }

        Self {
            environment,
            default_layers,
            default_tier,
            layers,
            projects,
            credentials,
            tiers,
            backends,
        }
    }

    /// Resolve the config path. Precedence:
    /// 1. `$CBOX_CONFIG` (explicit override).
    /// 2. `$HOME/.config/cbox/cbox.yaml` (default).
    pub fn default_path() -> Result<PathBuf> {
        if let Some(p) = std::env::var_os(CONFIG_ENV) {
            return Ok(PathBuf::from(p));
        }
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
        Ok(PathBuf::from(home).join(DEFAULT_CONFIG_RELPATH))
    }

    /// Walk the path-bearing fields and prepend `base` to any that are
    /// not absolute. Tilde expansion already happened at deserialise
    /// time, so anything still relative is relative to the yaml file.
    fn resolve_relative_paths(&mut self, base: &Path) {
        if let Some(env) = &mut self.environment {
            resolve(&mut env.0, base);
        }
        for layer in self.layers.values_mut() {
            resolve(&mut layer.0, base);
        }
        for cred in self.credentials.values_mut() {
            if let CredentialConfig::Mount { mount } = cred {
                resolve(&mut mount.host_path, base);
            }
        }
        for tier in self.tiers.values_mut() {
            if let Some(s) = &mut tier.settings {
                resolve(&mut s.0, base);
            }
        }
        for proj in self.projects.values_mut() {
            resolve_repo(&mut proj.repo, base);
        }
    }

    /// Absolute path to the environment image build context, or an error if
    /// no config source set one. Resolution happens at parse time, so the
    /// returned path is absolute. Callers that need the directory go through
    /// here rather than touching [`Self::environment`] directly, which is
    /// `Option` only to support the per-source merge (see [`Self::merge`]).
    pub fn environment_dir(&self) -> Result<&Path> {
        self.environment
            .as_ref()
            .map(ExpandedPath::as_path)
            .ok_or_else(|| anyhow::anyhow!("no `environment` directory set in any config source"))
    }

    /// Cross-reference checks: tiers refer to declared layers, credentials,
    /// and backends; default_tier and default_layers exist; project tiers
    /// exist. Also requires `environment` to be set (post-merge invariant).
    pub fn validate(&self) -> Result<()> {
        if self.environment.is_none() {
            bail!("config must set `environment` (the environment image build context)");
        }

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
            let known: Vec<&str> = self.tiers.keys().map(String::as_str).collect();
            bail!(
                "default_tier {t:?} is not defined under `tiers` (known: {})",
                known.join(", ")
            );
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
        let cfg = Config::load_file(&example_path()).expect("load full-setup example");

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
                assert!(
                    !mount.host_path.starts_with("~"),
                    "tilde should be expanded"
                );
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
        let err = serde_yaml_bw::from_str::<Config>(yaml)
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
        let err = serde_yaml_bw::from_str::<Config>(yaml)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("undefined credential"), "{err}");
    }

    #[test]
    fn validate_requires_environment() {
        // A source may omit `environment` (per-source optional), but a config
        // with no `environment` at all must fail validation.
        let yaml = r#"
layers:
  c: /tmp/c
tiers:
  t:
    layers: [c]
"#;
        let cfg: Config = serde_yaml_bw::from_str(yaml).unwrap();
        assert!(cfg.environment.is_none());
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("environment"), "{err}");
    }

    #[test]
    fn environment_dir_bails_when_unset() {
        let yaml = r#"
tiers: {}
"#;
        let cfg: Config = serde_yaml_bw::from_str(yaml).unwrap();
        assert!(cfg.environment_dir().is_err());
    }

    #[test]
    fn resolve_repo_dotdot_resolves_to_enclosing_repo() {
        // `.cbox/cbox.yaml` with `repo: ..` points at the dir containing
        // `.cbox/` (the repo root) — the dogfood shape.
        let tmp = tempfile::tempdir().unwrap();
        let cbox_dir = tmp.path().join(".cbox");
        std::fs::create_dir_all(&cbox_dir).unwrap();
        let yaml = r#"
environment: env
tiers: {}
projects:
  cbox:
    repo: ..
"#;
        let mut cfg: Config = serde_yaml_bw::from_str(yaml).unwrap();
        cfg.resolve_relative_paths(&cbox_dir);
        let expected = tmp.path().canonicalize().unwrap();
        assert_eq!(cfg.projects["cbox"].repo, expected.to_string_lossy());
    }

    #[test]
    fn resolve_repo_dot_resolves_to_config_dir() {
        // `repo: .` resolves to the cbox.yaml's own dir (parse base), per
        // the uniform "relative to my config file" rule.
        let tmp = tempfile::tempdir().unwrap();
        let yaml = r#"
environment: env
tiers: {}
projects:
  here:
    repo: .
"#;
        let mut cfg: Config = serde_yaml_bw::from_str(yaml).unwrap();
        cfg.resolve_relative_paths(tmp.path());
        let expected = tmp.path().canonicalize().unwrap();
        assert_eq!(cfg.projects["here"].repo, expected.to_string_lossy());
    }

    #[test]
    fn resolve_repo_leaves_remote_urls_untouched() {
        let yaml = r#"
environment: /tmp/e
tiers: {}
projects:
  ssh:
    repo: git@github.com:org/app.git
  https:
    repo: https://github.com/org/app.git
  bare:
    repo: some-shorthand
"#;
        let mut cfg: Config = serde_yaml_bw::from_str(yaml).unwrap();
        cfg.resolve_relative_paths(Path::new("/tmp/whatever"));
        assert_eq!(cfg.projects["ssh"].repo, "git@github.com:org/app.git");
        assert_eq!(cfg.projects["https"].repo, "https://github.com/org/app.git");
        assert_eq!(cfg.projects["bare"].repo, "some-shorthand");
    }

    #[test]
    fn agent_defaults_when_block_omitted() {
        let yaml = r#"
environment: /tmp/env
layers:
  c: /tmp/c
tiers:
  dev:
    layers: [c]
"#;
        let cfg: Config = serde_yaml_bw::from_str(yaml).unwrap();
        let agent = &cfg.tiers["dev"].agent;
        assert_eq!(agent.command, "claude");
        assert_eq!(agent.autonomous_args, vec!["-p".to_string()]);
    }

    #[test]
    fn agent_override_replaces_defaults() {
        let yaml = r#"
environment: /tmp/env
layers:
  c: /tmp/c
tiers:
  mock:
    layers: [c]
    agent:
      command: bash
      autonomous_args: ["-c", "echo $0"]
"#;
        let cfg: Config = serde_yaml_bw::from_str(yaml).unwrap();
        let agent = &cfg.tiers["mock"].agent;
        assert_eq!(agent.command, "bash");
        assert_eq!(agent.autonomous_args, vec!["-c", "echo $0"]);
    }

    #[test]
    fn agent_partial_override_keeps_other_default() {
        let yaml = r#"
environment: /tmp/env
layers:
  c: /tmp/c
tiers:
  t:
    layers: [c]
    agent:
      command: aider
"#;
        let cfg: Config = serde_yaml_bw::from_str(yaml).unwrap();
        let agent = &cfg.tiers["t"].agent;
        assert_eq!(agent.command, "aider");
        // autonomous_args wasn't overridden, so it keeps the claude-style default.
        assert_eq!(agent.autonomous_args, vec!["-p".to_string()]);
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
        let cfg: Config = serde_yaml_bw::from_str(yaml).unwrap();
        cfg.validate().unwrap();
        assert_eq!(
            cfg.effective_layers("dev").unwrap(),
            vec![
                "claude".to_string(),
                "python".to_string(),
                "node".to_string()
            ]
        );
    }

    // ---- two-source load + merge (ADR 015) ----

    fn parse(yaml: &str) -> Config {
        serde_yaml_bw::from_str(yaml).expect("parse test yaml")
    }

    fn write_config(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn merge_unions_disjoint_maps() {
        let personal = parse(
            "environment: /p/env\nlayers: {dotfiles: /p/dotfiles}\ntiers: {mine: {layers: [dotfiles]}}\n",
        );
        let project = parse("layers: {rust: /j/rust}\ntiers: {cbox-dev: {layers: [rust]}}\n");
        let m = Config::merge(personal, project);
        assert!(m.layers.contains_key("dotfiles"));
        assert!(m.layers.contains_key("rust"));
        assert!(m.tiers.contains_key("mine"));
        assert!(m.tiers.contains_key("cbox-dev"));
    }

    #[test]
    fn merge_project_wins_for_composition_personal_wins_for_layers() {
        let personal = parse(
            r#"
environment: /p/env
layers: {c: /p/c}
credentials:
  tok: {env_var: TOK, source: "personal"}
projects:
  app: {repo: "personal-url", tier: dev}
tiers:
  dev: {layers: [c], network: none}
"#,
        );
        let project = parse(
            r#"
layers: {c: /j/c}
credentials:
  tok: {env_var: TOK, source: "project"}
projects:
  app: {repo: "project-url", tier: dev}
tiers:
  dev: {layers: [c], network: bridge}
"#,
        );
        let m = Config::merge(personal, project);
        // composition (tiers/credentials/projects): project wins.
        assert_eq!(m.tiers["dev"].network, NetworkMode::Bridge);
        match &m.credentials["tok"] {
            CredentialConfig::Env { source, .. } => assert_eq!(source, "project"),
            _ => panic!("expected env credential"),
        }
        assert_eq!(m.projects["app"].repo, "project-url");
        // content (layers): personal wins.
        assert_eq!(m.layers["c"].as_path(), Path::new("/p/c"));
    }

    #[test]
    fn merge_default_tier_project_wins_else_personal() {
        let personal = parse(
            "environment: /e\ndefault_tier: dev\nlayers: {c: /c}\ntiers: {dev: {layers: [c]}, prod: {layers: [c]}}\n",
        );
        let project = parse("default_tier: prod\ntiers: {}\n");
        assert_eq!(
            Config::merge(personal.clone(), project)
                .default_tier
                .as_deref(),
            Some("prod")
        );
        // Project omits default_tier → inherit personal's.
        let project_no_default = parse("tiers: {}\n");
        assert_eq!(
            Config::merge(personal, project_no_default)
                .default_tier
                .as_deref(),
            Some("dev")
        );
    }

    #[test]
    fn merge_default_layers_set_union_project_first() {
        let personal = parse(
            "environment: /e\ndefault_layers: [dotfiles, claude]\nlayers: {dotfiles: /d, claude: /pc}\ntiers: {}\n",
        );
        let project =
            parse("default_layers: [claude, rust]\nlayers: {claude: /jc, rust: /r}\ntiers: {}\n");
        let m = Config::merge(personal, project);
        // project order first (claude, rust), then personal appends new ones
        // (dotfiles); duplicate claude is dropped.
        assert_eq!(
            m.default_layers,
            vec![
                "claude".to_string(),
                "rust".to_string(),
                "dotfiles".to_string()
            ]
        );
    }

    #[test]
    fn merge_environment_personal_wins_else_project() {
        let personal = parse("environment: /p/env\ntiers: {}\n");
        let project = parse("environment: /j/env\ntiers: {}\n");
        assert_eq!(
            Config::merge(personal, project).environment_dir().unwrap(),
            Path::new("/p/env")
        );
        // Personal omits environment → inherit project's (fresh-clone fallback).
        let personal_no_env = parse("tiers: {}\n");
        let project = parse("environment: /j/env\ntiers: {}\n");
        assert_eq!(
            Config::merge(personal_no_env, project)
                .environment_dir()
                .unwrap(),
            Path::new("/j/env")
        );
    }

    #[test]
    fn merge_validates_cross_source_references() {
        // Project tier references a layer only the personal source defines;
        // post-merge validation closes the reference.
        let personal = parse("environment: /e\nlayers: {claude: /c}\ntiers: {}\n");
        let project = parse("tiers: {cbox-dev: {layers: [claude]}}\n");
        let m = Config::merge(personal, project);
        m.validate()
            .expect("cross-source layer reference validates");
    }

    #[test]
    fn find_project_config_walks_ancestors_deepest_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_config(
            &root.join(".cbox/cbox.yaml"),
            "environment: /outer\ntiers: {}\n",
        );
        let sub = root.join("sub");
        write_config(
            &sub.join(".cbox/cbox.yaml"),
            "environment: /inner\ntiers: {}\n",
        );
        let deep = sub.join("deep/deeper");
        std::fs::create_dir_all(&deep).unwrap();

        // Deepest `.cbox` (sub) wins from a descendant.
        assert_eq!(
            Config::find_project_config(&deep).unwrap(),
            sub.join(".cbox/cbox.yaml")
        );
        // From the root, only the outer config is visible.
        assert_eq!(
            Config::find_project_config(root).unwrap(),
            root.join(".cbox/cbox.yaml")
        );
    }

    #[test]
    fn find_project_config_none_without_cbox() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&dir).unwrap();
        // No `.cbox` up to the filesystem root: returns None (and terminates).
        assert!(Config::find_project_config(&dir).is_none());
    }

    #[test]
    #[serial_test::serial(home)]
    fn load_personal_home_default_env_override_and_absence() {
        // Restore HOME/CBOX_CONFIG on drop. Only env vars are mutated (no
        // cwd), so this is safe alongside the `home` serial lock.
        struct Restore {
            home: Option<std::ffi::OsString>,
            cfg: Option<std::ffi::OsString>,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                // SAFETY: the `home` serial lock serialises env-mutating tests.
                unsafe {
                    match self.home.take() {
                        Some(v) => std::env::set_var("HOME", v),
                        None => std::env::remove_var("HOME"),
                    }
                    match self.cfg.take() {
                        Some(v) => std::env::set_var(CONFIG_ENV, v),
                        None => std::env::remove_var(CONFIG_ENV),
                    }
                }
            }
        }
        let _restore = Restore {
            home: std::env::var_os("HOME"),
            cfg: std::env::var_os(CONFIG_ENV),
        };

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        write_config(
            &home.join(DEFAULT_CONFIG_RELPATH),
            "environment: /home/env\ntiers: {}\n",
        );
        // SAFETY: serialised by the `home` lock.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::remove_var(CONFIG_ENV);
        }
        assert_eq!(
            Config::load_personal()
                .unwrap()
                .unwrap()
                .environment_dir()
                .unwrap(),
            Path::new("/home/env")
        );

        // CBOX_CONFIG overrides the home default.
        let custom = tmp.path().join("custom.yaml");
        write_config(&custom, "environment: /custom/env\ntiers: {}\n");
        unsafe { std::env::set_var(CONFIG_ENV, &custom) };
        assert_eq!(
            Config::load_personal()
                .unwrap()
                .unwrap()
                .environment_dir()
                .unwrap(),
            Path::new("/custom/env")
        );

        // No override and a home with no config → None.
        unsafe {
            std::env::remove_var(CONFIG_ENV);
            std::env::set_var("HOME", tmp.path().join("empty"));
        }
        assert!(Config::load_personal().unwrap().is_none());
    }
}
