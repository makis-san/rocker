//! The Docker-facing core: connection management, the command/event split the UI
//! talks over, and (later) log/stats/exec streams.
//!
//! Nothing here depends on a UI toolkit. The `egui` app is one consumer; a TUI
//! or menubar app would drive the same [`Command`]/[`Event`] channels.

pub mod error;
pub mod protocol;
pub mod runtime;
pub mod service;

pub use error::{EngineError, Result};
pub use protocol::{Command, Event, LifecycleAction, LogLine, LogStream};
pub use runtime::{start, EngineHandle};
pub use service::{reduce_stats, DockerService, LocalDocker};
