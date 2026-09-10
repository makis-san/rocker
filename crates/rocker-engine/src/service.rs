//! The Docker service abstraction.
//!
//! [`DockerService`] is the seam the rest of the engine builds on. The real
//! implementation ([`LocalDocker`]) is a thin `bollard` wrapper; tests use a
//! fake. Capability probing (Podman, rootless, old API) lives behind this trait
//! so unsupported calls are disabled rather than erroring (PLAN §4).

use bollard::models::{ContainerInspectResponse, ContainerStatsResponse};
use bollard::query_parameters as qp;

use rocker_core::{
    Container, ContainerDetail, ContainerId, ContainerState, HealthInfo, MountInfo, NetworkInfo,
    PortBinding, StatSample,
};

use crate::error::{EngineError, Result};
use crate::protocol::LifecycleAction;

/// Minimum Engine API we intend to support. Pinned properly before Phase 1
/// (PLAN §11.6); this is a placeholder that keeps the probe path exercised.
pub const MIN_API_VERSION: &str = "1.41";

#[allow(async_fn_in_trait)]
pub trait DockerService {
    /// Handshake with the daemon; returns its reported API version.
    async fn version(&self) -> Result<String>;

    /// Current containers (all states).
    async fn list_containers(&self) -> Result<Vec<Container>>;

    /// Apply a lifecycle action to one container.
    async fn lifecycle(&self, id: &ContainerId, action: LifecycleAction) -> Result<()>;
}

/// `bollard`-backed client for the local daemon (unix socket / named pipe).
///
/// `Clone` is cheap — `bollard::Docker` is an `Arc` handle — so background
/// stream tasks each take their own copy.
#[derive(Clone)]
pub struct LocalDocker {
    inner: bollard::Docker,
}

impl LocalDocker {
    /// Connect using Docker's own environment/default resolution.
    pub fn connect() -> Result<Self> {
        let inner = bollard::Docker::connect_with_local_defaults()
            .map_err(|e| EngineError::Unreachable(e.to_string()))?;
        Ok(Self { inner })
    }

    /// The underlying `bollard` handle, for the streaming endpoints (`logs`,
    /// `stats`, `exec`) that the engine drives on their own tasks.
    pub fn raw(&self) -> bollard::Docker {
        self.inner.clone()
    }

    /// Full inspect, reduced to [`ContainerDetail`].
    pub async fn inspect(&self, id: &ContainerId) -> Result<ContainerDetail> {
        let raw = self
            .inner
            .inspect_container(&id.0, None::<qp::InspectContainerOptions>)
            .await
            .map_err(|e| EngineError::Docker(e.to_string()))?;
        Ok(map_detail(raw))
    }

    /// Total bytes Docker is holding on disk — image layers, container
    /// writable layers, local volumes, and build cache — summed from
    /// `/system/df`.
    ///
    /// `Ok(None)` means the daemon answered but carried no usage figures: the
    /// pre-API-1.53 `/system/df` body shape, which this client doesn't parse.
    pub async fn disk_usage(&self) -> Result<Option<u64>> {
        let df = self
            .inner
            .df(None::<qp::DataUsageOptions>)
            .await
            .map_err(|e| EngineError::Docker(e.to_string()))?;

        let sections = [
            df.image_usage.and_then(|u| u.total_size),
            df.container_usage.and_then(|u| u.total_size),
            df.volume_usage.and_then(|u| u.total_size),
            df.build_cache_usage.and_then(|u| u.total_size),
        ];
        if sections.iter().all(Option::is_none) {
            return Ok(None);
        }
        let total: i64 = sections.into_iter().flatten().filter(|&n| n > 0).sum();
        Ok(Some(total as u64))
    }
}

impl DockerService for LocalDocker {
    async fn version(&self) -> Result<String> {
        let v = self
            .inner
            .version()
            .await
            .map_err(|e| EngineError::Unreachable(e.to_string()))?;
        Ok(v.api_version.unwrap_or_else(|| "unknown".to_string()))
    }

    async fn list_containers(&self) -> Result<Vec<Container>> {
        let opts = qp::ListContainersOptionsBuilder::new().all(true).build();
        let raw = self
            .inner
            .list_containers(Some(opts))
            .await
            .map_err(|e| EngineError::Docker(e.to_string()))?;

        Ok(raw.into_iter().map(map_container).collect())
    }

    async fn lifecycle(&self, id: &ContainerId, action: LifecycleAction) -> Result<()> {
        let name = id.0.as_str();
        let r = match action {
            LifecycleAction::Start => {
                self.inner
                    .start_container(name, Some(qp::StartContainerOptionsBuilder::new().build()))
                    .await
            }
            LifecycleAction::Stop => {
                self.inner
                    .stop_container(
                        name,
                        Some(qp::StopContainerOptionsBuilder::new().t(10).build()),
                    )
                    .await
            }
            LifecycleAction::Restart => {
                self.inner
                    .restart_container(
                        name,
                        Some(qp::RestartContainerOptionsBuilder::new().t(10).build()),
                    )
                    .await
            }
            LifecycleAction::Pause => self.inner.pause_container(name).await,
            LifecycleAction::Unpause => self.inner.unpause_container(name).await,
            LifecycleAction::Kill => {
                self.inner
                    .kill_container(
                        name,
                        Some(
                            qp::KillContainerOptionsBuilder::new()
                                .signal("SIGKILL")
                                .build(),
                        ),
                    )
                    .await
            }
            LifecycleAction::Remove => {
                self.inner
                    .remove_container(
                        name,
                        Some(
                            qp::RemoveContainerOptionsBuilder::new()
                                .force(true)
                                .v(true)
                                .build(),
                        ),
                    )
                    .await
            }
        };
        r.map_err(|e| EngineError::Docker(e.to_string()))
    }
}

fn map_container(c: bollard::models::ContainerSummary) -> Container {
    use bollard::models::ContainerSummaryStateEnum as S;

    let name = c
        .names
        .as_ref()
        .and_then(|n| n.first())
        .map(|n| n.trim_start_matches('/').to_string())
        .unwrap_or_default();

    let labels = c.labels.unwrap_or_default();

    let state = match c.state {
        Some(S::CREATED) => ContainerState::Created,
        Some(S::RUNNING) => ContainerState::Running,
        Some(S::PAUSED) => ContainerState::Paused,
        Some(S::RESTARTING) => ContainerState::Restarting,
        Some(S::REMOVING) => ContainerState::Removing,
        Some(S::EXITED) => ContainerState::Exited,
        Some(S::DEAD) => ContainerState::Dead,
        Some(S::STOPPING) | Some(S::EMPTY) | None => ContainerState::Unknown,
    };

    let ports = c
        .ports
        .unwrap_or_default()
        .into_iter()
        .map(|p| PortBinding {
            container_port: p.private_port,
            protocol: p
                .typ
                .map(|t| format!("{t:?}").to_lowercase())
                .unwrap_or_else(|| "tcp".into()),
            host_ip: p.ip,
            host_port: p.public_port,
        })
        .collect();

    let compose_project = labels.get("com.docker.compose.project").cloned();
    let compose_service = labels.get("com.docker.compose.service").cloned();
    let mut labels: Vec<(String, String)> = labels.into_iter().collect();
    labels.sort_by(|a, b| a.0.cmp(&b.0));

    Container {
        id: ContainerId::new(c.id.unwrap_or_default()),
        name,
        image: c.image.unwrap_or_default(),
        state,
        status: c.status.unwrap_or_default(),
        ports,
        compose_project,
        compose_service,
        labels,
    }
}

fn map_detail(c: ContainerInspectResponse) -> ContainerDetail {
    let state = c.state.clone().unwrap_or_default();
    let status = ContainerState::from_engine_str(
        state
            .status
            .map(|s| format!("{s:?}").to_lowercase())
            .unwrap_or_default()
            .as_str(),
    );

    let config = c.config.clone().unwrap_or_default();
    let host_config = c.host_config.clone().unwrap_or_default();

    let name = c
        .name
        .as_deref()
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_string();

    // Entrypoint + Cmd, joined the way `docker ps` renders the command column.
    let mut command_parts: Vec<String> = Vec::new();
    command_parts.extend(config.entrypoint.clone().unwrap_or_default());
    command_parts.extend(config.cmd.clone().unwrap_or_default());
    if command_parts.is_empty() {
        if let Some(path) = &c.path {
            command_parts.push(path.clone());
            command_parts.extend(c.args.clone().unwrap_or_default());
        }
    }
    let command = command_parts.join(" ");

    let mut env: Vec<(String, String)> = config
        .env
        .unwrap_or_default()
        .into_iter()
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (kv, String::new()),
        })
        .collect();
    env.sort_by(|a, b| a.0.cmp(&b.0));

    let labels_map = config.labels.unwrap_or_default();
    let compose_project = labels_map.get("com.docker.compose.project").cloned();
    let compose_service = labels_map.get("com.docker.compose.service").cloned();
    let mut labels: Vec<(String, String)> = labels_map.into_iter().collect();
    labels.sort_by(|a, b| a.0.cmp(&b.0));

    let ports = c
        .network_settings
        .as_ref()
        .and_then(|n| n.ports.clone())
        .map(map_ports)
        .unwrap_or_default();

    let mounts = c
        .mounts
        .unwrap_or_default()
        .into_iter()
        .map(|m| MountInfo {
            kind: m
                .typ
                .map(|t| format!("{t:?}").to_lowercase())
                .unwrap_or_else(|| "bind".into()),
            name: m.name.filter(|s| !s.is_empty()),
            source: m.source.unwrap_or_default(),
            destination: m.destination.unwrap_or_default(),
            read_write: m.rw.unwrap_or(true),
        })
        .collect();

    let networks = c
        .network_settings
        .as_ref()
        .and_then(|n| n.networks.clone())
        .unwrap_or_default()
        .into_iter()
        .map(|(name, e)| NetworkInfo {
            name,
            ip: e.ip_address.unwrap_or_default(),
            gateway: e.gateway.unwrap_or_default(),
            mac: e.mac_address.unwrap_or_default(),
        })
        .collect::<Vec<_>>();

    let health = state.health.as_ref().map(|h| HealthInfo {
        status: h
            .status
            .map(|s| format!("{s:?}").to_lowercase())
            .unwrap_or_default(),
        failing_streak: h.failing_streak.unwrap_or(0),
        last_output: h
            .log
            .as_ref()
            .and_then(|l| l.last())
            .and_then(|r| r.output.clone())
            .map(|o| o.trim().to_string())
            .filter(|s| !s.is_empty()),
    });

    let restart_policy = host_config
        .restart_policy
        .and_then(|p| p.name)
        .map(|n| {
            let s = format!("{n:?}").to_lowercase().replace('_', "-");
            if s.is_empty() {
                "no".to_string()
            } else {
                s
            }
        })
        .unwrap_or_else(|| "no".to_string());

    ContainerDetail {
        id: ContainerId::new(c.id.clone().unwrap_or_default()),
        name,
        image: config.image.clone().unwrap_or_default(),
        image_id: c.image.clone().unwrap_or_default(),
        state: status,
        status_line: status_line(
            status,
            &state.started_at,
            state.exit_code,
            state.error.as_deref(),
        ),
        created: c.created.unwrap_or_default(),
        started_at: state.started_at.unwrap_or_default(),
        finished_at: state.finished_at.unwrap_or_default(),
        restart_count: c.restart_count.unwrap_or(0),
        exit_code: state.exit_code,
        error: state.error.filter(|s| !s.is_empty()),
        command,
        working_dir: config.working_dir.unwrap_or_default(),
        user: config.user.unwrap_or_default(),
        restart_policy,
        platform: c.platform.unwrap_or_default(),
        log_path: c.log_path.unwrap_or_default(),
        env,
        labels,
        ports,
        mounts,
        networks,
        health,
        compose_project,
        compose_service,
    }
}

fn map_ports(map: bollard::models::PortMap) -> Vec<PortBinding> {
    let mut out = Vec::new();
    for (key, bindings) in map {
        // key is "80/tcp" or "80".
        let (port_s, proto) = key.split_once('/').unwrap_or((key.as_str(), "tcp"));
        let Ok(container_port) = port_s.parse::<u16>() else {
            continue;
        };
        match bindings {
            Some(list) if !list.is_empty() => {
                for b in list {
                    out.push(PortBinding {
                        container_port,
                        protocol: proto.to_string(),
                        host_ip: b.host_ip.filter(|s| !s.is_empty()),
                        host_port: b.host_port.as_deref().and_then(|s| s.parse().ok()),
                    });
                }
            }
            _ => out.push(PortBinding {
                container_port,
                protocol: proto.to_string(),
                host_ip: None,
                host_port: None,
            }),
        }
    }
    out.sort_by_key(|p| (p.container_port, p.host_port));
    out
}

fn status_line(
    state: ContainerState,
    started_at: &Option<String>,
    exit_code: Option<i64>,
    error: Option<&str>,
) -> String {
    match state {
        ContainerState::Running | ContainerState::Restarting => match started_at {
            Some(s) if !s.is_empty() => format!("Up (since {})", short_time(s)),
            _ => "Up".to_string(),
        },
        ContainerState::Paused => "Paused".to_string(),
        ContainerState::Exited | ContainerState::Dead => match error {
            Some(e) => format!("Exited ({}) — {e}", exit_code.unwrap_or(0)),
            None => format!("Exited ({})", exit_code.unwrap_or(0)),
        },
        ContainerState::Created => "Created".to_string(),
        ContainerState::Removing => "Removing".to_string(),
        ContainerState::Unknown => "Unknown".to_string(),
    }
}

/// `2026-09-08T14:03:11.123Z` → `2026-09-08 14:03`. Leaves anything it does not
/// recognise untouched.
fn short_time(rfc3339: &str) -> String {
    match (rfc3339.find('T'), rfc3339.len() >= 16) {
        (Some(t), true) if t + 6 <= rfc3339.len() => {
            format!("{} {}", &rfc3339[..t], &rfc3339[t + 1..t + 6])
        }
        _ => rfc3339.to_string(),
    }
}

/// Reduce one raw stats frame (which carries its own previous CPU totals) to a
/// [`StatSample`]. Mirrors `docker stats`' own maths.
pub fn reduce_stats(s: &ContainerStatsResponse) -> StatSample {
    let cpu = s.cpu_stats.clone().unwrap_or_default();
    let precpu = s.precpu_stats.clone().unwrap_or_default();

    let cpu_total = cpu
        .cpu_usage
        .as_ref()
        .and_then(|u| u.total_usage)
        .unwrap_or(0);
    let pre_total = precpu
        .cpu_usage
        .as_ref()
        .and_then(|u| u.total_usage)
        .unwrap_or(0);
    let sys = cpu.system_cpu_usage.unwrap_or(0);
    let pre_sys = precpu.system_cpu_usage.unwrap_or(0);

    let cores = cpu
        .online_cpus
        .or_else(|| {
            cpu.cpu_usage
                .as_ref()
                .and_then(|u| u.percpu_usage.as_ref())
                .map(|v| v.len() as u32)
        })
        .filter(|&n| n > 0)
        .unwrap_or(1) as f32;

    let cpu_delta = cpu_total.saturating_sub(pre_total) as f64;
    let sys_delta = sys.saturating_sub(pre_sys) as f64;
    let cpu_pct = if sys_delta > 0.0 && cpu_delta > 0.0 {
        (cpu_delta / sys_delta * cores as f64 * 100.0) as f32
    } else {
        0.0
    };

    let mem = s.memory_stats.clone().unwrap_or_default();
    // Match `docker stats`: subtract the reclaimable page cache.
    let cache = mem
        .stats
        .as_ref()
        .and_then(|m| {
            m.get("inactive_file")
                .or_else(|| m.get("total_inactive_file"))
        })
        .copied()
        .unwrap_or(0);
    let mem_used = mem.usage.unwrap_or(0).saturating_sub(cache);
    let mem_limit = mem.limit.unwrap_or(0);

    let (mut net_rx, mut net_tx) = (0u64, 0u64);
    for n in s.networks.clone().unwrap_or_default().values() {
        net_rx += n.rx_bytes.unwrap_or(0);
        net_tx += n.tx_bytes.unwrap_or(0);
    }

    let (mut blk_read, mut blk_write) = (0u64, 0u64);
    if let Some(io) = s
        .blkio_stats
        .as_ref()
        .and_then(|b| b.io_service_bytes_recursive.as_ref())
    {
        for e in io {
            match e.op.as_deref().map(str::to_ascii_lowercase).as_deref() {
                Some("read") => blk_read += e.value.unwrap_or(0),
                Some("write") => blk_write += e.value.unwrap_or(0),
                _ => {}
            }
        }
    }

    StatSample {
        // Stamped by the caller (`run_stats`) right before it emits, so a
        // sample carries the time it reached us, not the time it was reduced.
        ts_ms: 0,
        cpu_pct,
        cpu_cores: cores,
        mem_used,
        mem_limit,
        net_rx,
        net_tx,
        blk_read,
        blk_write,
        pids: s.pids_stats.as_ref().and_then(|p| p.current).unwrap_or(0),
    }
}
