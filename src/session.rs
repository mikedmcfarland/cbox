//! Interactive (dtach) session operations executed over SSH.
//!
//! Per ADR 005, interactive sessions use dtach: `dtach -A <sock> -z <cmd>`
//! creates or attaches to the session, with no escape character. The
//! socket lives under `/run/cbox/<name>.sock` inside the tier; presence
//! of the socket is the source of truth for whether the session is alive
//! (per plan §Session tracking).

// Consumed by the attach command in a later commit.

use std::path::Path;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::ssh::{SshConn, shell_quote};

/// Container path for a session's dtach socket.
pub fn socket_path(name: &str) -> String {
    format!("/run/cbox/{name}.sock")
}

/// What the dtach session should launch on first attach.
#[derive(Debug, Clone, Copy)]
pub enum LaunchCommand {
    Claude,
    Shell,
}

impl LaunchCommand {
    fn render(self) -> &'static str {
        match self {
            // `claude` is on PATH in the cbox-base image's layers.
            LaunchCommand::Claude => "claude",
            // `-l` so PATH and rc files are sourced (cbox is the login user).
            LaunchCommand::Shell => "bash -l",
        }
    }
}

/// Build the inner shell command for dtach: cd into the workspace, then
/// `dtach -A <socket> -z <cmd>`. `-z` suppresses the escape character so
/// host tmux keybindings pass through unchanged.
pub fn dtach_command(workspace: &Path, name: &str, launch: LaunchCommand) -> String {
    format!(
        "cd {ws} && exec dtach -A {sock} -z {cmd}",
        ws = shell_quote(&workspace.display().to_string()),
        sock = shell_quote(&socket_path(name)),
        cmd = launch.render(),
    )
}

/// Build the inner shell command for opening a plain shell in the workspace
/// (no dtach attach). Used by re-attach calls that ask for a side shell.
pub fn shell_in_workspace_command(workspace: &Path) -> String {
    format!(
        "cd {ws} && exec bash -l",
        ws = shell_quote(&workspace.display().to_string()),
    )
}

/// Test whether `/run/cbox/<name>.sock` exists on the tier. SSH exit code
/// 0 means the socket is present.
pub async fn socket_exists(ssh: &SshConn, name: &str) -> Result<bool> {
    let socket = socket_path(name);
    let status = Command::new("ssh")
        .args(ssh.args())
        .arg("--")
        .arg("test")
        .arg("-S")
        .arg(&socket)
        .status()
        .await
        .context("invoke ssh test -S")?;
    Ok(status.success())
}

/// Remove the dtach socket on the tier. The dtach process exits naturally
/// once its socket is gone (and no clients are attached).
///
/// We `pkill` against the socket name first so any in-flight Claude/shell
/// terminates cleanly, then `rm -f` to make the operation idempotent even
/// when the socket was already gone.
pub async fn destroy(ssh: &SshConn, name: &str) -> Result<()> {
    let socket = socket_path(name);
    let remote = format!(
        "pkill -f {sock_pat} || true; rm -f {sock}",
        sock_pat = shell_quote(&socket),
        sock = shell_quote(&socket),
    );
    let status = Command::new("ssh")
        .args(ssh.args())
        .arg("--")
        .arg("bash")
        .arg("-c")
        .arg(&remote)
        .status()
        .await
        .context("invoke ssh destroy")?;
    if !status.success() {
        anyhow::bail!("destroy session {name}: ssh exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn socket_path_uses_run_cbox_prefix() {
        assert_eq!(socket_path("auth-fix"), "/run/cbox/auth-fix.sock");
    }

    #[test]
    fn dtach_command_quotes_workspace_path() {
        let cmd = dtach_command(
            &PathBuf::from("/workspace/has space"),
            "session",
            LaunchCommand::Claude,
        );
        assert!(cmd.contains("cd '/workspace/has space'"), "{cmd}");
        assert!(cmd.contains("exec dtach -A /run/cbox/session.sock -z claude"), "{cmd}");
    }

    #[test]
    fn dtach_command_chooses_launch_target() {
        let claude = dtach_command(
            &PathBuf::from("/workspace/x"),
            "s",
            LaunchCommand::Claude,
        );
        assert!(claude.ends_with("-z claude"), "{claude}");
        let shell = dtach_command(
            &PathBuf::from("/workspace/x"),
            "s",
            LaunchCommand::Shell,
        );
        assert!(shell.ends_with("-z bash -l"), "{shell}");
    }

    #[test]
    fn shell_in_workspace_just_cds() {
        let cmd = shell_in_workspace_command(&PathBuf::from("/workspace/auth-fix"));
        assert_eq!(cmd, "cd /workspace/auth-fix && exec bash -l");
    }
}
