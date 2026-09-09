//! In-app terminal (PLAN §5.3, Phase 2).
//!
//! The plan: feed bytes from a Docker `exec` + `attach` hijacked stream into
//! `alacritty_terminal`'s VT state machine, then render the grid as per-row
//! `LayoutJob`s with a galley cache keyed by a row content hash so only dirty
//! rows re-layout.
//!
//! This scaffold fixes the public shape (a [`TermSize`] and a [`TermEvent`]
//! channel message) so `rocker-ui` can lay out a placeholder pane now. The VT
//! wiring is deliberately not here yet.

mod vt;

pub use vt::{keys, Cell, Color, Screen};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TermError {
    #[error("exec stream closed: {0}")]
    StreamClosed(String),
}

/// Grid dimensions, mirrored to the Docker exec resize endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TermSize {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

/// What the UI receives from a running session.
#[derive(Debug, Clone)]
pub enum TermEvent {
    /// The visible grid changed; the UI should repaint.
    Damaged,
    /// The session ended (process exit or stream drop).
    Closed(String),
}
