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
use crate::backend::local_docker::LocalDockerBackend;
use crate::backend::TierRunConfig;
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
    let cfg_path = Config::default_path()?;
    let cfg = Config::load(&cfg_path)
        .with_context(|| format!("load config from {}", cfg_path.display()))?;

    let project_source = resolve_project(&cfg, project.as_deref())?;
    let tier_name = resolve_tier(&cfg, &project_source, tier_override.as_deref())?;
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

    prepare_session_workspace(&tier_name, &name, &project_source, branch.as_deref())?;
    let workspace_container = container_session_path(&name);

    let ssh = SshConn {
        endpoint,
        identity_file: keypair.private_key_path.clone(),
    };

    let alive = socket_exists(&ssh, &name).await?;
    let action = decide_action(alive, shell_flag, claude_flag, attach_flag);

    apply_action(&ssh, &name, &workspace_container, action).await
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
        tmux::create_window(&tmux::window_name(name, Some(&ancillary_suffix(kind))), &line).await
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
        assert_eq!(decide_action(false, false, false, false), Action::StartClaude);
        assert_eq!(decide_action(false, true, false, false), Action::StartShell);

        // Existing session
        assert_eq!(decide_action(true, false, false, false), Action::OpenAncillaryShell);
        assert_eq!(decide_action(true, false, true, false), Action::OpenAncillaryClaude);
        assert_eq!(decide_action(true, false, false, true), Action::SelectExisting);
        // --attach wins over --claude when the session exists (clap also
        // rejects this combination, but the resolver is defensive).
        assert_eq!(decide_action(true, false, true, true), Action::SelectExisting);
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
}
