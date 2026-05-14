//! `cbox list` — enumerate tier instances and the sessions they hold.
//!
//! Iterates the tiers in `cbox.yaml`, asks the backend for each tier's
//! state, and — for running tiers — sshes in to enumerate
//! `/run/cbox/*.sock`. Paused tiers can't be queried without first
//! resuming them; we report them as `paused` with no session detail.

use anyhow::{Context, Result};

use crate::backend::Backend;
use crate::backend::TierState;
use crate::backend::local_docker::LocalDockerBackend;
use crate::config::Config;
use crate::keys::ensure_keypair;
use crate::session::list_active;
use crate::ssh::SshConn;

pub async fn run() -> Result<()> {
    let cfg_path = Config::default_path()?;
    let cfg = Config::load(&cfg_path)
        .with_context(|| format!("load config from {}", cfg_path.display()))?;

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
            let ssh = SshConn {
                endpoint,
                identity_file: keypair.as_ref().unwrap().private_key_path.clone(),
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

    print_table(&rows);
    Ok(())
}

struct Row {
    tier: String,
    state: TierState,
    sessions: Vec<String>,
}

fn print_table(rows: &[Row]) {
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

    println!("{:<tier_w$}  {:<state_w$}  SESSIONS", "TIER", "STATE");
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
        println!(
            "{:<tier_w$}  {:<state_w$}  {}",
            row.tier,
            state_label(row.state),
            sessions,
        );
    }
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
}
