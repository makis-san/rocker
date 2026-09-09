//! A container as Rocker models it, independent of the Engine API wire types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContainerId(pub String);

impl ContainerId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// First 12 hex characters, the form Docker shows by default.
    pub fn short(&self) -> &str {
        let n = self.0.len().min(12);
        &self.0[..n]
    }
}

impl std::fmt::Display for ContainerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lifecycle state, normalized from the Engine's free-form status string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerState {
    Created,
    Running,
    Paused,
    Restarting,
    Removing,
    Exited,
    Dead,
    Unknown,
}

impl ContainerState {
    pub fn from_engine_str(s: &str) -> Self {
        match s {
            "created" => Self::Created,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "restarting" => Self::Restarting,
            "removing" => Self::Removing,
            "exited" => Self::Exited,
            "dead" => Self::Dead,
            _ => Self::Unknown,
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Restarting)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortBinding {
    pub container_port: u16,
    pub protocol: String,
    pub host_ip: Option<String>,
    pub host_port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Container {
    pub id: ContainerId,
    /// Primary name with the leading `/` stripped.
    pub name: String,
    pub image: String,
    pub state: ContainerState,
    /// Raw Engine status line, e.g. `Up 3 hours (healthy)`.
    pub status: String,
    #[serde(default)]
    pub ports: Vec<PortBinding>,
    #[serde(default)]
    pub compose_project: Option<String>,
    #[serde(default)]
    pub compose_service: Option<String>,
}
