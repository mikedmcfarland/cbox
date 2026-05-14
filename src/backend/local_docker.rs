//! Local Docker backend (ADR 011).
//!
//! Tier instances are long-running Docker containers identified by labels:
//! `managed-by=cbox` and `cbox.tier=<name>`. The container name is
//! `cbox-tier-<name>`.
//!
//! Phase 1 wires the bollard client and lifecycle methods, and translates
//! `Mount` values into host-path bind mounts only — named-volume plumbing
//! (for `.claude.json`, workspaces, Docker image cache) lands in Phase 2
//! alongside session machinery. SSH endpoint discovery is also Phase 2;
//! [`endpoint`](LocalDockerBackend::endpoint) currently returns `None` even
//! for running instances.

// Phase 1 foundation: label constants and helpers are consumed by Phase 2
// session machinery. Drop this when consumers land.
#![allow(dead_code)]

use std::collections::HashMap;

use anyhow::{Context, Result};
use async_trait::async_trait;
use bollard::Docker;
use bollard::errors::Error as DockerError;
use bollard::models::{
    ContainerCreateBody, ContainerStateStatusEnum, ContainerSummaryStateEnum, HostConfig,
    NetworkingConfig, PortBinding,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, ListContainersOptionsBuilder, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};

use super::{Backend, Mount, MountSource, TierEndpoint, TierInfo, TierRunConfig, TierState};
use crate::config::NetworkMode;

/// Docker label that marks every cbox-managed container.
pub const MANAGED_BY_LABEL: &str = "managed-by";
pub const MANAGED_BY_VALUE: &str = "cbox";

/// Docker label whose value is the tier name.
pub const TIER_LABEL: &str = "cbox.tier";

/// Backend identifier reported in [`TierInfo`].
pub const BACKEND_NAME: &str = "local";

/// Container port we expose for SSH. The base image's sshd listens on :22;
/// we publish that to a dynamic host port and look it up via
/// [`Backend::endpoint`].
pub const SSH_CONTAINER_PORT: &str = "22/tcp";

/// Compose the container name cbox uses for a tier instance.
pub fn container_name(tier: &str) -> String {
    format!("cbox-tier-{tier}")
}

pub struct LocalDockerBackend {
    docker: Docker,
}

impl LocalDockerBackend {
    /// Connect to the local Docker daemon using platform defaults
    /// (Unix socket on macOS/Linux, named pipe on Windows).
    pub fn new() -> Result<Self> {
        let docker =
            Docker::connect_with_local_defaults().context("connect to local Docker daemon")?;
        Ok(Self { docker })
    }

    /// Borrow the underlying bollard client. Image building lives outside
    /// the [`Backend`] trait (ADR 011) but uses the same client.
    pub fn docker(&self) -> &Docker {
        &self.docker
    }
}

#[async_trait]
impl Backend for LocalDockerBackend {
    async fn ensure_running(&self, tier: &str, config: &TierRunConfig) -> Result<TierEndpoint> {
        let name = container_name(tier);

        match self.tier_state(tier).await? {
            TierState::Running => {}
            TierState::Paused => {
                self.docker
                    .unpause_container(&name)
                    .await
                    .with_context(|| format!("unpause {name}"))?;
            }
            TierState::Stopped => {
                self.docker
                    .start_container(
                        &name,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await
                    .with_context(|| format!("start {name}"))?;
            }
            TierState::NotCreated => {
                create_container(&self.docker, tier, config).await?;
                self.docker
                    .start_container(
                        &name,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await
                    .with_context(|| format!("start {name}"))?;
            }
        }

        // After `docker start`, NetworkSettings.Ports can briefly be empty
        // while the daemon publishes the port. Poll for a few seconds
        // before giving up.
        for _ in 0..50 {
            if let Some(ep) = self.endpoint(tier).await? {
                return Ok(ep);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        anyhow::bail!("{name} started but ssh port never published")
    }

    async fn pause(&self, tier: &str) -> Result<()> {
        let name = container_name(tier);
        match self.docker.pause_container(&name).await {
            Ok(()) => Ok(()),
            // 409 = already paused. Treat as success so callers (e.g.
            // auto-pause on session destroy) don't have to pre-check.
            Err(DockerError::DockerResponseServerError {
                status_code: 409, ..
            }) => Ok(()),
            Err(e) => Err(e).with_context(|| format!("pause {name}")),
        }
    }

    async fn stop(&self, tier: &str) -> Result<()> {
        let name = container_name(tier);
        let opts = StopContainerOptionsBuilder::default().t(10).build();
        self.docker
            .stop_container(&name, Some(opts))
            .await
            .with_context(|| format!("stop {name}"))?;
        Ok(())
    }

    async fn destroy(&self, tier: &str) -> Result<()> {
        let name = container_name(tier);
        let opts = RemoveContainerOptionsBuilder::default()
            .force(true)
            .v(true)
            .build();
        match self.docker.remove_container(&name, Some(opts)).await {
            Ok(()) => Ok(()),
            Err(DockerError::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove {name}")),
        }
    }

    async fn tier_state(&self, tier: &str) -> Result<TierState> {
        let name = container_name(tier);
        match self
            .docker
            .inspect_container(
                &name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(resp) => {
                let status = resp
                    .state
                    .and_then(|s| s.status)
                    .unwrap_or(ContainerStateStatusEnum::EMPTY);
                Ok(map_state(status))
            }
            Err(DockerError::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(TierState::NotCreated),
            Err(e) => Err(e).with_context(|| format!("inspect {name}")),
        }
    }

    async fn list_tiers(&self) -> Result<Vec<TierInfo>> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![format!("{MANAGED_BY_LABEL}={MANAGED_BY_VALUE}")],
        );
        let opts = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();
        let containers = self
            .docker
            .list_containers(Some(opts))
            .await
            .context("list cbox containers")?;

        let mut out = Vec::with_capacity(containers.len());
        for c in containers {
            let labels = c.labels.unwrap_or_default();
            let Some(tier) = labels.get(TIER_LABEL).cloned() else {
                continue;
            };
            let status = c
                .state
                .map(map_summary_state)
                .unwrap_or(TierState::NotCreated);
            out.push(TierInfo {
                tier,
                state: status,
                backend: BACKEND_NAME.to_string(),
            });
        }
        Ok(out)
    }

    async fn endpoint(&self, tier: &str) -> Result<Option<TierEndpoint>> {
        let name = container_name(tier);
        let resp = match self
            .docker
            .inspect_container(
                &name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(r) => r,
            Err(DockerError::DockerResponseServerError {
                status_code: 404, ..
            }) => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("inspect {name}")),
        };

        // Only running containers have an active port mapping. Paused
        // containers keep the same mapping, but `host_port` is still set
        // because Docker reserves it for the container's lifetime.
        let Some(port) = resp
            .network_settings
            .as_ref()
            .and_then(|ns| ns.ports.as_ref())
            .and_then(|p| p.get(SSH_CONTAINER_PORT).cloned())
            .flatten()
            .and_then(|bindings| {
                bindings
                    .into_iter()
                    .find_map(|b| b.host_port.and_then(|s| s.parse::<u16>().ok()))
            })
        else {
            return Ok(None);
        };

        Ok(Some(TierEndpoint {
            host: "127.0.0.1".to_string(),
            port,
            user: "cbox".to_string(),
            ssh_options: Vec::new(),
        }))
    }
}

async fn create_container(docker: &Docker, tier: &str, config: &TierRunConfig) -> Result<()> {
    let name = container_name(tier);
    let opts = CreateContainerOptionsBuilder::default().name(&name).build();

    let env: Vec<String> = config.env.iter().map(|(k, v)| format!("{k}={v}")).collect();

    let mut labels = HashMap::new();
    labels.insert(MANAGED_BY_LABEL.to_string(), MANAGED_BY_VALUE.to_string());
    labels.insert(TIER_LABEL.to_string(), tier.to_string());

    let mut port_bindings = HashMap::new();
    port_bindings.insert(
        SSH_CONTAINER_PORT.to_string(),
        Some(vec![PortBinding {
            // Bind to loopback only — the trust boundary is the host.
            host_ip: Some("127.0.0.1".to_string()),
            // Empty host_port asks the daemon to pick a free port.
            host_port: Some(String::new()),
        }]),
    );

    let host_config = HostConfig {
        privileged: Some(config.privileged),
        network_mode: Some(network_mode_str(config.network_mode).to_string()),
        binds: Some(binds_for(&config.mounts)),
        port_bindings: Some(port_bindings),
        ..Default::default()
    };

    let body = ContainerCreateBody {
        image: Some(config.image.clone()),
        env: Some(env),
        exposed_ports: Some(vec![SSH_CONTAINER_PORT.to_string()]),
        labels: Some(labels),
        host_config: Some(host_config),
        networking_config: Some(NetworkingConfig::default()),
        ..Default::default()
    };

    docker
        .create_container(Some(opts), body)
        .await
        .with_context(|| format!("create {name}"))?;
    Ok(())
}

fn binds_for(mounts: &[Mount]) -> Vec<String> {
    mounts
        .iter()
        .filter_map(|m| match &m.source {
            MountSource::HostPath(p) => {
                let mode = if m.read_only { ":ro" } else { "" };
                Some(format!("{}:{}{}", p.display(), m.target.display(), mode,))
            }
            // Named volumes will be encoded into ContainerCreateBody.mounts
            // when Phase 2 needs them — bind syntax is enough today.
            MountSource::Volume(_) => None,
        })
        .collect()
}

fn network_mode_str(mode: NetworkMode) -> &'static str {
    match mode {
        NetworkMode::Bridge => "bridge",
        NetworkMode::None => "none",
    }
}

fn map_state(status: ContainerStateStatusEnum) -> TierState {
    match status {
        ContainerStateStatusEnum::RUNNING | ContainerStateStatusEnum::RESTARTING => {
            TierState::Running
        }
        ContainerStateStatusEnum::PAUSED => TierState::Paused,
        ContainerStateStatusEnum::CREATED
        | ContainerStateStatusEnum::EXITED
        | ContainerStateStatusEnum::DEAD
        | ContainerStateStatusEnum::REMOVING
        | ContainerStateStatusEnum::STOPPING
        | ContainerStateStatusEnum::EMPTY => TierState::Stopped,
    }
}

fn map_summary_state(status: ContainerSummaryStateEnum) -> TierState {
    match status {
        ContainerSummaryStateEnum::RUNNING | ContainerSummaryStateEnum::RESTARTING => {
            TierState::Running
        }
        ContainerSummaryStateEnum::PAUSED => TierState::Paused,
        ContainerSummaryStateEnum::CREATED
        | ContainerSummaryStateEnum::EXITED
        | ContainerSummaryStateEnum::DEAD
        | ContainerSummaryStateEnum::REMOVING
        | ContainerSummaryStateEnum::STOPPING
        | ContainerSummaryStateEnum::EMPTY => TierState::Stopped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Image tag the DinD smoke test expects to find. Built by
    /// `just integration` (which runs `just base` first, then
    /// `cbox build --` is the operator's responsibility for their
    /// own tier images, OR our test target below builds the example
    /// tier itself before exercising it).
    const DIND_TEST_IMAGE: &str = "cbox-tier-dev:latest";

    #[test]
    fn container_name_uses_tier_prefix() {
        assert_eq!(container_name("dev"), "cbox-tier-dev");
        assert_eq!(container_name("auto"), "cbox-tier-auto");
    }

    #[test]
    fn binds_render_host_paths_with_ro_when_set() {
        let mounts = vec![
            Mount {
                source: MountSource::HostPath("/etc/foo".into()),
                target: "/in/foo".into(),
                read_only: false,
            },
            Mount {
                source: MountSource::HostPath("/etc/bar".into()),
                target: "/in/bar".into(),
                read_only: true,
            },
            Mount {
                source: MountSource::Volume("data".into()),
                target: "/in/data".into(),
                read_only: false,
            },
        ];
        let binds = binds_for(&mounts);
        assert_eq!(binds, vec!["/etc/foo:/in/foo", "/etc/bar:/in/bar:ro"]);
    }

    #[test]
    fn network_mode_strings() {
        assert_eq!(network_mode_str(NetworkMode::Bridge), "bridge");
        assert_eq!(network_mode_str(NetworkMode::None), "none");
    }

    #[test]
    fn state_mapping() {
        assert_eq!(
            map_state(ContainerStateStatusEnum::RUNNING),
            TierState::Running
        );
        assert_eq!(
            map_state(ContainerStateStatusEnum::PAUSED),
            TierState::Paused
        );
        assert_eq!(
            map_state(ContainerStateStatusEnum::EXITED),
            TierState::Stopped
        );
        assert_eq!(
            map_state(ContainerStateStatusEnum::EMPTY),
            TierState::Stopped
        );
        assert_eq!(
            map_state(ContainerStateStatusEnum::STOPPING),
            TierState::Stopped
        );

        assert_eq!(
            map_summary_state(ContainerSummaryStateEnum::RUNNING),
            TierState::Running
        );
        assert_eq!(
            map_summary_state(ContainerSummaryStateEnum::PAUSED),
            TierState::Paused
        );
        assert_eq!(
            map_summary_state(ContainerSummaryStateEnum::EXITED),
            TierState::Stopped
        );
        assert_eq!(
            map_summary_state(ContainerSummaryStateEnum::EMPTY),
            TierState::Stopped
        );
    }

    /// Smoke test: against a pre-built example tier image, start a tier
    /// via the [`Backend`] trait, exec `docker version` inside, then
    /// destroy. Verifies the Phase 1 deliverable ("can build a tier
    /// image, verify DinD works inside it") without raw `docker run`
    /// calls. The image build itself is exercised by `just integration`
    /// (which runs `cargo run -- build dev` against the example config
    /// first); this test panics if that image is missing.
    ///
    /// Ignored by default; run with `just integration`. The test is
    /// idempotent — it cleans up any prior `cbox-tier-dind-test`
    /// container before starting.
    #[tokio::test]
    #[ignore]
    async fn dind_smoke_via_backend() {
        let backend = LocalDockerBackend::new().expect("connect to docker");

        // Pre-flight: the tier image must exist. We don't build it here
        // — use `cargo run -- build dev` (or `just integration`) first.
        if backend
            .docker()
            .inspect_image(DIND_TEST_IMAGE)
            .await
            .is_err()
        {
            panic!(
                "missing image {DIND_TEST_IMAGE}. Build it first: \
                 CBOX_CONFIG=examples/full-setup/cbox.yaml \
                 cargo run -- build dev"
            );
        }

        let tier = "dind-test";

        // Idempotent setup: tear down any leftover container.
        let _ = backend.destroy(tier).await;

        let cfg = TierRunConfig {
            image: DIND_TEST_IMAGE.to_string(),
            env: Vec::new(),
            network_mode: NetworkMode::Bridge,
            privileged: true,
            mounts: Vec::new(),
        };

        let endpoint = backend
            .ensure_running(tier, &cfg)
            .await
            .expect("start tier");

        // SSH endpoint discovery: port was bound dynamically and reported
        // back. We only assert non-zero — the actual port is daemon-chosen.
        assert_eq!(endpoint.host, "127.0.0.1");
        assert_ne!(endpoint.port, 0, "ssh port should be discovered");
        assert_eq!(endpoint.user, "cbox");

        // Wait for dockerd to come up inside the container. supervisord
        // starts it asynchronously; poll up to ~60s. We require exec
        // exit-code 0 — dockerd typically prints a connection error to
        // stdout while still warming up, which is non-empty but not a
        // success.
        let mut version = None;
        for _ in 0..60 {
            match exec_capture(
                backend.docker(),
                &container_name(tier),
                vec!["docker", "version", "--format", "{{.Server.Version}}"],
            )
            .await
            {
                Ok((0, out)) if !out.trim().is_empty() => {
                    version = Some(out);
                    break;
                }
                _ => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
            }
        }

        let teardown = backend.destroy(tier).await;

        let version = version.expect("dockerd inside cbox-tier never reported a version");
        eprintln!("inner dockerd version: {}", version.trim());
        assert!(
            !version.trim().is_empty(),
            "expected non-empty docker server version"
        );
        teardown.expect("destroy tier");
    }

    /// Guard against regressions of `passwd -d cbox` in `base/Dockerfile`.
    /// Ubuntu 24.04 ships new users with `cbox:!:…` in `/etc/shadow`, which
    /// OpenSSH 9.6 with `UsePAM no` treats as fully locked — rejecting
    /// pubkey auth before even reading `authorized_keys`. The deleted-
    /// password state shows up as an empty second field.
    #[tokio::test]
    #[ignore]
    async fn cbox_account_is_not_password_locked() {
        let backend = LocalDockerBackend::new().expect("connect to docker");
        if backend
            .docker()
            .inspect_image(DIND_TEST_IMAGE)
            .await
            .is_err()
        {
            panic!(
                "missing image {DIND_TEST_IMAGE}. Build it first: \
                 CBOX_CONFIG=examples/full-setup/cbox.yaml \
                 cargo run -- build dev"
            );
        }

        let tier = "passwd-guard-test";
        let _ = backend.destroy(tier).await;

        let cfg = TierRunConfig {
            image: DIND_TEST_IMAGE.to_string(),
            env: Vec::new(),
            network_mode: NetworkMode::Bridge,
            privileged: true,
            mounts: Vec::new(),
        };
        backend
            .ensure_running(tier, &cfg)
            .await
            .expect("start tier");

        // `/etc/shadow` is root-readable only, so the exec runs as root
        // explicitly. May briefly fail while the entrypoint is still
        // populating the file; poll for a successful exec.
        let mut line = None;
        for _ in 0..30 {
            match exec_capture_as(
                backend.docker(),
                &container_name(tier),
                Some("root"),
                vec!["sh", "-c", "grep '^cbox:' /etc/shadow"],
            )
            .await
            {
                Ok((0, out)) if !out.trim().is_empty() => {
                    line = Some(out);
                    break;
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(200)).await,
            }
        }

        let teardown = backend.destroy(tier).await;

        let line = line.expect("getent shadow cbox returned nothing");
        // shadow(5) format: name:passwd:lastchange:min:max:warn:inactive:expire:reserved
        let pw_field = line
            .trim()
            .split(':')
            .nth(1)
            .expect("shadow line missing passwd field");
        assert!(
            !pw_field.starts_with('!') && !pw_field.starts_with('*'),
            "cbox account is locked ({line:?}); did `passwd -d cbox` get removed from base/Dockerfile?"
        );
        assert!(
            pw_field.is_empty(),
            "expected empty passwd field after `passwd -d cbox`, got {pw_field:?} ({line:?})"
        );
        teardown.expect("destroy tier");
    }

    /// Helper for execing a command in a running container and capturing
    /// stdout+stderr. Returns (exit_code, captured_output).
    async fn exec_capture(
        docker: &bollard::Docker,
        container: &str,
        cmd: Vec<&str>,
    ) -> Result<(i64, String)> {
        exec_capture_as(docker, container, None, cmd).await
    }

    /// Same as [`exec_capture`] but optionally runs the command as a
    /// specific user inside the container.
    async fn exec_capture_as(
        docker: &bollard::Docker,
        container: &str,
        user: Option<&str>,
        cmd: Vec<&str>,
    ) -> Result<(i64, String)> {
        use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
        use futures_util::StreamExt;

        let exec = docker
            .create_exec(
                container,
                CreateExecOptions {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(cmd),
                    user,
                    ..Default::default()
                },
            )
            .await?;
        let opts = StartExecOptions {
            detach: false,
            ..Default::default()
        };
        let mut buf = String::new();
        if let StartExecResults::Attached { mut output, .. } =
            docker.start_exec(&exec.id, Some(opts)).await?
        {
            while let Some(chunk) = output.next().await {
                let chunk = chunk?;
                buf.push_str(&chunk.to_string());
            }
        }
        let info = docker.inspect_exec(&exec.id).await?;
        Ok((info.exit_code.unwrap_or(-1), buf))
    }
}
