//! `cbox run <name> [project] <prompt>` — autonomous (headless) Claude.
//!
//! Same bring-up as the interactive path (ensure tier running, prepare
//! workspace, ensure keypair), but instead of running an inline ssh
//! that the user drives, we ssh in and spawn
//! `dtach -n <sock> claude -p '<prompt>'`. The `-n`
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
use crate::backend::TierState;
use crate::backend::local_docker::LocalDockerBackend;
use crate::commands::common::{build_run_config, resolve_tier};
use crate::config::{Config, TierConfig};
use crate::credentials::OnePasswordResolver;
use crate::keys::{KeyPair, ensure_keypair};
use crate::session::{is_alive, socket_path};
use crate::ssh::{SshConn, shell_quote, wait_for_sshd};
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

    let cfg = Config::load_async().await?;

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
    let resolver = OnePasswordResolver;
    let run_cfg = build_run_config(&tier_name, &cfg, &tier_cfg, &keypair, &resolver).await?;

    let backend = LocalDockerBackend::new()?;

    // Reject duplicate live session names *across all* currently-running
    // tiers before we touch this tier or its workspace. `cbox exec <name>`
    // resolves sessions globally, so a duplicate in another tier would
    // make later commands ambiguous. Probing other tiers here avoids
    // spinning the target tier up just to fail.
    ensure_session_not_alive_elsewhere(&backend, &cfg, &keypair, &tier_name, &name).await?;

    let endpoint = backend
        .ensure_running(&tier_name, &run_cfg)
        .await
        .with_context(|| format!("start tier {tier_name:?}"))?;

    let ssh = SshConn {
        endpoint,
        identity_file: keypair.private_key_path.clone(),
        forward_ports: Vec::new(),
    };

    // ensure_running returns once the container is up, but sshd inside
    // it may need a few seconds on a cold start. We're about to fire
    // *one* ssh and bail on failure — wait for sshd before doing so.
    wait_for_sshd(&ssh, std::time::Duration::from_secs(30))
        .await
        .with_context(|| format!("wait for sshd in tier {tier_name:?}"))?;

    // Check the target tier last, *before* preparing the workspace, so a
    // duplicate `cbox run` never rewrites/syncs the workspace of an
    // already-active session.
    if is_alive(&ssh, &name).await? {
        bail!(
            "session {name:?} is already live in tier {tier_name:?}; \
             destroy it first (`cbox destroy {name}`) before re-running"
        );
    }

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

    // `dtach -n <sock> <agent> <agent_args...> <prompt>` — detached spawn,
    // returns immediately. The agent's output collects in the pty buffer
    // until the user attaches via `cbox <name>`.
    //
    // Whole thing passed as one ssh arg so the remote shell parses our
    // quoting intact (sshd would otherwise space-join multi-arg into
    // `bash -c <first-word>`).
    let agent_cmd = build_autonomous_agent_command(&tier_cfg, &prompt);
    let inner = format!(
        "cd {ws} && exec dtach -n {sock} {agent}",
        ws = shell_quote(&workspace.display().to_string()),
        sock = shell_quote(&socket_path(&name)),
        agent = agent_cmd,
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

/// Assemble the shell-quoted command line for the autonomous agent.
/// Shape: `<cmd> [--dangerously-skip-permissions] <autonomous_args...>
/// <prompt>`. The skip-permissions flag goes before `autonomous_args`
/// so Claude Code's `-p` consumes the right positional.
fn build_autonomous_agent_command(tier_cfg: &TierConfig, prompt: &str) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(3 + tier_cfg.agent.autonomous_args.len());
    parts.push(shell_quote(&tier_cfg.agent.command));
    if tier_cfg.dangerously_skip_permissions {
        parts.push("--dangerously-skip-permissions".to_string());
    }
    for a in &tier_cfg.agent.autonomous_args {
        parts.push(shell_quote(a));
    }
    parts.push(shell_quote(prompt));
    parts.join(" ")
}

/// Refuse to start a session whose name already lives in some *other*
/// running tier. The target tier is excluded — the caller checks it
/// separately after `ensure_running` brings it up. Paused tiers can't
/// be probed without resuming them, so they're skipped; the worst case
/// is a duplicate name in a paused tier surfacing only when the user
/// resumes it (acceptable: `cbox exec` already surfaces the paused-tier
/// hint when no live session is found).
async fn ensure_session_not_alive_elsewhere(
    backend: &LocalDockerBackend,
    cfg: &Config,
    keypair: &KeyPair,
    target_tier: &str,
    session: &str,
) -> Result<()> {
    for tier in cfg.tiers.keys() {
        if tier == target_tier {
            continue;
        }
        let state = backend
            .tier_state(tier)
            .await
            .with_context(|| format!("inspect tier {tier:?}"))?;
        if state != TierState::Running {
            continue;
        }
        let Some(endpoint) = backend
            .endpoint(tier)
            .await
            .with_context(|| format!("endpoint for tier {tier:?}"))?
        else {
            continue;
        };
        let ssh = SshConn {
            endpoint,
            identity_file: keypair.private_key_path.clone(),
            forward_ports: Vec::new(),
        };
        if is_alive(&ssh, session).await? {
            bail!(
                "session {session:?} is already live in tier {tier:?}; \
                 destroy it there (`cbox destroy {session}`) before \
                 starting a new one in {target_tier:?}"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;

    fn tier_cfg_with(skip: bool, command: &str, args: &[&str]) -> TierConfig {
        TierConfig {
            layers: vec![],
            network: crate::config::NetworkMode::Bridge,
            credentials: vec![],
            dangerously_skip_permissions: skip,
            settings: None,
            backend: None,
            agent: AgentConfig {
                command: command.to_string(),
                autonomous_args: args.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    #[test]
    fn autonomous_cmd_default_shape() {
        let tier = tier_cfg_with(false, "claude", &["-p"]);
        let cmd = build_autonomous_agent_command(&tier, "do the thing");
        assert_eq!(cmd, "claude -p 'do the thing'");
    }

    #[test]
    fn autonomous_cmd_inserts_skip_perms_flag() {
        let tier = tier_cfg_with(true, "claude", &["-p"]);
        let cmd = build_autonomous_agent_command(&tier, "x");
        // Flag comes before autonomous_args so `-p` still consumes the
        // prompt positional.
        assert_eq!(cmd, "claude --dangerously-skip-permissions -p x");
    }

    #[test]
    fn autonomous_cmd_shell_quotes_command_and_args() {
        let tier = tier_cfg_with(false, "my agent", &["--flag with space"]);
        let cmd = build_autonomous_agent_command(&tier, "p");
        assert!(cmd.starts_with("'my agent' "), "{cmd}");
        assert!(cmd.contains("'--flag with space'"), "{cmd}");
    }

    /// End-to-end exercise of `cbox run` + `cbox list` + `cbox exec`
    /// against a real `cbox-tier-dev:latest`. Uses a mock agent config
    /// (`bash -c '...'`) so we don't need a working Claude install — the
    /// per-tier agent block exists precisely for this.
    ///
    /// Asserts:
    /// 1. `cbox run "hello world"` returns Ok and the dtach socket appears.
    /// 2. The mock agent received the prompt as a positional arg
    ///    (written to a file in the workspace; verified via `cbox exec
    ///    cat ...`).
    /// 3. `cbox list` includes the live session under the dev tier.
    /// 4. A second `cbox run` with the same name errors (already alive).
    /// 5. `cbox destroy` removes the socket and auto-pauses the tier.
    ///
    /// Ignored by default; run with `just integration`. The pre-flight
    /// check panics with a build instruction if `cbox-tier-dev:latest`
    /// is missing.
    #[tokio::test]
    #[ignore]
    #[serial_test::serial(home)]
    async fn run_list_exec_via_docker() {
        use std::time::Duration;

        use anyhow::{Context, Result};

        use crate::backend::Backend;
        use crate::backend::TierState;
        use crate::backend::local_docker::LocalDockerBackend;
        use crate::commands;
        use crate::session::is_alive;

        const IMAGE: &str = "cbox-tier-dev:latest";
        let backend = LocalDockerBackend::new().expect("connect docker");
        if backend.docker().inspect_image(IMAGE).await.is_err() {
            panic!(
                "missing image {IMAGE}; build first: \
                 CBOX_CONFIG=examples/full-setup/cbox.yaml cargo run -- build dev"
            );
        }

        // Isolate HOME so keys, workspace, and synthetic cbox.yaml all
        // land in a tempdir.
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
        let tmp_home = tempfile::tempdir().expect("home tempdir");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: every HOME-mutating test in this crate takes the
        // `home` serial lock; no other thread observes HOME during this
        // scope. RAII guard restores on panic.
        unsafe { std::env::set_var("HOME", tmp_home.path()) };
        let _home = HomeGuard(prev_home);

        // Minimal source repo for the Path-style project lookup.
        let src = tempfile::tempdir().expect("src tempdir");
        run_git(src.path(), &["init", "-q", "-b", "main"]);
        run_git(src.path(), &["config", "user.email", "test@example.com"]);
        run_git(src.path(), &["config", "user.name", "test"]);
        std::fs::write(src.path().join("README"), b"hi\n").expect("write README");
        run_git(src.path(), &["add", "."]);
        run_git(src.path(), &["commit", "-q", "-m", "init"]);

        // Synthetic cbox.yaml — note the agent block that turns the
        // "autonomous" run into `bash -c 'echo "$0" > prompt.txt; sleep
        // 60' "<prompt>"`. The script writes the prompt to a file in
        // the workspace (cbox cd's there first), so `cbox exec cat
        // prompt.txt` can read it back.
        let cfg_dir = tmp_home.path().join(".config/cbox");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir cfg_dir");
        let env_dir = tmp_home.path().join("env");
        let layer_dir = tmp_home.path().join("layer-c");
        std::fs::create_dir_all(&env_dir).expect("mkdir env");
        std::fs::create_dir_all(&layer_dir).expect("mkdir layer");
        let yaml = format!(
            "environment: {env}\n\
             default_tier: dev\n\
             layers:\n  c: {layer}\n\
             tiers:\n\
             \x20\x20dev:\n\
             \x20\x20\x20\x20layers: [c]\n\
             \x20\x20\x20\x20agent:\n\
             \x20\x20\x20\x20\x20\x20command: bash\n\
             \x20\x20\x20\x20\x20\x20autonomous_args: [\"-c\", \"printf %s \\\"$0\\\" > prompt.txt; sleep 60\"]\n",
            env = env_dir.display(),
            layer = layer_dir.display(),
        );
        std::fs::write(cfg_dir.join("cbox.yaml"), &yaml).expect("write cbox.yaml");

        let tier = "dev";
        let session = "phase3-orch";

        // Idempotent cleanup of any prior container.
        let _ = backend.remove_instance(tier).await;

        let outcome: Result<()> = async {
            let project_arg = src.path().to_str().expect("src path is utf8").to_string();
            let prompt = "hello world".to_string();

            // 1. cbox run — autonomous spawn with the mock agent.
            commands::run::run(
                session.to_string(),
                Some(project_arg.clone()),
                prompt.clone(),
                None,
            )
            .await
            .context("cbox run")?;

            // 2. Socket should appear within a couple seconds.
            //    Use a SshConn via the backend to query.
            let keypair = tokio::task::spawn_blocking(crate::keys::ensure_keypair)
                .await
                .context("join")??;
            let endpoint = backend
                .endpoint(tier)
                .await?
                .context("tier should be running after cbox run")?;
            let ssh = crate::ssh::SshConn {
                endpoint,
                identity_file: keypair.private_key_path.clone(),
                forward_ports: Vec::new(),
            };
            let mut alive_found = false;
            for _ in 0..30 {
                if is_alive(&ssh, session).await? {
                    alive_found = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            anyhow::ensure!(alive_found, "session socket never appeared after cbox run");

            // Give the bash script a moment to land prompt.txt.
            tokio::time::sleep(Duration::from_millis(300)).await;

            // 3. cbox exec — read back the prompt the mock agent wrote.
            //    Captures stdout via a child process this time since
            //    exec::run inherits the parent's stdio. We use a probe:
            //    `test -f prompt.txt && cat prompt.txt`.
            let cat_out = tokio::process::Command::new("ssh")
                .args(ssh.args())
                .arg("--")
                .arg(format!(
                    "bash -lc 'cd /workspace/{session} && cat prompt.txt'"
                ))
                .output()
                .await
                .context("ssh cat prompt.txt")?;
            anyhow::ensure!(
                cat_out.status.success(),
                "cat prompt.txt failed: stderr={:?}",
                String::from_utf8_lossy(&cat_out.stderr)
            );
            let observed = String::from_utf8_lossy(&cat_out.stdout).trim().to_string();
            anyhow::ensure!(
                observed == prompt,
                "mock agent wrote {observed:?}, expected {prompt:?}"
            );

            // 4. cbox exec — full handler path with a trivial probe.
            commands::exec::run(session.to_string(), vec!["true".to_string()])
                .await
                .context("cbox exec true")?;

            // 5. cbox list — capture and assert.
            let mut buf = Vec::<u8>::new();
            commands::list::run_with(&mut buf)
                .await
                .context("cbox list")?;
            let listing = String::from_utf8(buf).context("list output not utf8")?;
            anyhow::ensure!(listing.contains("dev"), "list missing dev tier: {listing}");
            anyhow::ensure!(
                listing.contains("running"),
                "list missing running state: {listing}"
            );
            anyhow::ensure!(
                listing.contains(session),
                "list missing session {session}: {listing}"
            );

            // 6. cbox run again with the same name — should bail.
            let second = commands::run::run(
                session.to_string(),
                Some(project_arg.clone()),
                prompt.clone(),
                None,
            )
            .await;
            anyhow::ensure!(
                second.is_err(),
                "second cbox run should error when session is alive"
            );

            // 7. cbox destroy — socket disappears, tier auto-pauses.
            commands::destroy::run(session.to_string(), false)
                .await
                .context("cbox destroy")?;
            let state = backend.tier_state(tier).await?;
            anyhow::ensure!(
                state == TierState::Paused,
                "expected tier auto-paused after last session, got {state:?}"
            );

            Ok(())
        }
        .await;

        let teardown = backend.remove_instance(tier).await;
        outcome.expect("phase 3 orchestration");
        teardown.expect("destroy tier");
    }

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .status()
            .unwrap_or_else(|e| panic!("invoke git {args:?}: {e}"));
        assert!(status.success(), "git {args:?} failed: {status}");
    }
}
