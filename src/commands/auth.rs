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
//!
//! ## OAuth callback forwarding (ADR 019)
//!
//! Anthropic's `/login` and OAuth-MCP flows print a `localhost:<port>`
//! `redirect_uri` and expect a host browser to land on it. Without
//! port forwarding, the user copies the resulting `?code=...` out of
//! the broken redirect page and pastes it back into the terminal.
//! With `--forward-port` (defaulting to `54545`, Anthropic's known
//! callback port), ssh forwards loopback->loopback so the browser
//! redirect completes silently. If the host port is busy, OpenSSH
//! prints `bind: Address already in use` and the session continues
//! with the manual-paste fallback intact.

use anyhow::{Context, Result, bail};

use crate::backend::Backend;
use crate::backend::local_docker::LocalDockerBackend;
use crate::commands::common::build_run_config;
use crate::config::Config;
use crate::credentials::OnePasswordResolver;
use crate::keys::ensure_keypair;
use crate::ssh::{SshConn, shell_quote, wait_for_sshd};
use crate::tmux;

/// Anthropic CLI `/login` OAuth callback port. Verifiable any time by
/// running `/login` and reading the `redirect_uri` query parameter in
/// the printed authorization URL — no token paste needed. See ADR 019.
pub const DEFAULT_OAUTH_CALLBACK_PORT: u16 = 54545;

/// Decide which loopback ports `cbox auth` should forward into the tier.
///
/// Precedence: explicit `--forward-port` wins; `--no-forward-port`
/// disables; otherwise the Anthropic `/login` default
/// ([`DEFAULT_OAUTH_CALLBACK_PORT`]) is forwarded so the common case
/// "just works". Duplicates are dropped (ssh would warn anyway).
pub fn resolve_forward_ports(forward_port: Vec<u16>, no_forward_port: bool) -> Vec<u16> {
    if no_forward_port {
        return Vec::new();
    }
    let mut ports = if forward_port.is_empty() {
        vec![DEFAULT_OAUTH_CALLBACK_PORT]
    } else {
        forward_port
    };
    // Stable de-dup: preserves first-seen order so the log line
    // matches the user's `--forward-port` order.
    let mut seen = std::collections::HashSet::new();
    ports.retain(|p| seen.insert(*p));
    ports
}

pub async fn run(tier: String, forward_port: Vec<u16>, no_forward_port: bool) -> Result<()> {
    let cfg = Config::load_async().await?;
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
    let forward_ports = resolve_forward_ports(forward_port, no_forward_port);
    for port in &forward_ports {
        eprintln!(
            "==> forwarding localhost:{port} -> tier loopback \
             (for OAuth callback; if the port is busy on this host, \
             OAuth still works via manual code paste)"
        );
    }
    let ssh = SshConn {
        endpoint,
        identity_file: keypair.private_key_path.clone(),
        forward_ports,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_forward_ports_defaults_to_anthropic_callback_port() {
        let ports = resolve_forward_ports(Vec::new(), false);
        assert_eq!(ports, vec![DEFAULT_OAUTH_CALLBACK_PORT]);
    }

    #[test]
    fn resolve_forward_ports_no_forward_wins() {
        let ports = resolve_forward_ports(vec![9999], true);
        assert!(
            ports.is_empty(),
            "no_forward_port should disable forwarding"
        );
    }

    #[test]
    fn resolve_forward_ports_uses_user_supplied_ports() {
        let ports = resolve_forward_ports(vec![8080, 9090], false);
        assert_eq!(ports, vec![8080, 9090]);
    }

    #[test]
    fn resolve_forward_ports_dedups_preserving_order() {
        let ports = resolve_forward_ports(vec![8080, 9090, 8080, 7070], false);
        assert_eq!(ports, vec![8080, 9090, 7070]);
    }

    #[test]
    fn resolve_forward_ports_user_supplied_overrides_default() {
        // Explicit non-empty list means user opted into specific ports
        // — don't silently add 54545 to it.
        let ports = resolve_forward_ports(vec![8080], false);
        assert_eq!(ports, vec![8080]);
    }
}
