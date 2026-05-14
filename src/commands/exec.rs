//! `cbox exec <name> -- <cmd...>` — one-off command in a session's workspace.
//!
//! Finds the tier that holds the named session (via [`session::is_alive`]),
//! then ssh's in and runs the command under the session's workspace
//! directory. The remote exit code is propagated to the caller via
//! [`std::process::exit`] so pipelines and `$?` work naturally.

use std::io::IsTerminal;

use anyhow::{Context, Result, bail};

use crate::backend::Backend;
use crate::backend::TierState;
use crate::backend::local_docker::LocalDockerBackend;
use crate::config::Config;
use crate::keys::ensure_keypair;
use crate::session::is_alive;
use crate::ssh::{SshConn, shell_quote};
use crate::workspace::container_session_path;

pub async fn run(name: String, cmd: Vec<String>) -> Result<()> {
    if cmd.is_empty() {
        // Should already be caught by clap (`required = true`), but guard
        // anyway so a misuse from a test surface doesn't silently spawn a
        // login shell.
        bail!("no command given to `cbox exec`");
    }
    let cfg_path = Config::default_path()?;
    let cfg = Config::load(&cfg_path)
        .with_context(|| format!("load config from {}", cfg_path.display()))?;

    let keypair = tokio::task::spawn_blocking(ensure_keypair)
        .await
        .context("join ensure_keypair task")??;
    let backend = LocalDockerBackend::new()?;

    let mut found_tier_paused = false;
    let mut ssh_for_session: Option<SshConn> = None;
    for tier_name in cfg.tiers.keys() {
        let state = backend
            .tier_state(tier_name)
            .await
            .with_context(|| format!("inspect tier {tier_name:?}"))?;
        if state == TierState::Paused {
            // Can't query a paused tier for sockets. Record so we can
            // emit a more helpful error if no running tier has it.
            found_tier_paused = true;
            continue;
        }
        if state != TierState::Running {
            continue;
        }
        let Some(endpoint) = backend.endpoint(tier_name).await? else {
            continue;
        };
        let ssh = SshConn {
            endpoint,
            identity_file: keypair.private_key_path.clone(),
        };
        if is_alive(&ssh, &name).await? {
            ssh_for_session = Some(ssh);
            break;
        }
    }

    let ssh = match ssh_for_session {
        Some(s) => s,
        None => {
            if found_tier_paused {
                bail!(
                    "no live session {name:?} in any running tier \
                     (a paused tier may hold it — resume the tier or run \
                     `cbox <name>` to attach)"
                );
            }
            bail!("no live session {name:?}");
        }
    };

    let workspace = container_session_path(&name)?;
    let quoted_cmd: Vec<String> = cmd.iter().map(|s| shell_quote(s)).collect();
    let inner = format!(
        "cd {ws} && exec {cmd}",
        ws = shell_quote(&workspace.display().to_string()),
        cmd = quoted_cmd.join(" "),
    );
    // bash -lc so PATH (claude, layer tools) is sourced. One ssh arg so
    // sshd doesn't space-join the script and break bash -lc.
    let remote = format!("bash -lc {}", shell_quote(&inner));

    let mut ssh_cmd = tokio::process::Command::new("ssh");
    ssh_cmd.args(ssh.args());
    if std::io::stdin().is_terminal() {
        // Interactive command (e.g. `cbox exec foo vim file`): allocate a
        // pty so cursor control / job control work.
        ssh_cmd.arg("-t");
    }
    ssh_cmd.arg("--").arg(&remote);

    let status = ssh_cmd.status().await.context("invoke ssh exec")?;
    if let Some(code) = status.code() {
        if code != 0 {
            std::process::exit(code);
        }
        Ok(())
    } else {
        // Killed by signal — propagate as exit 128 + signum convention.
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(sig) = status.signal() {
                std::process::exit(128 + sig);
            }
        }
        bail!("ssh terminated abnormally");
    }
}
