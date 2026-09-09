//! Persistence for Rocker.
//!
//! Two stores, by shape of data (PLAN §6):
//! - **Config** — connections, groups, settings — hand-editable TOML.
//! - **History** — usage samples and the exec audit log — `redb`, pruned on a
//!   timer to a retention window.
//!
//! This scaffold defines the config types and paths; the `redb` schemas land
//! with the stats collector in Phase 2.

pub mod config;
pub mod paths;

pub use config::{Config, Settings};
pub use paths::AppPaths;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("config parse: {0}")]
    Parse(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;
