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
    // Passed as ONE ssh arg — sshd hands it to the user's login shell as
    // `bash -c <arg>`, so shell metacharacters (`;`, `||`) work as written.
    // Splitting into multiple args would let ssh space-join them, and the
    // remote `bash -c` would then treat only the first word as its script.
    //
    // The pkill pattern is anchored to `^dtach ` so it doesn't match the
    // remote shell running this very script (whose argv contains the
    // socket path as part of the pkill argument).
    let remote = format!(
        "pkill -f {pat} || true; rm -f {sock}",
        pat = shell_quote(&format!("^dtach .*{socket}")),
        sock = shell_quote(&socket),
    );
    let status = Command::new("ssh")
        .args(ssh.args())
        .arg("--")
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
        assert!(
            cmd.contains("exec dtach -A /run/cbox/session.sock -z claude"),
            "{cmd}"
        );
    }

    #[test]
    fn dtach_command_chooses_launch_target() {
        let claude = dtach_command(&PathBuf::from("/workspace/x"), "s", LaunchCommand::Claude);
        assert!(claude.ends_with("-z claude"), "{claude}");
        let shell = dtach_command(&PathBuf::from("/workspace/x"), "s", LaunchCommand::Shell);
        assert!(shell.ends_with("-z bash -l"), "{shell}");
    }

    #[test]
    fn shell_in_workspace_just_cds() {
        let cmd = shell_in_workspace_command(&PathBuf::from("/workspace/auth-fix"));
        assert_eq!(cmd, "cd /workspace/auth-fix && exec bash -l");
    }

    /// End-to-end: bring up a tier container with the cbox keypair injected,
    /// create a dtach socket via SSH, observe it via `socket_exists`, then
    /// destroy and confirm the socket is gone.
    ///
    /// Reuses `cbox-tier-dev:latest` (the Phase 1 smoke-test image) — build
    /// it first with `just integration` (which chains through
    /// `cargo run -- build dev`). Ignored by default. Restores `HOME` even
    /// on failure so the test never leaks state into the developer's
    /// real `~/.cbox/`.
    #[tokio::test]
    #[ignore]
    #[serial_test::serial(home)]
    async fn session_lifecycle_via_ssh() {
        use std::time::Duration;

        use crate::backend::Backend;
        use crate::backend::TierRunConfig;
        use crate::backend::local_docker::LocalDockerBackend;
        use crate::config::NetworkMode;
        use crate::keys::{AUTHORIZED_KEYS_ENV, ensure_keypair};

        const IMAGE: &str = "cbox-tier-dev:latest";
        let tier = "phase2-session-test";
        let session_name = "lifecycle";

        let backend = LocalDockerBackend::new().expect("connect docker");
        if backend.docker().inspect_image(IMAGE).await.is_err() {
            panic!(
                "missing image {IMAGE}; build it first: \
                 CBOX_CONFIG=examples/full-setup/cbox.yaml cargo run -- build dev"
            );
        }

        // Isolate HOME so the keypair lands in a tempdir, not the
        // developer's real ~/.cbox/.
        let tmp_home = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: every HOME-mutating test in this crate takes the
        // `home` serial lock; no other thread observes HOME during this
        // scope. The RAII guard below restores HOME on panic.
        unsafe { std::env::set_var("HOME", tmp_home.path()) };
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
        let _home = HomeGuard(prev_home);

        // Idempotent teardown of any prior container.
        let _ = backend.destroy(tier).await;

        let kp = tokio::task::spawn_blocking(ensure_keypair)
            .await
            .expect("join")
            .expect("generate keypair");
        let cfg = TierRunConfig {
            image: IMAGE.to_string(),
            env: vec![(AUTHORIZED_KEYS_ENV.to_string(), kp.public_key.clone())],
            network_mode: NetworkMode::Bridge,
            privileged: true,
            mounts: Vec::new(),
        };

        let endpoint = backend
            .ensure_running(tier, &cfg)
            .await
            .expect("start tier");
        let ssh = SshConn {
            endpoint,
            identity_file: kp.private_key_path.clone(),
        };

        // sshd starts in parallel with dockerd via supervisord; poll until
        // a `ssh ... true` returns successfully.
        let mut sshd_ready = false;
        for _ in 0..60 {
            let ok = Command::new("ssh")
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
                sshd_ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // Run the rest in a closure so cleanup happens even on assertion failure.
        let outcome: Result<()> = async {
            if !sshd_ready {
                anyhow::bail!(
                    "sshd never became reachable on {}",
                    ssh.args().last().unwrap()
                );
            }

            assert!(
                !socket_exists(&ssh, session_name).await?,
                "fresh tier should have no session socket"
            );

            // Non-interactive dtach: forks, returns immediately, leaves the
            // socket around with `sleep 60` as the inner process. Passed
            // as a single ssh arg so the remote shell parses it intact.
            let inner = format!(
                "dtach -n {sock} sleep 60",
                sock = shell_quote(&socket_path(session_name)),
            );
            let status = Command::new("ssh")
                .args(ssh.args())
                .arg("--")
                .arg(&inner)
                .status()
                .await?;
            anyhow::ensure!(status.success(), "dtach -n failed: {status}");

            // Socket should appear nearly instantly.
            let mut found = false;
            for _ in 0..30 {
                if socket_exists(&ssh, session_name).await? {
                    found = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            anyhow::ensure!(found, "dtach socket never appeared");

            destroy(&ssh, session_name).await?;
            anyhow::ensure!(
                !socket_exists(&ssh, session_name).await?,
                "socket persisted after destroy"
            );
            Ok(())
        }
        .await;

        // Always tear down the tier, then propagate the outcome.
        let teardown = backend.destroy(tier).await;
        outcome.expect("session lifecycle");
        teardown.expect("destroy tier");
    }
}
