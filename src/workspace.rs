//! Host-side workspace plumbing.
//!
//! Each tier instance has a single workspace directory bind-mounted from
//! `~/.cbox/workspaces/<tier>/` on the host to `/workspace/` inside the
//! container. Per session, cbox creates a sub-directory (and clones the
//! chosen repo) at `~/.cbox/workspaces/<tier>/<session>/`, which the
//! container sees at `/workspace/<session>/`.
//!
//! Working from the host avoids two distractions: editors get direct
//! access to the checkout without reaching through SSH, and `git clone`
//! uses the user's existing keys/credentials instead of needing the
//! container to be auth'd for git remotes.

// Wired by the attach command in a later Phase 2 commit.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::backend::{Mount, MountSource};
use crate::config::Config;

/// Path inside the container where the tier workspace is mounted.
pub const WORKSPACE_TARGET: &str = "/workspace";

/// Where the session's git checkout came from. The `Path` variant uses
/// the local filesystem path as the git URL (`git clone` accepts local
/// paths and converts them into shared-object-store clones automatically).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectSource {
    Configured {
        name: String,
        repo: String,
        tier: Option<String>,
    },
    Path(PathBuf),
}

/// Resolve a `cbox <name> [project]` project argument against cbox.yaml.
///
/// `arg` heuristic:
/// - Starts with `/`, `./`, `../`, or `~` → filesystem path (must exist).
/// - Anything else → look up in `cbox.yaml` `projects:`.
///
/// Phase 2 does not yet support implicit project resolution from the
/// current working directory; the caller must pass something.
pub fn resolve_project(cfg: &Config, arg: Option<&str>) -> Result<ProjectSource> {
    let arg = arg.ok_or_else(|| {
        anyhow::anyhow!("no project specified (provide a project name or path)")
    })?;

    if looks_like_path(arg) {
        let expanded = PathBuf::from(shellexpand::tilde(arg).into_owned());
        if !expanded.exists() {
            bail!("project path {} does not exist", expanded.display());
        }
        let canonical = expanded
            .canonicalize()
            .with_context(|| format!("canonicalize {}", expanded.display()))?;
        return Ok(ProjectSource::Path(canonical));
    }

    if let Some(p) = cfg.projects.get(arg) {
        return Ok(ProjectSource::Configured {
            name: arg.to_string(),
            repo: p.repo.clone(),
            tier: p.tier.clone(),
        });
    }

    bail!(
        "no project named {arg:?} in cbox.yaml; pass a path (./{arg} or /abs) to skip the lookup"
    );
}

fn looks_like_path(s: &str) -> bool {
    s.starts_with('/') || s.starts_with("./") || s.starts_with("../") || s.starts_with('~')
}

/// `~/.cbox/workspaces/<tier>/` on the host (not created here).
pub fn tier_workspace_dir(tier: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".cbox/workspaces").join(tier))
}

pub fn ensure_tier_workspace_dir(tier: &str) -> Result<PathBuf> {
    let dir = tier_workspace_dir(tier)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

/// Bind-mount entry to pass to the backend at tier-instance create time.
pub fn tier_workspace_mount(tier: &str) -> Result<Mount> {
    let host = ensure_tier_workspace_dir(tier)?;
    Ok(Mount {
        source: MountSource::HostPath(host),
        target: PathBuf::from(WORKSPACE_TARGET),
        read_only: false,
    })
}

pub fn session_dir(tier: &str, session: &str) -> Result<PathBuf> {
    Ok(tier_workspace_dir(tier)?.join(session))
}

/// Path the session's workspace appears at inside the container.
pub fn container_session_path(session: &str) -> PathBuf {
    PathBuf::from(WORKSPACE_TARGET).join(session)
}

/// Populate `<tier>/<session>/` by cloning `project`. Idempotent — if
/// the directory already contains a `.git` entry the function returns
/// immediately and `branch` is **not** re-checked (re-attach intentionally
/// inherits whatever the user did in the session). If `branch` is
/// provided and the workspace is fresh, cbox checks it out (creating it
/// from HEAD if it doesn't already exist).
pub fn prepare_session_workspace(
    tier: &str,
    session: &str,
    project: &ProjectSource,
    branch: Option<&str>,
) -> Result<PathBuf> {
    let dir = session_dir(tier, session)?;
    if dir.join(".git").exists() {
        return Ok(dir);
    }
    if dir.exists() {
        bail!(
            "workspace {} exists but is not a git checkout",
            dir.display()
        );
    }

    ensure_tier_workspace_dir(tier)?;

    let repo = match project {
        ProjectSource::Configured { repo, .. } => repo.clone(),
        ProjectSource::Path(p) => p.display().to_string(),
    };

    git_clone(&repo, &dir)?;
    if let Some(b) = branch {
        git_checkout(&dir, b)?;
    }
    Ok(dir)
}

fn git_clone(repo: &str, dir: &Path) -> Result<()> {
    let status = Command::new("git")
        .arg("clone")
        .arg(repo)
        .arg(dir)
        .status()
        .with_context(|| format!("invoke git clone {repo}"))?;
    if !status.success() {
        bail!("git clone {repo} into {} exited with {status}", dir.display());
    }
    Ok(())
}

fn git_checkout(dir: &Path, branch: &str) -> Result<()> {
    // Try existing branch first; fall back to creating one from HEAD.
    let existing = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("checkout")
        .arg(branch)
        .status()
        .with_context(|| format!("invoke git checkout {branch}"))?;
    if existing.success() {
        return Ok(());
    }

    let created = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("checkout")
        .arg("-b")
        .arg(branch)
        .status()
        .with_context(|| format!("invoke git checkout -b {branch}"))?;
    if !created.success() {
        bail!("git checkout -b {branch} exited with {created}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_config(extra: &str) -> Config {
        let yaml = format!(
            r#"
environment: /tmp/env
layers:
  c: /tmp/c
tiers:
  dev:
    layers: [c]
projects:
{extra}
"#
        );
        serde_yaml_bw::from_str(&yaml).expect("yaml")
    }

    #[test]
    fn resolves_configured_project_by_name() {
        let cfg = synthetic_config(
            "  my-app:\n    repo: git@github.com:org/my-app.git\n    tier: dev\n",
        );
        let resolved = resolve_project(&cfg, Some("my-app")).unwrap();
        match resolved {
            ProjectSource::Configured { name, repo, tier } => {
                assert_eq!(name, "my-app");
                assert_eq!(repo, "git@github.com:org/my-app.git");
                assert_eq!(tier.as_deref(), Some("dev"));
            }
            _ => panic!("expected configured project"),
        }
    }

    #[test]
    fn unknown_project_name_is_an_error() {
        let cfg = synthetic_config("");
        let err = resolve_project(&cfg, Some("nope")).unwrap_err().to_string();
        assert!(err.contains("no project named"), "{err}");
    }

    #[test]
    fn missing_project_arg_is_an_error() {
        let cfg = synthetic_config("");
        assert!(resolve_project(&cfg, None).is_err());
    }

    #[test]
    fn resolves_absolute_path() {
        let cfg = synthetic_config("");
        let tmp = tempfile::tempdir().unwrap();
        let resolved = resolve_project(&cfg, Some(tmp.path().to_str().unwrap())).unwrap();
        match resolved {
            ProjectSource::Path(p) => assert_eq!(p, tmp.path().canonicalize().unwrap()),
            _ => panic!("expected path"),
        }
    }

    #[test]
    fn nonexistent_path_is_an_error() {
        let cfg = synthetic_config("");
        let err = resolve_project(&cfg, Some("/definitely/not/here/cbox-test"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn looks_like_path_detects_common_prefixes() {
        assert!(looks_like_path("/abs"));
        assert!(looks_like_path("./rel"));
        assert!(looks_like_path("../rel"));
        assert!(looks_like_path("~/home"));
        assert!(!looks_like_path("my-project"));
        assert!(!looks_like_path("project"));
    }

    #[test]
    fn container_session_path_is_under_workspace() {
        assert_eq!(
            container_session_path("auth-fix"),
            PathBuf::from("/workspace/auth-fix")
        );
    }

    /// Prepare workspaces against a real local git repo and assert each of
    /// the three branch shapes lands the checkout on the expected branch:
    /// no branch (inherits source HEAD), an existing branch (`checkout`
    /// succeeds via the clone's remote-tracking refs), and a new branch
    /// (`checkout` fails, fallback `checkout -b` succeeds). All three are
    /// exercised in a single test because they share a HOME swap and
    /// cargo runs tests in parallel — separate functions would race.
    #[test]
    fn prepare_session_workspace_covers_branch_shapes() {
        // Snapshot HOME so a panic doesn't leak the tempdir into the
        // developer's real `~/.cbox/`.
        struct HomeGuard(Option<std::ffi::OsString>);
        impl Drop for HomeGuard {
            fn drop(&mut self) {
                unsafe {
                    match self.0.take() {
                        Some(v) => std::env::set_var("HOME", v),
                        None => std::env::remove_var("HOME"),
                    }
                }
            }
        }

        let tmp_home = tempfile::tempdir().expect("home tempdir");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: this test is single-threaded with respect to HOME within
        // its own scope; the RAII guard restores HOME on panic. Other
        // tests in this binary that also swap HOME synchronise via being
        // the only consumer in their crate-local scope (see
        // `keys::tests::ensure_keypair_generates_on_first_call`).
        unsafe { std::env::set_var("HOME", tmp_home.path()) };
        let _home = HomeGuard(prev_home);

        // Build a source repo with `main` checked out and an extra branch
        // `existing-feature` pointing at HEAD. `git clone` from a local
        // path picks up both as remote-tracking refs; the DWIM behaviour
        // of `git checkout existing-feature` then creates the local
        // branch on demand.
        let src = tempfile::tempdir().expect("src tempdir");
        run_git(src.path(), &["init", "-q", "-b", "main"]);
        run_git(src.path(), &["config", "user.email", "test@example.com"]);
        run_git(src.path(), &["config", "user.name", "test"]);
        std::fs::write(src.path().join("README"), b"hello\n").expect("write README");
        run_git(src.path(), &["add", "."]);
        run_git(src.path(), &["commit", "-q", "-m", "init"]);
        run_git(src.path(), &["branch", "existing-feature"]);

        let tier = "branch-test";
        let project = ProjectSource::Path(src.path().to_path_buf());

        // Case 1: no branch arg → inherits source HEAD (`main`).
        let dir =
            prepare_session_workspace(tier, "no-branch", &project, None).expect("clone no-branch");
        assert_eq!(head_branch(&dir), "main");

        // Idempotency: re-running with the same session is a no-op.
        let dir2 = prepare_session_workspace(tier, "no-branch", &project, None)
            .expect("re-run no-branch");
        assert_eq!(dir, dir2);

        // Case 2: existing branch → `checkout` succeeds first try.
        let dir = prepare_session_workspace(tier, "existing", &project, Some("existing-feature"))
            .expect("clone existing");
        assert_eq!(head_branch(&dir), "existing-feature");

        // Case 3: new branch → `checkout` fails, falls back to `checkout -b`.
        let dir = prepare_session_workspace(tier, "fresh", &project, Some("fresh-branch"))
            .expect("clone fresh");
        assert_eq!(head_branch(&dir), "fresh-branch");
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .status()
            .unwrap_or_else(|e| panic!("invoke git {args:?}: {e}"));
        assert!(status.success(), "git {args:?} failed: {status}");
    }

    fn head_branch(dir: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("git rev-parse");
        assert!(out.status.success(), "rev-parse failed: {:?}", out);
        String::from_utf8(out.stdout).expect("utf8").trim().to_string()
    }
}
