//! How Rocker reaches a Docker Engine API endpoint.

use serde::{Deserialize, Serialize};

/// Stable identifier for a configured connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectionId(pub String);

impl ConnectionId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Transport used to talk to the daemon (PLAN §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ConnectionKind {
    /// Unix domain socket, e.g. `/var/run/docker.sock`.
    Socket { path: String },
    /// Windows named pipe, e.g. `//./pipe/docker_engine`.
    NamedPipe { path: String },
    /// TCP with mandatory TLS client certificates.
    Tcp {
        host: String,
        port: u16,
        tls: TlsConfig,
    },
    /// `ssh://user@host`, tunneled via `russh`.
    Ssh { uri: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsConfig {
    pub ca_cert: String,
    pub client_cert: String,
    pub client_key: String,
    /// Accept a server certificate that does not chain to a known CA.
    #[serde(default)]
    pub allow_self_signed: bool,
}

/// A user-configured Docker host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub id: ConnectionId,
    pub name: String,
    #[serde(flatten)]
    pub kind: ConnectionKind,
    /// The connection selected on startup when nothing else is chosen.
    #[serde(default)]
    pub default: bool,
}

impl Connection {
    /// The conventional local daemon on the current platform.
    pub fn local_default() -> Self {
        #[cfg(windows)]
        let kind = ConnectionKind::NamedPipe {
            path: r"//./pipe/docker_engine".to_string(),
        };
        #[cfg(not(windows))]
        let kind = ConnectionKind::Socket {
            path: "/var/run/docker.sock".to_string(),
        };

        Self {
            id: ConnectionId::new("local"),
            name: "Local".to_string(),
            kind,
            default: true,
        }
    }
}
