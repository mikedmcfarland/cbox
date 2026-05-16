//! `cbox auth <tier>` — one-time interactive setup for OAuth-based MCPs.
//!
//! OAuth MCPs (Notion, Linear, Slack, ...) need a browser round-trip to
//! mint tokens that Claude Code stores in the per-tier `.claude` volume.
//! This command brings up the tier and opens an interactive Claude
//! session in `$HOME` so the user can drive
//! `/login`, `/mcp`, `claude mcp add notion ...`, etc., and walk through
//! whichever OAuth flow the MCP server prompts for.
//!
//! Unlike `cbox <name>`, auth:
//! - never registers as a session in `/run/cbox/` (so it doesn't keep
//!   the tier instance "busy" for auto-pause / cleanup purposes),
//! - skips the per-session workspace clone (no project arg, no
//!   `prepare_session_workspace` call — auth runs in `$HOME`),
//! - uses no dtach socket (it's a one-shot setup, not a long-running
//!   session — exiting the agent ends the flow).
//!
//! The tier-level workspace bind mount at `/workspace/` is still
//! attached — that's per-tier plumbing every container shares, and
//! the host-side parent dir is reused by any later sessions. Auth
//! just doesn't put anything there.
//!
//! The OAuth tokens themselves land on the per-tier `.claude` named
//! volume (see [`crate::commands::common::CLAUDE_STATE_TARGET`]), so
//! they survive image rebuilds.

use anyhow::{Context, Result, bail};

use crate::backend::Backend;
use crate::backend::local_docker::LocalDockerBackend;
use crate::commands::common::build_run_config;
use crate::config::Config;
use crate::credentials::OnePasswordResolver;
use crate::keys::ensure_keypair;
use crate::ssh::{SshConn, shell_quote, wait_for_sshd};
use crate::tmux;

pub async fn run(tier: String) -> Result<()> {
    let cfg_path = Config::default_path()?;
    let cfg = Config::load_async(cfg_path).await?;
    let tier_cfg = cfg
        .tiers
        .get(&tier)
        .with_context(|| format!("tier {tier:?} not defined in cbox.yaml"))?
        .clone();

    let keypair = tokio::task::spawn_blocking(ensure_keypair)
        .await
        .context("join ensure_keypair task")??;
    let resolver = OnePasswordResolver;
    let run_cfg = build_run_config(&tier, &cfg, &tier_cfg, &keypair, &resolver).await?;

    let backend = LocalDockerBackend::new()?;
    let endpoint = backend
        .ensure_running(&tier, &run_cfg)
        .await
        .with_context(|| format!("start tier {tier:?}"))?;
    let ssh = SshConn {
        endpoint,
        identity_file: keypair.private_key_path.clone(),
    };
    wait_for_sshd(&ssh, std::time::Duration::from_secs(60))
        .await
        .context("wait for tier sshd")?;

    // Land in $HOME so `claude mcp add` writes into the persistent
    // .claude volume — the workspace dir would be wrong both
    // semantically (auth is per-tier, not per-session) and on disk
    // (it lives outside the .claude volume).
    let agent = &tier_cfg.agent.command;
    let inner = format!("cd ~ && exec {agent}");
    let remote = format!("bash -lc {}", shell_quote(&inner));

    eprintln!(
        "==> opening interactive {agent:?} in tier {tier:?} \
         for OAuth MCP setup (exit the agent to finish)"
    );

    if tmux::inside_tmux() {
        let window = format!("cbox-auth:{tier}");
        let line = ssh.quoted_command_line(&["-t", "--", &remote]);
        tmux::create_window(&window, &line).await
    } else {
        run_inline(&ssh, &remote).await
    }
}

async fn run_inline(ssh: &SshConn, remote: &str) -> Result<()> {
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args(ssh.args()).args(["-t", "--", remote]);
    let status = cmd.status().await.context("invoke ssh")?;
    if !status.success() {
        bail!("ssh exited with {status}");
    }
    Ok(())
}
