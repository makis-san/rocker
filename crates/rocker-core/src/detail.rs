//! The full inspect view of a container, independent of the Engine API wire
//! types. [`Container`](crate::Container) is the list-row summary; this is what
//! the container screen's Overview tab renders.

use serde::{Deserialize, Serialize};

use crate::{ContainerId, ContainerState, PortBinding};

/// A bind mount, named volume, or tmpfs attached to the container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountInfo {
    /// `bind`, `volume`, `tmpfs`, …
    pub kind: String,
    /// Named volume name, when the mount is a volume.
    pub name: Option<String>,
    /// Host path (bind) or volume mountpoint.
    pub source: String,
    /// Path inside the container.
    pub destination: String,
    pub read_write: bool,
}

/// One network the container is attached to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInfo {
    pub name: String,
    pub ip: String,
    pub gateway: String,
    pub mac: String,
}

/// Health-check state, present only when the image declares a `HEALTHCHECK`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthInfo {
    /// `starting`, `healthy`, `unhealthy`.
    pub status: String,
    pub failing_streak: i64,
    /// Trimmed output of the most recent probe.
    pub last_output: Option<String>,
}

/// Everything the container screen needs about one container. Built from a
/// single Engine `inspect` call (PLAN §5.3, Phase 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerDetail {
    pub id: ContainerId,
    pub name: String,
    pub image: String,
    pub image_id: String,
    pub state: ContainerState,
    /// Human status line, e.g. `Up 3 hours (healthy)` — synthesised from the
    /// inspect state so the screen header matches the list row.
    pub status_line: String,
    /// RFC 3339 timestamps straight from the Engine (empty when absent).
    pub created: String,
    pub started_at: String,
    pub finished_at: String,
    pub restart_count: i64,
    pub exit_code: Option<i64>,
    pub error: Option<String>,
    /// Entrypoint + command joined the way `docker ps` shows it.
    pub command: String,
    pub working_dir: String,
    pub user: String,
    pub restart_policy: String,
    pub platform: String,
    pub log_path: String,
    /// `(key, value)` pairs, sorted by key.
    pub env: Vec<(String, String)>,
    pub labels: Vec<(String, String)>,
    pub ports: Vec<PortBinding>,
    pub mounts: Vec<MountInfo>,
    pub networks: Vec<NetworkInfo>,
    pub health: Option<HealthInfo>,
    pub compose_project: Option<String>,
    pub compose_service: Option<String>,
}
