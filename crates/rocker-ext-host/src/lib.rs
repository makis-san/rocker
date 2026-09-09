//! Extension host supervisor (PLAN §5.5).
//!
//! Runs as a **separate supervised process** from the UI for crash isolation and
//! per-extension resource ceilings. This scaffold defines the capability gate
//! and the supervisor's public surface; the `rhai` runtime (Phase 5a) and the
//! `wasmtime` + Component Model runtime (Phase 5b) attach behind
//! [`ExtensionRuntime`].

use std::collections::HashSet;

use rocker_ext_api::{Capability, Manifest};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostError {
    #[error("extension {ext} requested capability {cap:?} that was not granted")]
    CapabilityDenied { ext: String, cap: Capability },
    #[error("extension {0} crashed")]
    Crashed(String),
    #[error("runtime: {0}")]
    Runtime(String),
}

pub type Result<T> = std::result::Result<T, HostError>;

/// The set of capabilities a user granted one extension. All host API calls are
/// checked against this before dispatch.
#[derive(Debug, Clone, Default)]
pub struct CapabilityGate {
    ext_id: String,
    granted: HashSet<Capability>,
}

impl CapabilityGate {
    pub fn new(manifest: &Manifest) -> Self {
        Self {
            ext_id: manifest.id.clone(),
            granted: manifest.capabilities.iter().copied().collect(),
        }
    }

    pub fn require(&self, cap: Capability) -> Result<()> {
        if self.granted.contains(&cap) {
            Ok(())
        } else {
            Err(HostError::CapabilityDenied {
                ext: self.ext_id.clone(),
                cap,
            })
        }
    }
}

/// Implemented by each runtime tier.
pub trait ExtensionRuntime {
    fn activate(&mut self) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocker_ext_api::Tier;

    fn manifest(caps: Vec<Capability>) -> Manifest {
        Manifest {
            id: "example".into(),
            name: "Example".into(),
            version: "0.1.0".into(),
            tier: Tier::Script,
            capabilities: caps,
            entry: "main.rhai".into(),
        }
    }

    #[test]
    fn gate_enforces_grants() {
        let gate = CapabilityGate::new(&manifest(vec![Capability::ContainersRead]));
        assert!(gate.require(Capability::ContainersRead).is_ok());
        assert!(gate.require(Capability::ContainersLifecycle).is_err());
    }
}
