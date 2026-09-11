//! The extension contract (PLAN §5.5).
//!
//! Extensions do not run render code against an immediate-mode host. They return
//! a declarative [`UiNode`] tree that the host draws with `egui` and routes
//! events back over. This crate holds the manifest, the capability set, and that
//! node vocabulary — shared by `rocker-ext-host` and (via generated bindings)
//! the WIT world in `wit/world.wit`.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Deny-by-default capabilities, granted at install and revocable (PLAN §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    ContainersRead,
    ContainersLifecycle,
    ContainersExec,
    LogsRead,
    StatsRead,
    ImagesRead,
    RegistriesRead,
    Network,
    Storage,
    Notifications,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// `rhai` scripting (Phase 5, ships first).
    Script,
    /// `wasmtime` component (Phase 5, second).
    Component,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub tier: Tier,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Extension entry point, relative to the extension folder.
    pub entry: String,
}

/// A manifest that would let an extension escape its installation directory or
/// cannot be addressed reliably by Rocker.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestError {
    /// Extension identifiers are used as stable configuration keys and local
    /// directory names, so their vocabulary is deliberately narrow.
    #[error("extension id `{id}` must contain only lowercase ASCII letters, digits, `-`, or `.`")]
    InvalidId { id: String },
    /// The entry point must remain inside the installed extension directory.
    #[error("extension entry `{entry}` must be a relative path without `..` components")]
    InvalidEntry { entry: String },
    /// Repeated capabilities would make install prompts and configuration
    /// comparisons ambiguous.
    #[error("extension `{id}` declares capability {cap:?} more than once")]
    DuplicateCapability { id: String, cap: Capability },
}

impl Manifest {
    /// Validate fields which define the local extension boundary.
    ///
    /// This does not grant the requested capabilities. Installers must obtain
    /// those grants explicitly before activating the extension.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.id.is_empty()
            || !self.id.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
            })
        {
            return Err(ManifestError::InvalidId {
                id: self.id.clone(),
            });
        }

        let entry_path = Path::new(&self.entry);
        if self.entry.is_empty()
            || entry_path.is_absolute()
            || entry_path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(ManifestError::InvalidEntry {
                entry: self.entry.clone(),
            });
        }

        let mut capabilities = std::collections::HashSet::new();
        for capability in &self.capabilities {
            if !capabilities.insert(*capability) {
                return Err(ManifestError::DuplicateCapability {
                    id: self.id.clone(),
                    cap: *capability,
                });
            }
        }

        Ok(())
    }
}

/// The constrained UI vocabulary for v1 (PLAN §5.5). Expanded from real
/// extension needs, not up front.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "node")]
pub enum UiNode {
    Label {
        text: String,
    },
    Button {
        id: String,
        text: String,
    },
    TextInput {
        id: String,
        value: String,
    },
    Row {
        children: Vec<UiNode>,
    },
    Column {
        children: Vec<UiNode>,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

/// An event the host routes back to the extension after a render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum UiEvent {
    Clicked { id: String },
    Changed { id: String, value: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest {
            id: "example.extension".into(),
            name: "Example".into(),
            version: "0.1.0".into(),
            tier: Tier::Script,
            capabilities: vec![Capability::ContainersRead],
            entry: "main.rhai".into(),
        }
    }

    #[test]
    fn validate_accepts_a_safe_manifest() {
        assert_eq!(manifest().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_parent_entry_paths() {
        let mut manifest = manifest();
        manifest.entry = "../outside.rhai".into();

        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::InvalidEntry { .. })
        ));
    }

    #[test]
    fn validate_rejects_duplicate_capabilities() {
        let mut manifest = manifest();
        manifest.capabilities.push(Capability::ContainersRead);

        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::DuplicateCapability { .. })
        ));
    }
}
