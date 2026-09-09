//! The extension contract (PLAN §5.5).
//!
//! Extensions do not run render code against an immediate-mode host. They return
//! a declarative [`UiNode`] tree that the host draws with `egui` and routes
//! events back over. This crate holds the manifest, the capability set, and that
//! node vocabulary — shared by `rocker-ext-host` and (via generated bindings)
//! the WIT world in `wit/world.wit`.

use serde::{Deserialize, Serialize};

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
