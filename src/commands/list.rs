//! `cbox list` — enumerate tier instances and the sessions they hold.
//!
//! Iterates the tiers in `cbox.yaml`, asks the backend for each tier's
//! state, and — for running tiers — sshes in to enumerate
//! `/run/cbox/*.sock`. Paused tiers can't be queried without first
//! resuming them; we report them as `paused` with no session detail.

use std::io::{self, Write};

use anyhow::{Context, Result};

use crate::backend::Backend;
use crate::backend::TierState;
use crate::backend::local_docker::LocalDockerBackend;
use crate::config::Config;
use crate::keys::ensure_keypair;
use crate::session::list_active;
use crate::ssh::SshConn;

pub async fn run() -> Result<()> {
    let mut stdout = io::stdout();
    run_with(&mut stdout).await
}

/// Inner entrypoint: writes table output to `out` instead of stdout.
/// Used by integration tests to capture and assert on output.
pub(crate) async fn run_with(out: &mut impl Write) -> Result<()> {
    let cfg = Config::load_async().await?;

    let backend = LocalDockerBackend::new()?;

    // Only generate / read the keypair if we actually have a running tier
    // to query. Avoids surprise ssh-keygen side effects on `cbox list`
    // when nothing is running yet.
    let mut keypair = None;

    let mut rows: Vec<Row> = Vec::with_capacity(cfg.tiers.len());
    for tier in cfg.tiers.keys() {
        let state = backend
            .tier_state(tier)
            .await
            .with_context(|| format!("inspect tier {tier:?}"))?;
        let sessions = if state == TierState::Running
            && let Some(endpoint) = backend
                .endpoint(tier)
                .await
                .with_context(|| format!("endpoint for tier {tier:?}"))?
        {
            if keypair.is_none() {
                keypair = Some(
                    tokio::task::spawn_blocking(ensure_keypair)
                        .await
                        .context("join ensure_keypair task")??,
                );
            }
            let kp = keypair
                .as_ref()
                .context("ensure_keypair did not initialize keypair")?;
            let ssh = SshConn {
                endpoint,
                identity_file: kp.private_key_path.clone(),
                forward_ports: Vec::new(),
            };
            list_active(&ssh)
                .await
                .with_context(|| format!("list sessions in tier {tier:?}"))?
        } else {
            Vec::new()
        };
        rows.push(Row {
            tier: tier.clone(),
            state,
            sessions,
        });
    }

    print_table(out, &rows)?;
    Ok(())
}

pub(crate) struct Row {
    pub(crate) tier: String,
    pub(crate) state: TierState,
    pub(crate) sessions: Vec<String>,
}

fn print_table(out: &mut impl Write, rows: &[Row]) -> io::Result<()> {
    let tier_w = rows
        .iter()
        .map(|r| r.tier.len())
        .max()
        .unwrap_or(0)
        .max("TIER".len());
    let state_w = rows
        .iter()
        .map(|r| state_label(r.state).len())
        .max()
        .unwrap_or(0)
        .max("STATE".len());

    writeln!(out, "{:<tier_w$}  {:<state_w$}  SESSIONS", "TIER", "STATE")?;
    for row in rows {
        let sessions = if matches!(row.state, TierState::Running) {
            if row.sessions.is_empty() {
                "(none)".to_string()
            } else {
                row.sessions.join(", ")
            }
        } else {
            "-".to_string()
        };
        writeln!(
            out,
            "{:<tier_w$}  {:<state_w$}  {}",
            row.tier,
            state_label(row.state),
            sessions,
        )?;
    }
    Ok(())
}

fn state_label(state: TierState) -> &'static str {
    match state {
        TierState::NotCreated => "not created",
        TierState::Running => "running",
        TierState::Paused => "paused",
        TierState::Stopped => "stopped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_label_is_human_readable() {
        assert_eq!(state_label(TierState::NotCreated), "not created");
        assert_eq!(state_label(TierState::Running), "running");
        assert_eq!(state_label(TierState::Paused), "paused");
        assert_eq!(state_label(TierState::Stopped), "stopped");
    }

    #[test]
    fn print_table_aligns_and_lists_sessions() {
        let rows = vec![
            Row {
                tier: "dev".into(),
                state: TierState::Running,
                sessions: vec!["auth-fix".into(), "refactor".into()],
            },
            Row {
                tier: "auto".into(),
                state: TierState::Paused,
                sessions: vec![],
            },
            Row {
                tier: "power".into(),
                state: TierState::NotCreated,
                sessions: vec![],
            },
        ];
        let mut buf = Vec::new();
        print_table(&mut buf, &rows).unwrap();
        let s = String::from_utf8(buf).unwrap();

        // Header present.
        assert!(s.contains("TIER"), "{s}");
        assert!(s.contains("STATE"), "{s}");
        assert!(s.contains("SESSIONS"), "{s}");
        // Running tier shows its sessions, joined.
        assert!(s.contains("dev    running"), "{s}");
        assert!(s.contains("auth-fix, refactor"), "{s}");
        // Non-running tiers show `-`, not session list.
        assert!(s.contains("auto   paused       -"), "{s}");
        assert!(s.contains("power  not created  -"), "{s}");
    }

    #[test]
    fn print_table_empty_sessions_running_tier_shows_none() {
        let rows = vec![Row {
            tier: "dev".into(),
            state: TierState::Running,
            sessions: vec![],
        }];
        let mut buf = Vec::new();
        print_table(&mut buf, &rows).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("(none)"), "{s}");
    }
}
