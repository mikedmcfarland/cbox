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
use crate::config::{Config, repo_is_path};

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
/// When `arg` is `None`, the current working directory is canonicalized
/// and compared against the canonicalized `repo:` of each configured
/// project that is path-shaped (URL-style entries are skipped — they
/// have no host path to compare). A unique match wins; on no match or
/// ambiguity, the error message lists the candidates that were checked.
pub fn resolve_project(cfg: &Config, arg: Option<&str>) -> Result<ProjectSource> {
    let Some(arg) = arg else {
        return infer_project_from_cwd(cfg);
    };

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

/// Try to match the current working directory against a configured
/// project's `repo:`. Path-shaped repos are already canonicalized at
/// parse time (see `config::resolve_repo`), so this canonicalizes cwd
/// once and compares string-for-string. URL-style repos are skipped.
fn infer_project_from_cwd(cfg: &Config) -> Result<ProjectSource> {
    let cwd = std::env::current_dir().context("determine current directory")?;
    let cwd_canonical = cwd
        .canonicalize()
        .with_context(|| format!("canonicalize {}", cwd.display()))?;

    // Names of every path-shaped project we considered, regardless of
    // match outcome — surfaced in the error so the user can see which
    // entries were eligible.
    let mut considered: Vec<&str> = Vec::new();
    let mut matches: Vec<(&str, &crate::config::ProjectConfig)> = Vec::new();

    for (name, proj) in &cfg.projects {
        if !repo_is_path(&proj.repo) {
            continue;
        }
        considered.push(name.as_str());
        // `repo:` is canonicalized at parse time when the path exists;
        // if it didn't exist then, fall back to canonicalizing here so a
        // freshly-cloned repo still matches. Failures (e.g. the repo
        // path doesn't exist) simply skip the candidate.
        let repo_canonical = match PathBuf::from(&proj.repo).canonicalize() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if repo_canonical == cwd_canonical {
            matches.push((name.as_str(), proj));
        }
    }

    match matches.as_slice() {
        [(name, proj)] => Ok(ProjectSource::Configured {
            name: (*name).to_string(),
            repo: proj.repo.clone(),
            tier: proj.tier.clone(),
        }),
        [] => {
            let hint = if considered.is_empty() {
                String::from(
                    "no path-shaped projects in cbox.yaml to match against (URL-style \
                     entries are skipped)",
                )
            } else {
                format!("checked: {}", considered.join(", "))
            };
            bail!(
                "no project specified and cwd {} does not match any configured project; {hint}",
                cwd_canonical.display()
            )
        }
        many => {
            let names: Vec<&str> = many.iter().map(|(n, _)| *n).collect();
            bail!(
                "no project specified and cwd {} matches multiple configured projects: {}; \
                 pass an explicit project name to disambiguate",
                cwd_canonical.display(),
                names.join(", ")
            )
        }
    }
}

fn looks_like_path(s: &str) -> bool {
    s.starts_with('/') || s.starts_with("./") || s.starts_with("../") || s.starts_with('~')
}

/// `~/.cbox/workspaces/<tier>/` on the host (not created here).
pub fn tier_workspace_dir(tier: &str) -> Result<PathBuf> {
    validate_component("tier", tier)?;
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".cbox/workspaces").join(tier))
}

/// Reject anything that isn't a single normal path segment: tier names
/// come from config and session names from CLI args, and both get joined
/// into host paths. Without this guard, `cbox foo/../../../etc` would
/// canonicalise to an arbitrary location and `cbox destroy --workspace`
/// would happily `remove_dir_all` it.
fn validate_component(kind: &str, value: &str) -> Result<()> {
    use std::path::Component;
    let mut components = Path::new(value).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => bail!("{kind} must be a single path component: {value:?}"),
    }
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
    validate_component("session", session)?;
    Ok(tier_workspace_dir(tier)?.join(session))
}

/// Path the session's workspace appears at inside the container.
pub fn container_session_path(session: &str) -> Result<PathBuf> {
    validate_component("session", session)?;
    Ok(PathBuf::from(WORKSPACE_TARGET).join(session))
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
        // A killed or partial `git clone` can leave `.git` behind without
        // a valid worktree; reusing it would silently hand the user a
        // broken workspace forever. `rev-parse` is the cheapest probe
        // that fails on corrupt repos.
        if is_valid_git_checkout(&dir) {
            return Ok(dir);
        }
        bail!(
            "workspace {} contains an invalid git checkout; remove it with `cbox destroy --workspace`",
            dir.display()
        );
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

fn is_valid_git_checkout(dir: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git_clone(repo: &str, dir: &Path) -> Result<()> {
    let status = Command::new("git")
        .arg("clone")
        .arg(repo)
        .arg(dir)
        .status()
        .with_context(|| format!("invoke git clone {repo}"))?;
    if !status.success() {
        bail!(
            "git clone {repo} into {} exited with {status}",
            dir.display()
        );
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
        let cfg =
            synthetic_config("  my-app:\n    repo: git@github.com:org/my-app.git\n    tier: dev\n");
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
    fn missing_project_arg_with_no_match_errors_with_cwd_hint() {
        let cfg = synthetic_config("");
        let err = resolve_project(&cfg, None).unwrap_err().to_string();
        assert!(err.contains("cwd"), "{err}");
        assert!(err.contains("no path-shaped projects"), "{err}");
    }

    /// Helper for cwd-inference tests: build a config with one or more
    /// path-shaped projects pointing at tempdirs, run `resolve_project`
    /// with cwd set to `from_dir`, and return the result. cwd is mutated
    /// under the shared `home` serial lock — same convention as the
    /// other env-mutating tests in this file.
    fn run_with_cwd<R>(from_dir: &Path, f: impl FnOnce() -> R) -> R {
        struct CwdGuard(PathBuf);
        impl Drop for CwdGuard {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }
        let prev = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(from_dir).expect("chdir");
        let _g = CwdGuard(prev);
        f()
    }

    #[test]
    #[serial_test::serial(home)]
    fn missing_project_arg_infers_from_cwd_on_unique_match() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().canonicalize().unwrap();
        let extra = format!("  cbox:\n    repo: {}\n    tier: dev\n", repo_dir.display());
        let cfg = synthetic_config(&extra);
        let resolved = run_with_cwd(&repo_dir, || resolve_project(&cfg, None)).unwrap();
        match resolved {
            ProjectSource::Configured { name, repo, tier } => {
                assert_eq!(name, "cbox");
                assert_eq!(repo, repo_dir.to_string_lossy());
                assert_eq!(tier.as_deref(), Some("dev"));
            }
            _ => panic!("expected configured project"),
        }
    }

    #[test]
    #[serial_test::serial(home)]
    fn missing_project_arg_ambiguous_match_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().canonicalize().unwrap();
        // Two projects pointing at the same canonical path.
        let extra = format!(
            "  a:\n    repo: {0}\n    tier: dev\n  b:\n    repo: {0}\n    tier: dev\n",
            repo_dir.display()
        );
        let cfg = synthetic_config(&extra);
        let err = run_with_cwd(&repo_dir, || resolve_project(&cfg, None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("multiple configured projects"), "{err}");
        assert!(err.contains('a') && err.contains('b'), "{err}");
    }

    #[test]
    #[serial_test::serial(home)]
    fn missing_project_arg_no_match_lists_considered() {
        // One path-shaped project, but cwd is elsewhere — error should
        // mention the project name that was checked.
        let repo_tmp = tempfile::tempdir().unwrap();
        let cwd_tmp = tempfile::tempdir().unwrap();
        let repo_dir = repo_tmp.path().canonicalize().unwrap();
        let cwd_dir = cwd_tmp.path().canonicalize().unwrap();
        let extra = format!(
            "  myproj:\n    repo: {}\n    tier: dev\n",
            repo_dir.display()
        );
        let cfg = synthetic_config(&extra);
        let err = run_with_cwd(&cwd_dir, || resolve_project(&cfg, None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not match"), "{err}");
        assert!(err.contains("checked: myproj"), "{err}");
    }

    #[test]
    #[serial_test::serial(home)]
    fn missing_project_arg_skips_url_only_projects() {
        // Even when cwd is "the project," a URL-shaped `repo:` can't
        // match, so inference fails and the error notes that no
        // path-shaped projects were eligible.
        let tmp = tempfile::tempdir().unwrap();
        let cwd_dir = tmp.path().canonicalize().unwrap();
        let extra = concat!(
            "  ssh:\n    repo: git@github.com:org/app.git\n    tier: dev\n",
            "  bare:\n    repo: some-shorthand\n    tier: dev\n",
        );
        let cfg = synthetic_config(extra);
        let err = run_with_cwd(&cwd_dir, || resolve_project(&cfg, None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no path-shaped projects"), "{err}");
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
            container_session_path("auth-fix").unwrap(),
            PathBuf::from("/workspace/auth-fix")
        );
    }

    #[test]
    fn path_components_in_tier_or_session_are_rejected() {
        assert!(tier_workspace_dir("../escape").is_err());
        assert!(tier_workspace_dir("a/b").is_err());
        assert!(tier_workspace_dir("/abs").is_err());
        assert!(session_dir("dev", "..").is_err());
        assert!(session_dir("dev", "foo/bar").is_err());
        assert!(container_session_path("../etc").is_err());
    }

    /// Prepare workspaces against a real local git repo and assert each of
    /// the three branch shapes lands the checkout on the expected branch:
    /// no branch (inherits source HEAD), an existing branch (`checkout`
    /// succeeds via the clone's remote-tracking refs), and a new branch
    /// (`checkout` fails, fallback `checkout -b` succeeds). All three are
    /// exercised in a single test because they share a HOME swap; the
    /// `home` serial lock keeps this test from racing other crate-local
    /// tests that also mutate HOME.
    #[test]
    #[serial_test::serial(home)]
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
        // SAFETY: the `home` serial lock (above) serialises every
        // crate-local test that mutates HOME, so no other thread
        // observes HOME during this scope. RAII guard restores on panic.
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
        let dir2 =
            prepare_session_workspace(tier, "no-branch", &project, None).expect("re-run no-branch");
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
        String::from_utf8(out.stdout)
            .expect("utf8")
            .trim()
            .to_string()
    }
}
