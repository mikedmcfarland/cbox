//! Host tmux helpers used by `cbox auth <tier>`.
//!
//! Interactive sessions (`cbox <name>`) run inline in the invoking pane
//! per ADR 016 and do not touch tmux from cbox's side — users compose
//! tmux windows/panes themselves. The only remaining caller is
//! `cbox auth`, which opens a one-shot `cbox-auth:<tier>` window when
//! invoked from inside a host tmux client so the browser-driven OAuth
//! handoff doesn't take over the user's current pane.

use anyhow::{Context, Result, bail};
use tokio::process::Command;

/// True if cbox is running inside a host tmux client.
pub fn inside_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// `tmux new-window -n <name> <cmd>` and switch focus to it. `command_line`
/// is reparsed by tmux as a shell command, so it must already be safely
/// quoted (use [`crate::ssh::SshConn::quoted_command_line`]).
pub async fn create_window(name: &str, command_line: &str) -> Result<()> {
    let status = Command::new("tmux")
        .arg("new-window")
        .arg("-n")
        .arg(name)
        .arg(command_line)
        .status()
        .await
        .context("invoke tmux new-window")?;
    if !status.success() {
        bail!("tmux new-window exited with {status}");
    }
    Ok(())
}
