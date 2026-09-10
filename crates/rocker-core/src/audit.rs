//! The local exec/attach audit record (PLAN §6, §7).
//!
//! Every interactive session Rocker opens into a container is logged locally so
//! there is a trail of what ran where. The record is written when the session
//! ends, so it can carry the exit code.

use serde::{Deserialize, Serialize};

/// One completed terminal session against a container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecAudit {
    /// When the session started, Unix milliseconds.
    pub ts_ms: u64,
    /// The connection it ran over (`"local"` for the local daemon).
    pub connection_id: String,
    /// Full container id.
    pub container: String,
    /// Container name at the time, for a readable log.
    pub container_name: String,
    /// The argv actually exec'd (or `["<attach>"]` for an attach session).
    pub argv: Vec<String>,
    /// Process exit code, if the daemon reported one.
    pub exit_code: Option<i64>,
    /// Session length in seconds, if known.
    pub duration_secs: Option<u64>,
}
