//! SSH command construction for tier instances.
//!
//! Phase 2 uses inline `-p`/`-i`/`-o` flags so the binary works before
//! `~/.ssh/cbox_hosts` exists (that lands in Phase 5 as `cbox ssh-config`).
//! Host-key checking is disabled because each `cbox-tier-<name>` container
//! regenerates host keys on first boot and the dynamic port makes
//! `known_hosts` entries useless — the trust boundary is the loopback
//! interface, not the host key.

// Used by the session module and the attach command landing in later commits.

use std::path::PathBuf;

use crate::backend::TierEndpoint;

#[derive(Debug, Clone)]
pub struct SshConn {
    pub endpoint: TierEndpoint,
    pub identity_file: PathBuf,
    /// Loopback ports to forward host→container for the lifetime of
    /// the ssh process. Rendered as `-L 127.0.0.1:N:127.0.0.1:N` —
    /// loopback on both ends, per ADR 019.
    ///
    /// Used by `cbox auth` so OAuth providers' `localhost:N/callback`
    /// redirects from a host browser land on Claude Code's in-container
    /// listener. Empty for regular sessions.
    pub forward_ports: Vec<u16>,
}

impl SshConn {
    /// SSH options + destination — what comes after `ssh` (or `ssh -t`).
    /// The last entry is `user@host`; everything before it is options.
    pub fn args(&self) -> Vec<String> {
        let mut out = vec![
            "-p".to_string(),
            self.endpoint.port.to_string(),
            "-i".to_string(),
            self.identity_file.display().to_string(),
            // Loopback container; treat host keys as ephemeral.
            "-o".to_string(),
            "StrictHostKeyChecking=no".to_string(),
            "-o".to_string(),
            "UserKnownHostsFile=/dev/null".to_string(),
            "-o".to_string(),
            "LogLevel=ERROR".to_string(),
            "-o".to_string(),
            "IdentitiesOnly=yes".to_string(),
        ];
        // Port forwards, before -o passthroughs so any user-supplied
        // `-o ExitOnForwardFailure=...` still wins. Loopback-bound on
        // both ends: never `*:N` or `0.0.0.0:N` (ADR 019).
        for port in &self.forward_ports {
            out.push("-L".to_string());
            out.push(format!("127.0.0.1:{port}:127.0.0.1:{port}"));
        }
        for (k, v) in &self.endpoint.ssh_options {
            out.push("-o".to_string());
            out.push(format!("{k}={v}"));
        }
        out.push(format!("{}@{}", self.endpoint.user, self.endpoint.host));
        out
    }

    /// Rendered as a shell command line, with each argument single-quoted.
    /// Useful when handing the SSH command to `tmux new-window`, which
    /// re-parses its argument as a shell command.
    pub fn quoted_command_line(&self, extra_args: &[&str]) -> String {
        let mut parts = vec!["ssh".to_string()];
        for a in self.args() {
            parts.push(shell_quote(&a));
        }
        for a in extra_args {
            parts.push(shell_quote(a));
        }
        parts.join(" ")
    }
}

/// Poll until sshd answers a trivial `true` over the connection, or
/// give up after `timeout`. Needed by command paths that run a *single*
/// ssh and bail on failure (`cbox run`'s `dtach -n` spawn): if the
/// tier was just freshly started, sshd inside the container takes a
/// few seconds to come up. The interactive `cbox <name>` path doesn't
/// need this — its `ssh -t` blocks until sshd answers anyway.
pub async fn wait_for_sshd(ssh: &SshConn, timeout: std::time::Duration) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    let mut delay = std::time::Duration::from_millis(200);
    loop {
        let ok = tokio::process::Command::new("ssh")
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
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "sshd on {}:{} did not become reachable within {:?}",
                ssh.endpoint.host,
                ssh.endpoint.port,
                timeout
            );
        }
        tokio::time::sleep(delay).await;
        // Mild backoff so a slow image doesn't get hammered.
        delay = (delay * 2).min(std::time::Duration::from_secs(1));
    }
}

/// Single-quote a token for POSIX shells. Safe for paths, tokens with
/// spaces, and anything except a literal NUL.
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars().all(is_shell_safe) {
        return s.to_string();
    }
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

fn is_shell_safe(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-' | '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_endpoint() -> TierEndpoint {
        TierEndpoint {
            host: "127.0.0.1".to_string(),
            port: 33421,
            user: "cbox".to_string(),
            ssh_options: Vec::new(),
        }
    }

    #[test]
    fn args_include_port_identity_and_destination() {
        let conn = SshConn {
            endpoint: fake_endpoint(),
            identity_file: PathBuf::from("/home/me/.cbox/keys/id_ed25519"),
            forward_ports: Vec::new(),
        };
        let args = conn.args();
        assert!(args.contains(&"-p".into()));
        assert!(args.contains(&"33421".into()));
        assert!(args.contains(&"-i".into()));
        assert!(args.contains(&"/home/me/.cbox/keys/id_ed25519".into()));
        assert_eq!(args.last().unwrap(), "cbox@127.0.0.1");
    }

    #[test]
    fn args_pass_through_endpoint_ssh_options() {
        let mut ep = fake_endpoint();
        ep.ssh_options
            .push(("ProxyCommand".into(), "foo bar".into()));
        let conn = SshConn {
            endpoint: ep,
            identity_file: PathBuf::from("/k"),
            forward_ports: Vec::new(),
        };
        let args = conn.args();
        let pos = args.iter().position(|a| a == "ProxyCommand=foo bar");
        assert!(pos.is_some(), "expected ProxyCommand option");
    }

    #[test]
    fn args_render_loopback_forwards_when_requested() {
        let conn = SshConn {
            endpoint: fake_endpoint(),
            identity_file: PathBuf::from("/k"),
            forward_ports: vec![54545],
        };
        let args = conn.args();
        // Find -L flag and confirm the forward spec is loopback on both sides.
        let mut iter = args.iter();
        let mut found = false;
        while let Some(a) = iter.next() {
            if a == "-L" {
                let spec = iter.next().expect("-L must be followed by spec");
                assert_eq!(spec, "127.0.0.1:54545:127.0.0.1:54545");
                found = true;
            }
        }
        assert!(found, "expected -L 127.0.0.1:54545:... in: {args:?}");
    }

    #[test]
    fn args_omit_l_flag_when_no_forwards_configured() {
        let conn = SshConn {
            endpoint: fake_endpoint(),
            identity_file: PathBuf::from("/k"),
            forward_ports: Vec::new(),
        };
        let args = conn.args();
        assert!(
            !args.iter().any(|a| a == "-L"),
            "no forwards configured should not emit -L: {args:?}"
        );
    }

    #[test]
    fn args_render_multiple_forwards_as_separate_l_flags() {
        let conn = SshConn {
            endpoint: fake_endpoint(),
            identity_file: PathBuf::from("/k"),
            forward_ports: vec![54545, 7777],
        };
        let args = conn.args();
        let count = args.iter().filter(|a| *a == "-L").count();
        assert_eq!(count, 2, "expected one -L per port: {args:?}");
        // Both specs present.
        assert!(args.iter().any(|a| a == "127.0.0.1:54545:127.0.0.1:54545"));
        assert!(args.iter().any(|a| a == "127.0.0.1:7777:127.0.0.1:7777"));
    }

    #[test]
    fn args_never_bind_forwards_to_non_loopback() {
        // Regression guard for ADR 019: the host-side bind must always be
        // loopback. A future refactor must not let `0.0.0.0:N:...` or
        // `*:N:...` slip through.
        let conn = SshConn {
            endpoint: fake_endpoint(),
            identity_file: PathBuf::from("/k"),
            forward_ports: vec![54545],
        };
        let args = conn.args();
        for a in &args {
            assert!(
                !a.starts_with("0.0.0.0:") && !a.starts_with("*:"),
                "forward spec must be loopback-bound, found {a:?} in {args:?}"
            );
        }
    }

    #[test]
    fn shell_quote_leaves_simple_tokens_alone() {
        assert_eq!(shell_quote("simple"), "simple");
        assert_eq!(shell_quote("/abs/path"), "/abs/path");
        assert_eq!(shell_quote("a-b_c.txt"), "a-b_c.txt");
    }

    #[test]
    fn shell_quote_quotes_spaces_and_specials() {
        assert_eq!(shell_quote("with space"), "'with space'");
        assert_eq!(shell_quote("a&b"), "'a&b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn quoted_command_line_passes_extra_args_safely() {
        let conn = SshConn {
            endpoint: fake_endpoint(),
            identity_file: PathBuf::from("/k"),
            forward_ports: Vec::new(),
        };
        let line = conn.quoted_command_line(&["echo", "hi there"]);
        assert!(line.starts_with("ssh "));
        assert!(line.contains("cbox@127.0.0.1"));
        assert!(line.contains(" echo 'hi there'"));
    }
}
