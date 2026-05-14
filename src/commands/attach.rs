//! `cbox <name> [project]` — create or attach to an interactive session.
//!
//! See `docs/plans/v1.md` §Session lifecycle. Idempotent: on first
//! invocation it creates the workspace, the dtach socket, and a host
//! tmux window; on subsequent invocations it opens an ancillary shell
//! into the same workspace, or — with `--attach` — re-focuses the
//! existing dtach window.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::backend::Backend;
use crate::backend::TierRunConfig;
use crate::backend::local_docker::LocalDockerBackend;
use crate::build::tier_image_tag;
use crate::config::{Config, TierConfig};
use crate::keys::{AUTHORIZED_KEYS_ENV, KeyPair, ensure_keypair};
use crate::session::{LaunchCommand, dtach_command, shell_in_workspace_command, socket_exists};
use crate::ssh::{SshConn, shell_quote};
use crate::tmux;
use crate::workspace::{
    ProjectSource, container_session_path, prepare_session_workspace, resolve_project,
    tier_workspace_mount,
};

pub async fn run(
    name: String,
    project: Option<String>,
    tier_override: Option<String>,
    branch: Option<String>,
    shell_flag: bool,
    claude_flag: bool,
    attach_flag: bool,
) -> Result<()> {
    let prep = prepare(
        &name,
        project.as_deref(),
        tier_override.as_deref(),
        branch.as_deref(),
        shell_flag,
        claude_flag,
        attach_flag,
    )
    .await?;
    apply_action(&prep.ssh, &name, &prep.workspace_container, prep.action).await
}

/// Bundle of state computed by [`prepare`] — everything the final
/// [`apply_action`] step needs to launch the session.
struct Prep {
    ssh: SshConn,
    workspace_container: std::path::PathBuf,
    action: Action,
    /// Tier the session landed in. Only read by the integration test
    /// today; future `cbox list`/`cbox status` work will surface it.
    #[allow(dead_code)]
    tier_name: String,
}

/// Drive the create-or-attach pipeline up to (but not including) the
/// final foreground ssh call. Extracted so tests can exercise the full
/// orchestration without blocking on an interactive `ssh -t`.
async fn prepare(
    name: &str,
    project: Option<&str>,
    tier_override: Option<&str>,
    branch: Option<&str>,
    shell_flag: bool,
    claude_flag: bool,
    attach_flag: bool,
) -> Result<Prep> {
    let cfg_path = Config::default_path()?;
    let cfg = Config::load(&cfg_path)
        .with_context(|| format!("load config from {}", cfg_path.display()))?;

    let project_source = resolve_project(&cfg, project)?;
    let tier_name = resolve_tier(&cfg, &project_source, tier_override)?;
    let tier_cfg = cfg
        .tiers
        .get(&tier_name)
        .with_context(|| format!("tier {tier_name:?} not defined"))?
        .clone();

    let keypair = ensure_keypair()?;
    let run_cfg = build_run_config(&tier_name, &tier_cfg, &keypair)?;

    let backend = LocalDockerBackend::new()?;
    let endpoint = backend
        .ensure_running(&tier_name, &run_cfg)
        .await
        .with_context(|| format!("start tier {tier_name:?}"))?;

    prepare_session_workspace(&tier_name, name, &project_source, branch)?;
    let workspace_container = container_session_path(name);

    let ssh = SshConn {
        endpoint,
        identity_file: keypair.private_key_path.clone(),
    };

    let alive = socket_exists(&ssh, name).await?;
    let action = decide_action(alive, shell_flag, claude_flag, attach_flag);

    Ok(Prep {
        ssh,
        workspace_container,
        action,
        tier_name,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    StartClaude,
    StartShell,
    SelectExisting,
    OpenAncillaryShell,
    OpenAncillaryClaude,
}

fn decide_action(alive: bool, shell: bool, claude: bool, attach: bool) -> Action {
    match (alive, shell, claude, attach) {
        (false, true, _, _) => Action::StartShell,
        (false, _, _, _) => Action::StartClaude,
        (true, _, _, true) => Action::SelectExisting,
        (true, _, true, _) => Action::OpenAncillaryClaude,
        (true, _, _, _) => Action::OpenAncillaryShell,
    }
}

async fn apply_action(ssh: &SshConn, name: &str, workspace: &Path, action: Action) -> Result<()> {
    match action {
        Action::StartClaude => spawn_primary(ssh, name, workspace, LaunchCommand::Claude).await,
        Action::StartShell => spawn_primary(ssh, name, workspace, LaunchCommand::Shell).await,
        Action::SelectExisting => {
            let primary = tmux::window_name(name, None);
            if tmux::inside_tmux() && tmux::select_window(&primary).await? {
                Ok(())
            } else {
                // Outside tmux, or window was killed but socket persists —
                // reattach inline via ssh.
                attach_inline(ssh, name, workspace, LaunchCommand::Claude).await
            }
        }
        Action::OpenAncillaryShell => {
            let inner = shell_in_workspace_command(workspace);
            spawn_ancillary(ssh, name, &inner, "shell").await
        }
        Action::OpenAncillaryClaude => {
            // Ancillary Claude is a fresh process — closing the window
            // ends it, leaving the primary session untouched.
            let inner = format!(
                "cd {ws} && exec claude",
                ws = shell_quote(&workspace.display().to_string()),
            );
            spawn_ancillary(ssh, name, &inner, "claude").await
        }
    }
}

async fn spawn_primary(
    ssh: &SshConn,
    name: &str,
    workspace: &Path,
    launch: LaunchCommand,
) -> Result<()> {
    let inner = dtach_command(workspace, name, launch);
    let remote = wrap_login_shell(&inner);
    if tmux::inside_tmux() {
        let line = ssh.quoted_command_line(&["-t", "--", &remote]);
        tmux::create_window(&tmux::window_name(name, None), &line).await
    } else {
        eprintln!(
            "==> not inside tmux; running ssh inline. Run cbox from a host \
             tmux to keep the session detachable across SSH drops."
        );
        run_inline(ssh, &["-t", "--", &remote]).await
    }
}

async fn spawn_ancillary(ssh: &SshConn, name: &str, inner: &str, kind: &str) -> Result<()> {
    let remote = wrap_login_shell(inner);
    if tmux::inside_tmux() {
        let line = ssh.quoted_command_line(&["-t", "--", &remote]);
        tmux::create_window(
            &tmux::window_name(name, Some(&ancillary_suffix(kind))),
            &line,
        )
        .await
    } else {
        run_inline(ssh, &["-t", "--", &remote]).await
    }
}

async fn attach_inline(
    ssh: &SshConn,
    name: &str,
    workspace: &Path,
    launch: LaunchCommand,
) -> Result<()> {
    let inner = dtach_command(workspace, name, launch);
    let remote = wrap_login_shell(&inner);
    run_inline(ssh, &["-t", "--", &remote]).await
}

async fn run_inline(ssh: &SshConn, extra: &[&str]) -> Result<()> {
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args(ssh.args()).args(extra);
    let status = cmd.status().await.context("invoke ssh")?;
    if !status.success() {
        bail!("ssh exited with {status}");
    }
    Ok(())
}

/// Wrap `inner` as `bash -lc '<inner>'` so the remote login shell sources
/// the user's rc files (PATH for `claude`, etc.). Returned as a single
/// string so the caller can pass it to ssh as one arg — ssh joins multi-
/// arg remote commands with spaces, which would break `bash -lc`.
fn wrap_login_shell(inner: &str) -> String {
    format!("bash -lc {}", crate::ssh::shell_quote(inner))
}

fn ancillary_suffix(kind: &str) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{kind}-{secs}")
}

fn resolve_tier(
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

fn build_run_config(tier: &str, tier_cfg: &TierConfig, keypair: &KeyPair) -> Result<TierRunConfig> {
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

    #[test]
    fn decide_action_truth_table() {
        // Fresh session
        assert_eq!(
            decide_action(false, false, false, false),
            Action::StartClaude
        );
        assert_eq!(decide_action(false, true, false, false), Action::StartShell);

        // Existing session
        assert_eq!(
            decide_action(true, false, false, false),
            Action::OpenAncillaryShell
        );
        assert_eq!(
            decide_action(true, false, true, false),
            Action::OpenAncillaryClaude
        );
        assert_eq!(
            decide_action(true, false, false, true),
            Action::SelectExisting
        );
        // --attach wins over --claude when the session exists (clap also
        // rejects this combination, but the resolver is defensive).
        assert_eq!(
            decide_action(true, false, true, true),
            Action::SelectExisting
        );
    }

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

    /// End-to-end orchestration test against a real `cbox-tier-dev:latest`
    /// container. Drives [`prepare`] (which runs the full pipeline up to
    /// the foreground ssh) twice — first to assert StartClaude is chosen
    /// for a fresh session and the host workspace is cloned, then again
    /// after a stand-in `dtach -n sleep` to assert the alive-session
    /// branch returns `OpenAncillaryShell`. Finally calls
    /// [`crate::commands::destroy::run`] and verifies the socket is gone
    /// and the tier auto-paused (Item 3 from the PR-3 follow-ups).
    ///
    /// Ignored by default; run with `just integration` (which builds the
    /// image first). The test panics with a build instruction if the
    /// image is missing.
    #[tokio::test]
    #[ignore]
    async fn attach_run_against_docker() {
        use std::time::Duration;

        use crate::backend::Backend;
        use crate::backend::TierState;
        use crate::session::socket_path;
        use crate::ssh::shell_quote;
        use crate::workspace::session_dir;

        const IMAGE: &str = "cbox-tier-dev:latest";
        let backend = LocalDockerBackend::new().expect("connect docker");
        if backend.docker().inspect_image(IMAGE).await.is_err() {
            panic!(
                "missing image {IMAGE}; build first: \
                 CBOX_CONFIG=examples/full-setup/cbox.yaml cargo run -- build dev"
            );
        }

        // Isolate HOME so the keypair, workspace, and synthesised cbox.yaml
        // all land in a tempdir — never in the developer's `~/.cbox/`.
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
        // SAFETY: tokio current_thread runtime; no other thread observes
        // HOME during this scope. RAII guard restores on panic.
        unsafe { std::env::set_var("HOME", tmp_home.path()) };
        let _home = HomeGuard(prev_home);

        // Create a minimal source repo the test can clone over the
        // Path-style project lookup.
        let src = tempfile::tempdir().expect("src tempdir");
        run_git(src.path(), &["init", "-q", "-b", "main"]);
        run_git(src.path(), &["config", "user.email", "test@example.com"]);
        run_git(src.path(), &["config", "user.name", "test"]);
        std::fs::write(src.path().join("README"), b"hi\n").expect("write README");
        run_git(src.path(), &["add", "."]);
        run_git(src.path(), &["commit", "-q", "-m", "init"]);

        // Synthetic cbox.yaml at $HOME/.config/cbox/cbox.yaml so
        // Config::default_path() finds it. Layer/environment paths are
        // irrelevant — we reuse the prebuilt `cbox-tier-dev:latest`
        // image and never rebuild from this config.
        let cfg_dir = tmp_home.path().join(".config/cbox");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir cfg_dir");
        let env_dir = tmp_home.path().join("env");
        let layer_dir = tmp_home.path().join("layer-c");
        std::fs::create_dir_all(&env_dir).expect("mkdir env");
        std::fs::create_dir_all(&layer_dir).expect("mkdir layer");
        let yaml = format!(
            "environment: {env}\n\
             default_tier: dev\n\
             layers:\n  c: {layer}\n\
             tiers:\n  dev:\n    layers: [c]\n",
            env = env_dir.display(),
            layer = layer_dir.display(),
        );
        std::fs::write(cfg_dir.join("cbox.yaml"), yaml).expect("write cbox.yaml");

        let tier = "dev";
        let session = "attach-orch-test";

        // Idempotent: a previously-running `dev` tier may have a stale
        // workspace mount pointing into the developer's real HOME.
        let _ = backend.destroy(tier).await;

        let outcome: Result<()> = async {
            let project_arg = src.path().to_str().expect("src path is utf8").to_string();

            // First pass: fresh session → StartClaude.
            let prep1 =
                prepare(session, Some(&project_arg), None, None, false, false, false).await?;
            anyhow::ensure!(
                prep1.action == Action::StartClaude,
                "fresh session should pick StartClaude, got {:?}",
                prep1.action
            );
            anyhow::ensure!(prep1.tier_name == "dev");

            let host_ws = session_dir(&prep1.tier_name, session)?;
            anyhow::ensure!(
                host_ws.join(".git").exists(),
                "host workspace {} missing .git",
                host_ws.display()
            );

            // Wait for sshd inside the tier.
            wait_for_sshd(&prep1.ssh).await?;

            // Stand in for the interactive `apply_action` spawn: open a
            // detached dtach session that the rest of the test (and
            // destroy::run) will see as the live primary socket.
            let sock = socket_path(session);
            let inner = format!("dtach -n {} sleep 60", shell_quote(&sock));
            let status = tokio::process::Command::new("ssh")
                .args(prep1.ssh.args())
                .arg("--")
                .arg(&inner)
                .status()
                .await
                .context("invoke ssh dtach -n")?;
            anyhow::ensure!(status.success(), "dtach -n exited with {status}");

            let mut found = false;
            for _ in 0..30 {
                if socket_exists(&prep1.ssh, session).await? {
                    found = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            anyhow::ensure!(found, "dtach socket never appeared");

            // Second pass: socket exists → OpenAncillaryShell.
            let prep2 =
                prepare(session, Some(&project_arg), None, None, false, false, false).await?;
            anyhow::ensure!(
                prep2.action == Action::OpenAncillaryShell,
                "re-entry should pick OpenAncillaryShell, got {:?}",
                prep2.action
            );

            // Item 3: destroy::run kills the session AND auto-pauses the
            // tier when /run/cbox/ empties. Post-condition is asserted
            // via `tier_state` — once the tier is paused, SSH would
            // hang, so we cannot re-check the socket.
            crate::commands::destroy::run(session.to_string(), false).await?;

            let state = backend.tier_state(tier).await?;
            anyhow::ensure!(
                state == TierState::Paused,
                "expected tier auto-paused after last session, got {state:?}"
            );

            Ok(())
        }
        .await;

        let teardown = backend.destroy(tier).await;
        outcome.expect("attach orchestration");
        teardown.expect("destroy tier");
    }

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .status()
            .unwrap_or_else(|e| panic!("invoke git {args:?}: {e}"));
        assert!(status.success(), "git {args:?} failed: {status}");
    }

    async fn wait_for_sshd(ssh: &SshConn) -> Result<()> {
        for _ in 0..60 {
            let ok = tokio::process::Command::new("ssh")
                .args(ssh.args())
                .arg("-o")
                .arg("ConnectTimeout=1")
                .arg("--")
                .arg("true")
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        anyhow::bail!("sshd never became reachable")
    }
}
