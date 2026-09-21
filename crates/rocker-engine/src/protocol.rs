//! The message vocabulary between the UI thread and the async engine.
//!
//! The UI posts [`Command`]s on an mpsc channel and never `.await`s. Background
//! tasks post [`Event`]s back; the UI drains them each frame and requests a
//! repaint (PLAN §3.2).

use rocker_core::{
    Connection, ConnectionId, Container, ContainerDetail, ContainerId, ExecAudit, KubernetesPod,
    KubernetesWorkloadRef, StatSample,
};

/// Kubernetes-native lifecycle operations for scalable workloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KubernetesLifecycleAction {
    /// Scale a workload to zero replicas.
    Stop,
    /// Restore a previously recorded replica count.
    Start { replicas: u32 },
    /// Ask the controller for a rolling restart.
    Restart,
}

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

/// How much history to pull before switching to follow mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogTail {
    /// The last `n` lines.
    Lines(u32),
    /// Every line the engine still has.
    All,
}

impl LogTail {
    /// The value Docker's `tail=` query parameter expects.
    pub fn as_param(self) -> String {
        match self {
            LogTail::Lines(n) => n.to_string(),
            LogTail::All => "all".to_string(),
        }
    }
}

impl Default for LogTail {
    fn default() -> Self {
        LogTail::Lines(400)
    }
}

/// One line of container output, newline stripped.
///
/// The engine always asks Docker for timestamps, so `ts` is populated whenever
/// Docker prefixed the line with one (`Some` for real container output, `None`
/// for a synthetic line the client itself produced). The UI decides whether to
/// show it — toggling that is a pure view change, no stream restart.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub stream: LogStream,
    pub ts: Option<String>,
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
    Connect(Connection),
    /// One-shot refresh of the container list.
    RefreshContainers,
    /// One-shot query of Docker's on-disk usage (`/system/df`) for the tray
    /// summary. Answered with [`Event::DiskUsage`].
    RefreshDiskUsage,
    /// Query Pods through the selected local kubeconfig without blocking the UI.
    RefreshKubernetesPods {
        kubeconfig: String,
        context: String,
    },
    /// Aggregate Metrics Server usage for one Kubernetes context.
    RefreshKubernetesUsage {
        kubeconfig: String,
        context: String,
    },
    /// Apply a Kubernetes-native lifecycle action to one workload.
    KubernetesLifecycle {
        kubeconfig: String,
        workload: KubernetesWorkloadRef,
        action: KubernetesLifecycleAction,
    },
    /// Fetch a read-only Pod description for the Kubernetes detail screen.
    InspectKubernetesPod {
        kubeconfig: String,
        pod: KubernetesPod,
    },
    /// Fetch a bounded Pod log tail across its containers.
    KubernetesPodLogs {
        kubeconfig: String,
        pod: KubernetesPod,
    },
    /// Fetch current Pod resource usage from Metrics Server when available.
    KubernetesPodStats {
        kubeconfig: String,
        pod: KubernetesPod,
    },
    /// Start an interactive shell in a Kubernetes Pod.
    OpenKubernetesExec {
        kubeconfig: String,
        pod: KubernetesPod,
    },
    /// Send bytes to the active Kubernetes exec session.
    KubernetesExecInput(Vec<u8>),
    /// Close the active Kubernetes exec session.
    CloseKubernetesExec,
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
    /// Start following a container's logs: pull `tail` of history, then follow.
    /// Sending this again (e.g. after the tail size changes) replaces the
    /// running stream.
    OpenLogs {
        container: ContainerId,
        tail: LogTail,
    },
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
    /// Set how long usage samples are kept in the history store before the
    /// prune timer drops them (Settings > retention).
    SetStatsRetentionHours(u32),
    /// Read this container's persisted usage history and answer with
    /// [`Event::StatHistory`], so the Stats tab opens with real history rather
    /// than a blank chart that fills over minutes.
    LoadStatHistory(ContainerId),
    /// Read recent terminal-session audit rows and answer with
    /// [`Event::ExecAuditLog`].
    LoadExecAudit,
    /// Open an interactive `exec` shell session in a container.
    OpenExec(ContainerId),
    /// Attach to the container's main process stdio (PLAN §5.3). Shares the
    /// entrypoint's TTY — input, Ctrl-C and resize reach it directly. Uses the
    /// same `Exec*` input/output/close vocabulary as a shell session; only one
    /// interactive session is open at a time.
    OpenAttach(ContainerId),
    /// Bytes typed into the terminal, forwarded to the session's stdin.
    ExecInput(Vec<u8>),
    /// The terminal grid was resized; mirror it to the session's TTY.
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
    /// Pods returned from a read-only Kubernetes query.
    KubernetesPods {
        context: String,
        pods: Vec<KubernetesPod>,
    },
    /// Kubernetes query failure kept separate from Docker connection errors.
    KubernetesPodsFailed(String),
    /// Aggregate CPU millicores and memory bytes for one Kubernetes context.
    KubernetesUsage {
        context: String,
        cpu_millicores: u64,
        memory_bytes: u64,
    },
    /// A workload action completed; Stop carries the count needed for Start.
    KubernetesLifecycleDone {
        workload: KubernetesWorkloadRef,
        action: KubernetesLifecycleAction,
        previous_replicas: Option<u32>,
    },
    /// A Kubernetes lifecycle action failed without affecting Docker state.
    KubernetesLifecycleFailed(String),
    KubernetesPodInfo {
        pod: KubernetesPod,
        text: String,
    },
    KubernetesPodLogs {
        pod: KubernetesPod,
        text: String,
    },
    KubernetesPodStats {
        pod: KubernetesPod,
        text: String,
    },
    KubernetesPodQueryFailed {
        pod: KubernetesPod,
        message: String,
    },
    KubernetesExecReady {
        pod: KubernetesPod,
    },
    KubernetesExecOutput {
        pod: KubernetesPod,
        bytes: Vec<u8>,
    },
    KubernetesExecClosed {
        pod: KubernetesPod,
        reason: Option<String>,
    },
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
    LogsClosed {
        reason: Option<String>,
    },
    /// One resource sample for an open stats stream.
    Stat {
        container: ContainerId,
        sample: StatSample,
    },
    /// A container's persisted usage history, oldest first (answer to
    /// [`Command::LoadStatHistory`]). Empty if there is no store or no rows.
    StatHistory {
        container: ContainerId,
        samples: Vec<StatSample>,
    },
    /// Recent terminal-session audit rows, newest first (answer to
    /// [`Command::LoadExecAudit`]).
    ExecAuditLog(Vec<ExecAudit>),
    /// A stats stream ended — closed deliberately, the container stopped, an
    /// engine error, or LRU eviction past the stats-stream cap.
    StatsClosed {
        container: ContainerId,
        reason: Option<String>,
    },
    /// The exec shell is attached and ready for input.
    ExecReady {
        container: ContainerId,
    },
    /// Raw bytes from the exec stdout/stderr TTY.
    ExecOutput(Vec<u8>),
    /// The exec session ended.
    ExecClosed {
        reason: Option<String>,
    },
    /// Something went wrong; surface it in a banner.
    Error(String),
}
