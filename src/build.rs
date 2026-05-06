//! Image build pipeline (ADR 011: lives outside the [`Backend`] trait).
//!
//! The pipeline stacks plain Dockerfiles in three layers per plan.md
//! §Image layers:
//!
//! ```text
//! cbox-base          (base/ — ships with cbox)
//!   └── cbox-environment    (user's environment/Dockerfile)
//!        └── cbox-tier-<name>     (stack of per-language layers)
//! ```
//!
//! Each layer Dockerfile uses `ARG BASE_IMAGE` / `FROM ${BASE_IMAGE}` so
//! the same Dockerfile can sit on any predecessor.
//!
//! [`Backend`]: crate::backend::Backend
//!
//! ## Build context
//!
//! Bollard's `/build` endpoint takes a tar of the build context. We pack
//! the directory in memory (Dockerfiles are small) rather than streaming
//! from disk; this keeps the implementation synchronous up to the bollard
//! call and avoids tempfiles.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use bollard::Docker;
use bollard::query_parameters::BuildImageOptionsBuilder;
use bytes::Bytes;
use futures_util::StreamExt;

use crate::config::Config;

/// Tag for the foundation image built from `base/`.
pub const BASE_IMAGE_TAG: &str = "cbox-base:latest";
/// Tag for the user's environment layer.
pub const ENVIRONMENT_IMAGE_TAG: &str = "cbox-environment:latest";

/// Compose the final image tag for a tier.
pub fn tier_image_tag(tier: &str) -> String {
    format!("cbox-tier-{tier}:latest")
}

/// Resolve the directory containing the cbox base Dockerfile.
///
/// Search order:
/// 1. `$CBOX_BASE_DIR` (explicit override).
/// 2. `./base` relative to the current working directory (the repo
///    layout — useful while developing).
/// 3. `<exe parent>/base` and `<exe parent>/../share/cbox/base` (when
///    cbox is installed alongside its base/ tree).
pub fn resolve_base_dir() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("CBOX_BASE_DIR") {
        let p = PathBuf::from(p);
        if p.join("Dockerfile").is_file() {
            return Ok(p);
        }
        bail!(
            "CBOX_BASE_DIR={} does not contain a Dockerfile",
            p.display()
        );
    }

    let mut candidates = vec![PathBuf::from("base")];
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        candidates.push(parent.join("base"));
        candidates.push(parent.join("../share/cbox/base"));
    }

    for c in &candidates {
        if c.join("Dockerfile").is_file() {
            return Ok(c.clone());
        }
    }

    Err(anyhow!(
        "could not locate cbox base/ directory; set CBOX_BASE_DIR \
         or run from the repo root (tried: {})",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Driver for the per-tier build pipeline.
pub struct ImageBuilder<'a> {
    docker: &'a Docker,
    no_cache: bool,
}

impl<'a> ImageBuilder<'a> {
    pub fn new(docker: &'a Docker, no_cache: bool) -> Self {
        Self { docker, no_cache }
    }

    /// Build the foundation image at [`BASE_IMAGE_TAG`].
    pub async fn build_base(&self, base_dir: &Path) -> Result<()> {
        eprintln!("==> building {BASE_IMAGE_TAG} from {}", base_dir.display());
        self.build_dir(base_dir, BASE_IMAGE_TAG, &HashMap::new())
            .await
    }

    /// Build the user's environment layer FROM [`BASE_IMAGE_TAG`].
    pub async fn build_environment(&self, env_dir: &Path) -> Result<()> {
        eprintln!(
            "==> building {ENVIRONMENT_IMAGE_TAG} from {}",
            env_dir.display()
        );
        let mut args = HashMap::new();
        args.insert("BASE_IMAGE".to_string(), BASE_IMAGE_TAG.to_string());
        self.build_dir(env_dir, ENVIRONMENT_IMAGE_TAG, &args).await
    }

    /// Stack the layers for a tier and tag the result as
    /// [`tier_image_tag`].
    ///
    /// `layers` is the ordered list of `(name, dockerfile_dir)` to apply
    /// on top of the environment image. Empty list is allowed — the
    /// environment image itself becomes the tier image.
    pub async fn build_tier(
        &self,
        tier: &str,
        layers: &[(String, PathBuf)],
    ) -> Result<()> {
        let final_tag = tier_image_tag(tier);
        let mut prev = ENVIRONMENT_IMAGE_TAG.to_string();

        if layers.is_empty() {
            // Re-tag the environment as the tier image so callers always
            // resolve via `cbox-tier-<name>`.
            self.tag(&prev, &final_tag).await?;
            eprintln!("==> tagged {prev} as {final_tag}");
            return Ok(());
        }

        for (idx, (name, dir)) in layers.iter().enumerate() {
            let is_last = idx == layers.len() - 1;
            let tag = if is_last {
                final_tag.clone()
            } else {
                format!("cbox-tier-{tier}-stage-{idx}-{name}:latest")
            };
            eprintln!(
                "==> building {tag} (layer {name}, FROM {prev}) from {}",
                dir.display()
            );
            let mut args = HashMap::new();
            args.insert("BASE_IMAGE".to_string(), prev.clone());
            self.build_dir(dir, &tag, &args).await?;
            prev = tag;
        }
        Ok(())
    }

    async fn build_dir(
        &self,
        dir: &Path,
        tag: &str,
        build_args: &HashMap<String, String>,
    ) -> Result<()> {
        let context = pack_context(dir)
            .with_context(|| format!("tar build context at {}", dir.display()))?;

        let opts = BuildImageOptionsBuilder::default()
            .dockerfile("Dockerfile")
            .t(tag)
            .rm(true)
            .nocache(self.no_cache)
            .buildargs(
                &build_args
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect(),
            )
            .build();

        let body = bollard::body_full(Bytes::from(context));
        let mut stream = self.docker.build_image(opts, None, Some(body));

        while let Some(msg) = stream.next().await {
            let info = msg.with_context(|| format!("build {tag}"))?;
            if let Some(detail) = info.error_detail {
                let msg = detail.message.unwrap_or_else(|| "unknown error".to_string());
                bail!("docker build {tag} failed: {msg}");
            }
            if let Some(s) = info.stream {
                // Forward Docker's build output verbatim to stderr so
                // users see the same thing `docker build` would print.
                let s = s.trim_end_matches('\n');
                if !s.is_empty() {
                    eprintln!("{s}");
                }
            }
        }
        Ok(())
    }

    async fn tag(&self, source: &str, target: &str) -> Result<()> {
        let (repo, tag) = split_tag(target);
        let opts = bollard::query_parameters::TagImageOptionsBuilder::default()
            .repo(repo)
            .tag(tag)
            .build();
        self.docker
            .tag_image(source, Some(opts))
            .await
            .with_context(|| format!("tag {source} -> {target}"))?;
        Ok(())
    }
}

fn split_tag(image: &str) -> (&str, &str) {
    image.split_once(':').unwrap_or((image, "latest"))
}

/// Tar a directory into an in-memory build context.
///
/// Symlinks are followed (we copy the target's contents); special files
/// are skipped. Files are added with paths relative to `dir` so the
/// `Dockerfile` ends up at the root of the context.
fn pack_context(dir: &Path) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut buf);
        tar.follow_symlinks(true);
        append_dir_recursive(&mut tar, dir, dir)?;
        tar.finish().context("finalize tar")?;
    }
    Ok(buf)
}

fn append_dir_recursive<W: Write>(
    tar: &mut tar::Builder<W>,
    root: &Path,
    dir: &Path,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|e| anyhow!("strip_prefix: {e}"))?
            .to_path_buf();
        let ft = entry.file_type()?;

        if ft.is_dir() {
            append_dir_recursive(tar, root, &path)?;
        } else if ft.is_file() || ft.is_symlink() {
            let mut f = std::fs::File::open(&path)
                .with_context(|| format!("open {}", path.display()))?;
            let metadata = f.metadata()?;
            let mut header = tar::Header::new_gnu();
            header.set_metadata(&metadata);
            let mut data = Vec::with_capacity(metadata.len() as usize);
            f.read_to_end(&mut data)?;
            header.set_size(data.len() as u64);
            header.set_cksum();
            tar.append_data(&mut header, &rel, data.as_slice())
                .with_context(|| format!("append {}", rel.display()))?;
        }
        // Skip sockets, fifos, etc. — irrelevant for build contexts.
    }
    Ok(())
}

/// Plan the per-tier build (which layers, in what order, from what
/// directories). Pure: pulls everything from the parsed config so it's
/// unit-testable without Docker.
pub struct TierBuildPlan {
    pub tier: String,
    pub environment_dir: PathBuf,
    /// Ordered `(layer_name, dockerfile_dir)` to stack on the
    /// environment image.
    pub layers: Vec<(String, PathBuf)>,
}

impl TierBuildPlan {
    pub fn from_config(cfg: &Config, tier: &str) -> Result<Self> {
        let layer_names = cfg.effective_layers(tier)?;
        let mut layers = Vec::with_capacity(layer_names.len());
        for name in layer_names {
            let dir = cfg
                .layers
                .get(&name)
                .ok_or_else(|| anyhow!("layer {name:?} not declared under layers:"))?
                .as_path()
                .to_path_buf();
            layers.push((name, dir));
        }
        Ok(Self {
            tier: tier.to_string(),
            environment_dir: cfg.environment.as_path().to_path_buf(),
            layers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_image_tag_format() {
        assert_eq!(tier_image_tag("dev"), "cbox-tier-dev:latest");
    }

    #[test]
    fn split_tag_defaults_to_latest() {
        assert_eq!(split_tag("foo"), ("foo", "latest"));
        assert_eq!(split_tag("foo:bar"), ("foo", "bar"));
    }

    #[test]
    fn pack_context_includes_dockerfile() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Dockerfile"), b"FROM scratch\n").unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub/file.txt"), b"hi").unwrap();

        let bytes = pack_context(tmp.path()).unwrap();
        let mut ar = tar::Archive::new(bytes.as_slice());
        let names: Vec<String> = ar
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().display().to_string())
            .collect();
        assert!(names.contains(&"Dockerfile".to_string()), "{names:?}");
        assert!(names.contains(&"sub/file.txt".to_string()), "{names:?}");
    }

    #[test]
    fn plan_uses_default_layers_then_tier_layers() {
        let yaml = r#"
environment: /tmp/env
default_layers: [claude]
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
        let plan = TierBuildPlan::from_config(&cfg, "dev").unwrap();
        assert_eq!(plan.tier, "dev");
        assert_eq!(plan.environment_dir, PathBuf::from("/tmp/env"));
        let names: Vec<&str> = plan.layers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["claude", "python", "node"]);
    }
}
