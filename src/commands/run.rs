//! `cbox run <name> [project] <prompt>` — autonomous (headless) Claude.
//!
//! Same bring-up as the interactive path (ensure tier running, prepare
//! workspace, ensure keypair), but instead of opening a host-tmux window
//! we ssh in and spawn `dtach -n <sock> claude -p '<prompt>'`. The `-n`
//! flag detaches immediately: ssh returns, the user gets their terminal
//! back, and `claude -p` continues in the pty buffer. A subsequent
//! `cbox <name>` attaches to that same socket.
//!
//! Refuses to start over a live session — the user has to `cbox destroy
//! <name>` first. That keeps `run` non-clobbering; the interactive
//! `cbox <name>` path is where the "open another shell into the same
//! session" semantics live.

use anyhow::{Context, Result, bail};

use crate::backend::Backend;
use crate::backend::local_docker::LocalDockerBackend;
use crate::commands::common::{build_run_config, resolve_tier};
use crate::config::Config;
use crate::keys::ensure_keypair;
use crate::session::{is_alive, socket_path};
use crate::ssh::{SshConn, shell_quote};
use crate::workspace::{container_session_path, prepare_session_workspace, resolve_project};

pub async fn run(
    name: String,
    project: Option<String>,
    prompt: String,
    tier_override: Option<String>,
) -> Result<()> {
    if prompt.trim().is_empty() {
        bail!("prompt is empty; pass a non-empty prompt to `cbox run`");
    }

    let cfg_path = Config::default_path()?;
    let cfg = Config::load(&cfg_path)
        .with_context(|| format!("load config from {}", cfg_path.display()))?;

    let project_source = {
        let cfg = cfg.clone();
        let project = project.clone();
        tokio::task::spawn_blocking(move || resolve_project(&cfg, project.as_deref()))
            .await
            .context("join resolve_project task")??
    };
    let tier_name = resolve_tier(&cfg, &project_source, tier_override.as_deref())?;
    let tier_cfg = cfg
        .tiers
        .get(&tier_name)
        .with_context(|| format!("tier {tier_name:?} not defined"))?
        .clone();

    let keypair = tokio::task::spawn_blocking(ensure_keypair)
        .await
        .context("join ensure_keypair task")??;
    let run_cfg = build_run_config(&tier_name, &tier_cfg, &keypair)?;

    let backend = LocalDockerBackend::new()?;
    let endpoint = backend
        .ensure_running(&tier_name, &run_cfg)
        .await
        .with_context(|| format!("start tier {tier_name:?}"))?;

    {
        let tier_name = tier_name.clone();
        let session = name.clone();
        let project_source = project_source.clone();
        tokio::task::spawn_blocking(move || {
            prepare_session_workspace(&tier_name, &session, &project_source, None)
        })
        .await
        .context("join prepare_session_workspace task")??;
    }
    let workspace = container_session_path(&name)?;

    let ssh = SshConn {
        endpoint,
        identity_file: keypair.private_key_path.clone(),
    };

    if is_alive(&ssh, &name).await? {
        bail!(
            "session {name:?} is already live in tier {tier_name:?}; \
             destroy it first (`cbox destroy {name}`) before re-running"
        );
    }

    // `dtach -n <sock> claude -p <prompt>` — detached spawn, returns
    // immediately. claude's output collects in the pty buffer until the
    // user attaches via `cbox <name>`.
    //
    // Whole thing passed as one ssh arg so the remote shell parses our
    // quoting intact (sshd would otherwise space-join multi-arg into
    // `bash -c <first-word>`).
    let inner = format!(
        "cd {ws} && exec dtach -n {sock} claude -p {prompt}",
        ws = shell_quote(&workspace.display().to_string()),
        sock = shell_quote(&socket_path(&name)),
        prompt = shell_quote(&prompt),
    );
    let remote = format!("bash -lc {}", shell_quote(&inner));

    let status = tokio::process::Command::new("ssh")
        .args(ssh.args())
        .arg("--")
        .arg(&remote)
        .status()
        .await
        .context("invoke ssh to spawn autonomous claude")?;
    if !status.success() {
        bail!("ssh exited with {status} while spawning autonomous session");
    }

    eprintln!(
        "==> autonomous session {name:?} started in tier {tier_name:?}; \
         attach with `cbox {name}`"
    );
    Ok(())
}
