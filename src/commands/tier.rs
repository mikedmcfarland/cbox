//! `cbox tier {stop,pause,resume,reset,destroy}` — explicit tier-instance
//! lifecycle and state teardown.
//!
//! Auto-pause on the last `destroy` is handled in `commands/destroy.rs`;
//! these handlers cover the operator-initiated paths the plan calls out
//! under §Container lifecycle, plus the destructive `reset` / `destroy`
//! teardown verbs (issue #10).

use std::io::{BufRead, Write};

use anyhow::{Context, Result};

use crate::backend::Backend;
use crate::backend::TierState;
use crate::backend::local_docker::LocalDockerBackend;
use crate::build::tier_image_tag;
use crate::cli::TierOp;
use crate::commands::common::{build_run_config, claude_volume_name};
use crate::config::Config;
use crate::credentials::OnePasswordResolver;
use crate::keys::ensure_keypair;

pub async fn run(op: TierOp) -> Result<()> {
    match op {
        TierOp::Stop { tier } => stop(&tier).await,
        TierOp::Pause { tier } => pause(&tier).await,
        TierOp::Resume { tier } => resume(&tier).await,
        TierOp::Reset { tier, yes } => reset(&tier, yes).await,
        TierOp::Destroy { tier, yes } => destroy(&tier, yes).await,
        // `cbox list` already prints the tier table; keep `tier list` out
        // of scope so we don't ship two near-identical commands.
        TierOp::List => anyhow::bail!("`cbox tier list` is not implemented; use `cbox list`"),
    }
}

async fn stop(tier: &str) -> Result<()> {
    let cfg = load_config_and_validate_tier(tier).await?;
    let _ = cfg;
    let backend = LocalDockerBackend::new()?;
    match backend.tier_state(tier).await? {
        TierState::NotCreated => {
            eprintln!("==> tier {tier:?} not created; nothing to stop");
            return Ok(());
        }
        TierState::Stopped => {
            eprintln!("==> tier {tier:?} already stopped");
            return Ok(());
        }
        TierState::Running | TierState::Paused => {}
    }
    backend
        .stop(tier)
        .await
        .with_context(|| format!("stop tier {tier:?}"))?;
    eprintln!("==> tier {tier:?} stopped");
    Ok(())
}

async fn pause(tier: &str) -> Result<()> {
    let _ = load_config_and_validate_tier(tier).await?;
    let backend = LocalDockerBackend::new()?;
    match backend.tier_state(tier).await? {
        TierState::Running => {}
        TierState::Paused => {
            eprintln!("==> tier {tier:?} already paused");
            return Ok(());
        }
        TierState::Stopped => anyhow::bail!("tier {tier:?} is stopped; nothing to pause"),
        TierState::NotCreated => anyhow::bail!("tier {tier:?} is not created; nothing to pause"),
    }
    backend
        .pause(tier)
        .await
        .with_context(|| format!("pause tier {tier:?}"))?;
    eprintln!("==> tier {tier:?} paused");
    Ok(())
}

async fn resume(tier: &str) -> Result<()> {
    let cfg = load_config_and_validate_tier(tier).await?;

    // Resume goes through `ensure_running` rather than a dedicated
    // `unpause` because the user's intent is "I want this tier
    // available." If it was paused we unpause; if it was stopped we
    // start; if it was never created we create it. The cost: we resolve
    // credentials even when just unpausing — but that's the same path
    // `cbox <name>` takes on every invocation, so the surface area is
    // already paid for.
    let tier_cfg = cfg
        .tiers
        .get(tier)
        .with_context(|| format!("tier {tier:?} not defined"))?
        .clone();
    let keypair = tokio::task::spawn_blocking(ensure_keypair)
        .await
        .context("join ensure_keypair task")??;
    let resolver = OnePasswordResolver;
    let run_cfg = build_run_config(tier, &cfg, &tier_cfg, &keypair, &resolver).await?;

    let backend = LocalDockerBackend::new()?;
    let endpoint = backend
        .ensure_running(tier, &run_cfg)
        .await
        .with_context(|| format!("resume tier {tier:?}"))?;
    eprintln!(
        "==> tier {tier:?} running at {}:{}",
        endpoint.host, endpoint.port
    );
    Ok(())
}

async fn reset(tier: &str, yes: bool) -> Result<()> {
    // Reset is allowed even for tiers that aren't in cbox.yaml: the
    // operator may have removed the tier from config and now wants to
    // wipe the leftover volume. Don't require config-membership here.
    let prompt = format!(
        "About to reset tier {tier:?}: remove the tier instance and \
         wipe the {volume} volume (Claude state, OAuth tokens, MCP \
         credentials). The tier image is preserved. Continue?",
        volume = claude_volume_name(tier),
    );
    if !confirm(&prompt, yes, &mut Tty)? {
        eprintln!("==> aborted");
        return Ok(());
    }

    let backend = LocalDockerBackend::new()?;
    backend
        .reset(tier)
        .await
        .with_context(|| format!("reset tier {tier:?}"))?;
    eprintln!(
        "==> tier {tier:?} reset: instance removed, volume {} wiped",
        claude_volume_name(tier)
    );
    Ok(())
}

async fn destroy(tier: &str, yes: bool) -> Result<()> {
    let prompt = format!(
        "About to destroy tier {tier:?}: remove the tier instance, \
         wipe the {volume} volume, remove the {image} image, and \
         delete the on-host workspace dir ~/.cbox/workspaces/{tier}/. \
         `cbox build {tier}` is required before using it again. \
         Continue?",
        volume = claude_volume_name(tier),
        image = tier_image_tag(tier),
    );
    if !confirm(&prompt, yes, &mut Tty)? {
        eprintln!("==> aborted");
        return Ok(());
    }

    let backend = LocalDockerBackend::new()?;
    backend
        .tier_destroy(tier)
        .await
        .with_context(|| format!("destroy tier {tier:?}"))?;
    eprintln!("==> tier {tier:?} destroyed");
    Ok(())
}

async fn load_config_and_validate_tier(tier: &str) -> Result<Config> {
    let cfg = Config::load_async().await?;
    if !cfg.tiers.contains_key(tier) {
        anyhow::bail!("tier {tier:?} is not defined in cbox.yaml");
    }
    Ok(cfg)
}

/// Interactive y/N prompt. Returns `Ok(true)` to proceed.
///
/// - `yes` short-circuits to true (used by `--yes` / `-y`).
/// - Otherwise the prompt is read from `confirm_io`'s stdin handle. If
///   that handle reports no TTY, we bail with an actionable error rather
///   than silently proceeding — this is a destructive operation that
///   must not run without explicit consent from a non-script caller.
///
/// Only "y" / "yes" (case-insensitive) count as confirmation; everything
/// else (including a bare Enter) declines.
fn confirm(prompt: &str, yes: bool, io: &mut dyn ConfirmIo) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if !io.stdin_is_tty() {
        anyhow::bail!(
            "refusing to proceed without confirmation: stdin is not a TTY; \
             pass --yes to skip the prompt"
        );
    }
    io.write_prompt(&format!("{prompt} [y/N]: "))
        .context("write prompt")?;

    let mut line = String::new();
    io.read_line(&mut line).context("read confirmation")?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}

/// Indirection so [`confirm`] can be unit-tested without owning stdio.
trait ConfirmIo {
    fn stdin_is_tty(&self) -> bool;
    fn write_prompt(&mut self, msg: &str) -> std::io::Result<()>;
    fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize>;
}

/// Production [`ConfirmIo`]: real stdin/stderr.
struct Tty;

impl ConfirmIo for Tty {
    fn stdin_is_tty(&self) -> bool {
        // We avoid pulling in a TTY detection crate: `cbox` already ships
        // without one. `/dev/tty` opening is a reliable proxy on Unix —
        // it succeeds iff the process has a controlling terminal, which
        // matches the intent ("is there a human to prompt").
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_ok()
    }
    fn write_prompt(&mut self, msg: &str) -> std::io::Result<()> {
        let mut out = std::io::stderr().lock();
        out.write_all(msg.as_bytes())?;
        out.flush()
    }
    fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
        std::io::stdin().lock().read_line(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock [`ConfirmIo`] for unit tests: scriptable TTY state and
    /// canned stdin lines.
    struct MockIo {
        tty: bool,
        stdin: std::io::Cursor<Vec<u8>>,
        stderr: Vec<u8>,
    }

    impl MockIo {
        fn new(tty: bool, input: &str) -> Self {
            Self {
                tty,
                stdin: std::io::Cursor::new(input.as_bytes().to_vec()),
                stderr: Vec::new(),
            }
        }
    }

    impl ConfirmIo for MockIo {
        fn stdin_is_tty(&self) -> bool {
            self.tty
        }
        fn write_prompt(&mut self, msg: &str) -> std::io::Result<()> {
            self.stderr.extend_from_slice(msg.as_bytes());
            Ok(())
        }
        fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
            self.stdin.read_line(buf)
        }
    }

    #[test]
    fn yes_flag_short_circuits_prompt() {
        // --yes path: should not touch stdin (we'd hang on an empty
        // cursor in a TTY-claiming mock if it did).
        let mut io = MockIo::new(false, "");
        assert!(confirm("destroy?", true, &mut io).unwrap());
        // No prompt should be written because we short-circuited.
        assert!(io.stderr.is_empty(), "no prompt expected with --yes");
    }

    #[test]
    fn affirmative_answer_proceeds() {
        for ans in ["y\n", "Y\n", "yes\n", "YES\n", "  yes  \n"] {
            let mut io = MockIo::new(true, ans);
            assert!(
                confirm("destroy?", false, &mut io).unwrap(),
                "{ans:?} should confirm"
            );
        }
    }

    #[test]
    fn negative_or_empty_answer_declines() {
        for ans in ["n\n", "no\n", "\n", "maybe\n", "yep\n"] {
            let mut io = MockIo::new(true, ans);
            assert!(
                !confirm("destroy?", false, &mut io).unwrap(),
                "{ans:?} should decline"
            );
        }
    }

    #[test]
    fn non_tty_without_yes_flag_errors() {
        let mut io = MockIo::new(false, "y\n");
        let err =
            confirm("destroy?", false, &mut io).expect_err("non-tty without --yes must error");
        let msg = err.to_string();
        assert!(msg.contains("--yes"), "{msg}");
        assert!(msg.contains("TTY"), "{msg}");
    }

    #[test]
    fn prompt_is_written_with_y_n_suffix() {
        let mut io = MockIo::new(true, "n\n");
        confirm("Continue?", false, &mut io).unwrap();
        let written = String::from_utf8(io.stderr).unwrap();
        assert!(written.contains("Continue?"), "{written:?}");
        assert!(written.contains("[y/N]"), "{written:?}");
    }
}
