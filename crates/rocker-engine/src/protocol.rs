//! The message vocabulary between the UI thread and the async engine.
//!
//! The UI posts [`Command`]s on an mpsc channel and never `.await`s. Background
//! tasks post [`Event`]s back; the UI drains them each frame and requests a
//! repaint (PLAN §3.2).

use rocker_core::{ConnectionId, Container, ContainerDetail, ContainerId, StatSample};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleAction {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Kill,
    /// Force-remove the container (and its anonymous volumes), running or
    /// not. There is no separate "stop first" step: Docker's force-remove
    /// already covers a running container.
    Remove,
}

impl LifecycleAction {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Pause => "pause",
            Self::Unpause => "unpause",
            Self::Kill => "kill",
            Self::Remove => "remove",
        }
    }
}

/// Which of a log line's two streams it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// One line of container output, newline stripped.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub stream: LogStream,
    pub text: String,
}

/// A request from the UI to the engine.
///
/// The `Open*` / `Close*` pairs each drive one background stream. Opening a
/// stream that is already running replaces it, so the UI can switch the target
/// container by sending `Open*` again without a matching `Close*` first.
#[derive(Debug, Clone)]
pub enum Command {
    /// Connect (or reconnect) and begin the events stream for this host.
    Connect(ConnectionId),
    /// One-shot refresh of the container list.
    RefreshContainers,
    /// One-shot query of Docker's on-disk usage (`/system/df`) for the tray
    /// summary. Answered with [`Event::DiskUsage`].
    RefreshDiskUsage,
    /// Run a lifecycle action against a container.
    Lifecycle {
        container: ContainerId,
        action: LifecycleAction,
    },
    /// Run one lifecycle action against several containers at once (a
    /// group's "start all" / "stop all" / "delete all"). Each container is
    /// acted on independently — one failing doesn't stop the rest — and the
    /// container list is refreshed once after the whole batch lands.
    BulkLifecycle {
        containers: Vec<ContainerId>,
        action: LifecycleAction,
    },
    /// One-shot full inspect of a container for the Overview tab.
    Inspect(ContainerId),
    /// Start following a container's logs (tail + follow).
    OpenLogs(ContainerId),
    CloseLogs,
    /// Start streaming a container's resource stats. Independent callers (a
    /// group's combined header total, a detail screen) can each open the same
    /// container; the engine reference-counts so the underlying stream stays
    /// up until every opener has closed it.
    OpenStats(ContainerId),
    CloseStats(ContainerId),
    /// Change the concurrent stats-stream cap (Settings > Live graphs),
    /// evicting the least-recently-opened streams right away if it's now
    /// lower than what's currently open.
    SetMaxStatsStreams(usize),
    /// Open an interactive `exec` shell session in a container.
    OpenExec(ContainerId),
    /// Bytes typed into the terminal, forwarded to the exec stdin.
    ExecInput(Vec<u8>),
    /// The terminal grid was resized; mirror it to the exec TTY.
    ExecResize {
        cols: u16,
        rows: u16,
    },
    CloseExec,
    /// Stop background work and drop connections.
    Shutdown,
}

/// A notification from the engine to the UI.
#[derive(Debug, Clone)]
pub enum Event {
    /// Connection state changed.
    Connected {
        connection: ConnectionId,
        version: String,
    },
    Disconnected {
        connection: ConnectionId,
        reason: String,
    },
    /// Full container list after a refresh or an events-driven reconcile.
    Containers(Vec<Container>),
    /// Docker's total on-disk usage in bytes, or `None` if the daemon didn't
    /// report it. Answer to [`Command::RefreshDiskUsage`].
    DiskUsage(Option<u64>),
    /// A lifecycle action finished.
    LifecycleDone {
        container: ContainerId,
        action: LifecycleAction,
    },
    /// Inspect result for the container screen.
    Inspected(Box<ContainerDetail>),
    /// A batch of log lines for the currently open logs stream.
    LogLines {
        container: ContainerId,
        lines: Vec<LogLine>,
    },
    /// The logs stream ended; `reason` is `None` on a clean EOF.
    LogsClosed { reason: Option<String> },
    /// One resource sample for an open stats stream.
    Stat {
        container: ContainerId,
        sample: StatSample,
    },
    /// A stats stream ended — closed deliberately, the container stopped, an
    /// engine error, or LRU eviction past the stats-stream cap.
    StatsClosed {
        container: ContainerId,
        reason: Option<String>,
    },
    /// The exec shell is attached and ready for input.
    ExecReady { container: ContainerId },
    /// Raw bytes from the exec stdout/stderr TTY.
    ExecOutput(Vec<u8>),
    /// The exec session ended.
    ExecClosed { reason: Option<String> },
    /// Something went wrong; surface it in a banner.
    Error(String),
}
