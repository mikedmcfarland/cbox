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
use crate::commands::common::{CLAUDE_STATE_TARGET, build_run_config};
use crate::commands::login_status;
use crate::config::Config;
use crate::credentials::OnePasswordResolver;
use crate::keys::ensure_keypair;
use crate::ssh::{SshConn, shell_quote, wait_for_sshd};
use crate::tmux;

pub async fn run(tier: String) -> Result<()> {
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
    let ssh = SshConn {
        endpoint,
        identity_file: keypair.private_key_path.clone(),
    };
    wait_for_sshd(&ssh, std::time::Duration::from_secs(60))
        .await
        .context("wait for tier sshd")?;

    // Print the current login state before handing control over.
    // Strictly informational — detection failures degrade to a warning
    // and the agent still launches. See issue #15.
    print_login_state(&tier, &ssh).await;

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

/// Sentinels delimiting the two JSON payloads in [`fetch_state_payload`].
/// Picked to be exceedingly unlikely to occur in `.credentials.json` /
/// `.claude.json` content (which is JSON, so no bare `===` lines).
const CREDS_BEGIN: &str = "=====CBOX_CREDS_BEGIN=====";
const CREDS_END: &str = "=====CBOX_CREDS_END=====";
const CLAUDE_JSON_BEGIN: &str = "=====CBOX_CLAUDE_JSON_BEGIN=====";
const CLAUDE_JSON_END: &str = "=====CBOX_CLAUDE_JSON_END=====";

/// Fetch the per-tier `.claude` state files and print a short status
/// block. Never fails the command — any error degrades to a warning.
async fn print_login_state(tier: &str, ssh: &SshConn) {
    match fetch_state_payload(ssh).await {
        Ok(Some((creds, claude_json))) => {
            let anth =
                login_status::classify_anthropic(creds.as_deref(), login_status::now_unix_ms());
            let mcps = login_status::classify_mcps(claude_json.as_deref());
            // Print to stderr so the agent's own stdout/stdin aren't
            // polluted once it takes over the tty.
            eprint!("{}", login_status::render(tier, &anth, &mcps));
        }
        Ok(None) => {
            // Volume is empty / unreadable in a benign way (first-time
            // login on this tier). Be explicit; don't pretend to know.
            eprintln!(
                "==> tier {tier:?}: no .claude state yet — \
                 will be created on first OAuth login"
            );
        }
        Err(e) => {
            eprintln!(
                "==> tier {tier:?}: could not read login state ({e:#}); \
                 launching agent anyway"
            );
        }
    }
}

/// Read `.credentials.json` and `.claude.json` from the per-tier
/// `.claude` volume in a single SSH round-trip.
///
/// Why exec-over-SSH rather than a `docker run --rm -v ...` helper or
/// a new method on the `Backend` trait:
///
/// - The tier is already running by this point (we just brought it
///   up); SSH is already wired and authenticated. Spawning a separate
///   `docker run` would race with the running container for the
///   volume and introduce a second code path we'd then have to
///   reimplement for remote backends.
/// - Reading-over-SSH stays inside the ADR 011 trust boundary: the
///   `Backend` trait only owns lifecycle + endpoint, and "what's
///   inside the container" is a session-layer concern.
/// - A dedicated `Backend::read_state_file` would be the right shape
///   if/when a second caller appears (e.g. `cbox tier status`). Until
///   then it'd be a one-caller abstraction. TODO: revisit if/when
///   `cbox tier status` lands.
///
/// Returns `Ok(None)` when the directory exists but both files are
/// absent (volume present, nothing logged in yet). Returns the raw
/// JSON strings otherwise — parsing happens in [`login_status`] so it
/// can be unit-tested without Docker.
async fn fetch_state_payload(ssh: &SshConn) -> Result<Option<(Option<String>, Option<String>)>> {
    let script = format!(
        // -lc gives a login shell with normal $PATH; we only need
        // `cat` and `printf` which are coreutils.
        "printf '%s\\n' '{CREDS_BEGIN}'; \
         [ -f {dir}/.credentials.json ] && cat {dir}/.credentials.json; \
         printf '\\n%s\\n' '{CREDS_END}'; \
         printf '%s\\n' '{CLAUDE_JSON_BEGIN}'; \
         [ -f {dir}/.claude.json ] && cat {dir}/.claude.json; \
         printf '\\n%s\\n' '{CLAUDE_JSON_END}'",
        dir = CLAUDE_STATE_TARGET,
    );
    let remote = format!("bash -lc {}", shell_quote(&script));

    let output = tokio::process::Command::new("ssh")
        .args(ssh.args())
        .args(["--", &remote])
        .output()
        .await
        .context("invoke ssh for login-state probe")?;
    if !output.status.success() {
        bail!("login-state probe ssh exited with {}", output.status);
    }
    let text = String::from_utf8_lossy(&output.stdout);

    let creds = extract_between(&text, CREDS_BEGIN, CREDS_END)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let claude_json = extract_between(&text, CLAUDE_JSON_BEGIN, CLAUDE_JSON_END)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    if creds.is_none() && claude_json.is_none() {
        Ok(None)
    } else {
        Ok(Some((creds, claude_json)))
    }
}

/// Extract the slice strictly between `begin` and `end` markers.
/// Returns `None` if either marker is missing or they're out of order.
fn extract_between<'a>(haystack: &'a str, begin: &str, end: &str) -> Option<&'a str> {
    let after_begin = haystack.find(begin).map(|i| i + begin.len())?;
    let end_idx = haystack[after_begin..].find(end)? + after_begin;
    Some(&haystack[after_begin..end_idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_between_pulls_payload() {
        let text = "noise\n===BEGIN===\npayload-line\n===END===\nmore noise";
        assert_eq!(
            extract_between(text, "===BEGIN===", "===END===").map(str::trim),
            Some("payload-line"),
        );
    }

    #[test]
    fn extract_between_returns_none_when_marker_missing() {
        assert_eq!(
            extract_between("just noise", "===BEGIN===", "===END==="),
            None
        );
    }

    #[test]
    fn extract_between_returns_none_when_end_before_begin() {
        let text = "===END===\nstuff\n===BEGIN===\n";
        assert_eq!(extract_between(text, "===BEGIN===", "===END==="), None);
    }
}
