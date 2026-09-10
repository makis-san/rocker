//! Domain models and pure logic shared by every Rocker front-end.
//!
//! This crate has no I/O and no UI dependency. It is the common vocabulary the
//! `egui` app, a future menubar app, and `rocker-tui` all speak.

pub mod audit;
pub mod connection;
pub mod container;
pub mod detail;
pub mod group;
pub mod stat;

pub use audit::ExecAudit;
pub use connection::{Connection, ConnectionId, ConnectionKind};
pub use container::{Container, ContainerId, ContainerState, PortBinding};
pub use detail::{ContainerDetail, HealthInfo, MountInfo, NetworkInfo};
pub use group::{Group, GroupId, GroupKind, GroupRule};
pub use stat::StatSample;

/// Semantic version of the token schema that themes and extensions freeze
/// against. Bump on any breaking change to the token set (see PLAN §5.4).
pub const TOKEN_SCHEMA_VERSION: u32 = 1;
