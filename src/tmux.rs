//! Host tmux window management for cbox interactive sessions.
//!
//! Phase 2 places each session's dtach connection in its own host tmux
//! window named `cbox:<session>`. Subsequent invocations that open a
//! plain shell get a suffixed name (`cbox:<session>:shell`) so re-attach
//! never collides with the primary window.

// Consumed by the attach command in a later commit.

use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

/// True if cbox is running inside a host tmux client.
pub fn inside_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// Tmux window name for `(session, suffix)`. The bare form
/// (`suffix == None`) is reserved for the dtach window holding Claude.
pub fn window_name(session: &str, suffix: Option<&str>) -> String {
    match suffix {
        Some(s) => format!("cbox:{session}:{s}"),
        None => format!("cbox:{session}"),
    }
}

/// Prefix used to match all windows belonging to a session for cleanup.
pub fn session_prefix(session: &str) -> String {
    format!("cbox:{session}")
}

/// Returns true if a name belongs to `session` — exact match on the
/// primary window, or `cbox:<session>:` prefix for ancillary windows.
pub fn window_belongs_to_session(name: &str, session: &str) -> bool {
    let primary = session_prefix(session);
    if name == primary {
        return true;
    }
    let ancillary = format!("{primary}:");
    name.starts_with(&ancillary)
}

/// `tmux new-window -n <name> <cmd>` and switch focus to it. `command_line`
/// is reparsed by tmux as a shell command, so it must already be safely
/// quoted (use [`ssh::SshConn::quoted_command_line`]).
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

/// Switch focus to an existing window whose name is exactly `name`.
/// Returns `Ok(false)` if no such window exists.
pub async fn select_window(name: &str) -> Result<bool> {
    let status = Command::new("tmux")
        .arg("select-window")
        .arg("-t")
        .arg(name)
        .stderr(Stdio::null())
        .status()
        .await
        .context("invoke tmux select-window")?;
    Ok(status.success())
}

/// Kill every window whose name belongs to `session` (see
/// [`window_belongs_to_session`]). Returns `Ok(())` if no tmux server is
/// running — `cbox destroy` from a non-tmux shell must still succeed.
pub async fn kill_session_windows(session: &str) -> Result<()> {
    let output = Command::new("tmux")
        .arg("list-windows")
        .arg("-a")
        .arg("-F")
        .arg("#{window_id} #{window_name}")
        .stderr(Stdio::null())
        .output()
        .await
        .context("invoke tmux list-windows")?;
    if !output.status.success() {
        return Ok(());
    }

    let listing = String::from_utf8_lossy(&output.stdout);
    for line in listing.lines() {
        let Some((id, name)) = line.split_once(' ') else {
            continue;
        };
        if window_belongs_to_session(name, session) {
            let status = Command::new("tmux")
                .arg("kill-window")
                .arg("-t")
                .arg(id)
                .status()
                .await
                .context("invoke tmux kill-window")?;
            if !status.success() {
                bail!("tmux kill-window {id} exited with {status}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_names_are_stable() {
        assert_eq!(window_name("auth-fix", None), "cbox:auth-fix");
        assert_eq!(
            window_name("auth-fix", Some("shell-1")),
            "cbox:auth-fix:shell-1"
        );
    }

    #[test]
    fn window_belongs_matches_primary_and_ancillary() {
        assert!(window_belongs_to_session("cbox:auth-fix", "auth-fix"));
        assert!(window_belongs_to_session("cbox:auth-fix:shell-1", "auth-fix"));
        assert!(window_belongs_to_session("cbox:auth-fix:claude-2", "auth-fix"));
    }

    #[test]
    fn window_belongs_rejects_unrelated_names() {
        assert!(!window_belongs_to_session("cbox:auth-fix-extra", "auth-fix"));
        assert!(!window_belongs_to_session("auth-fix", "auth-fix"));
        assert!(!window_belongs_to_session("zsh", "auth-fix"));
        assert!(!window_belongs_to_session("cbox:other", "auth-fix"));
    }
}
