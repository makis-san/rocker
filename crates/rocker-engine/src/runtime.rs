//! The background engine task and the handle the UI drives it through.
//!
//! The UI never `.await`s. It holds an [`EngineHandle`], pushes [`Command`]s with
//! a non-blocking send, and drains [`Event`]s with `try_recv` once per frame
//! (PLAN §3.2). After emitting an event the task calls the supplied repaint
//! callback so an idle UI wakes exactly when there is something to show.
//!
//! Each `Open*` command spawns one background stream task; its handle lives in
//! [`EngineTask`] and aborts on drop, so replacing or closing a stream is just a
//! reassignment. The command loop itself never blocks on a stream.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use rocker_core::{ConnectionId, ContainerId, ExecAudit};
use rocker_store::HistoryStore;

use crate::protocol::{Command, Event, LogLine, LogStream, LogTail};
use crate::service::{self, DockerService, LocalDocker};

/// Default retention window until the UI sends the persisted value.
const DEFAULT_RETENTION_HOURS: u64 = 24;
/// The exec audit log is kept far longer than usage samples — it's tiny.
const AUDIT_RETENTION_MS: u64 = 90 * 24 * 3600 * 1000;
/// How often the prune timer sweeps the history store.
const PRUNE_EVERY: Duration = Duration::from_secs(300);

pub struct EngineHandle {
    commands: UnboundedSender<Command>,
    events: UnboundedReceiver<Event>,
}

impl EngineHandle {
    /// Queue a command. Fails only if the engine task has stopped.
    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }

    /// Non-blocking drain of one pending event.
    pub fn try_recv(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

/// Matches `rocker_store::Settings::default().max_stats_streams` — the UI
/// sends its own `SetMaxStatsStreams` once it has loaded the persisted value,
/// so this is only what's in effect before that lands.
const DEFAULT_MAX_STATS_STREAMS: usize = 12;

/// Spawn the engine task on `rt` and return its handle.
///
/// `repaint` is invoked after every emitted event; wire it to
/// `egui::Context::request_repaint`. `history`, when present, receives usage
/// samples and exec-audit rows, and a background timer prunes it to the
/// retention window (updated via [`Command::SetStatsRetentionHours`]).
pub fn start(
    rt: &tokio::runtime::Handle,
    repaint: impl Fn() + Send + Sync + 'static,
    history: Option<Arc<HistoryStore>>,
) -> EngineHandle {
    let (cmd_tx, cmd_rx) = unbounded_channel::<Command>();
    let (evt_tx, evt_rx) = unbounded_channel::<Event>();

    let emitter = Emitter {
        tx: evt_tx,
        repaint: Arc::new(repaint),
    };

    let retention_ms = Arc::new(AtomicU64::new(DEFAULT_RETENTION_HOURS * 3600 * 1000));

    if let Some(h) = history.clone() {
        let ret = retention_ms.clone();
        rt.spawn(async move {
            let mut tick = tokio::time::interval(PRUNE_EVERY);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let now = now_ms();
                let usage_before = now.saturating_sub(ret.load(Ordering::Relaxed));
                let h = h.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = h.prune_samples(usage_before);
                    let _ = h.prune_exec(now.saturating_sub(AUDIT_RETENTION_MS));
                })
                .await;
            }
        });
    }

    rt.spawn(async move {
        let mut task = EngineTask {
            docker: None,
            connection: None,
            emitter,
            history,
            retention_ms,
            logs: None,
            stats: HashMap::new(),
            stats_order: VecDeque::new(),
            max_stats_streams: DEFAULT_MAX_STATS_STREAMS,
            exec: None,
        };
        task.run(cmd_rx).await;
    });

    EngineHandle {
        commands: cmd_tx,
        events: evt_rx,
    }
}

/// A cloneable event sink shared by the command loop and every stream task.
#[derive(Clone)]
struct Emitter {
    tx: UnboundedSender<Event>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl Emitter {
    fn emit(&self, event: Event) {
        if self.tx.send(event).is_ok() {
            (self.repaint)();
        }
    }
}

/// A background stream. Aborts the moment it is dropped.
struct StreamTask(JoinHandle<()>);

impl Drop for StreamTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A running exec session: the task plus the channel its stdin/resize messages
/// travel on.
struct ExecTask {
    task: JoinHandle<()>,
    input: UnboundedSender<ExecMsg>,
}

impl Drop for ExecTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum ExecMsg {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

struct EngineTask {
    docker: Option<LocalDocker>,
    /// The connection id currently in use, for the exec audit trail.
    connection: Option<ConnectionId>,
    emitter: Emitter,
    /// Persisted usage + audit history, when a store opened.
    history: Option<Arc<HistoryStore>>,
    /// Retention window in milliseconds, shared with the prune timer.
    retention_ms: Arc<AtomicU64>,
    logs: Option<StreamTask>,
    /// One entry per container currently streaming stats, plus how many
    /// callers have it open (the list view's group aggregates and a detail
    /// screen can overlap on the same container without stepping on each
    /// other — the stream only actually closes once every opener has).
    stats: HashMap<ContainerId, (StreamTask, u32)>,
    /// Insertion order of `stats`, oldest first, so a cap breach evicts the
    /// least-recently-opened stream (PLAN §5.3).
    stats_order: VecDeque<ContainerId>,
    max_stats_streams: usize,
    exec: Option<ExecTask>,
}

impl EngineTask {
    async fn run(&mut self, mut commands: UnboundedReceiver<Command>) {
        while let Some(cmd) = commands.recv().await {
            match cmd {
                Command::Connect(id) => self.connect(id).await,
                Command::RefreshContainers => self.refresh().await,
                Command::RefreshDiskUsage => self.refresh_disk_usage(),
                Command::Lifecycle { container, action } => self.lifecycle(container, action),
                Command::BulkLifecycle { containers, action } => {
                    self.bulk_lifecycle(containers, action)
                }
                Command::Inspect(id) => self.inspect(id),
                Command::OpenLogs { container, tail } => self.open_logs(container, tail),
                Command::CloseLogs => self.logs = None,
                Command::OpenStats(id) => self.open_stats(id),
                Command::CloseStats(id) => self.close_stats(id),
                Command::SetMaxStatsStreams(n) => self.set_max_stats_streams(n),
                Command::SetStatsRetentionHours(h) => {
                    self.retention_ms
                        .store((h as u64).max(1) * 3600 * 1000, Ordering::Relaxed);
                }
                Command::LoadStatHistory(id) => self.load_stat_history(id),
                Command::LoadExecAudit => self.load_exec_audit(),
                Command::OpenExec(id) => self.open_exec(id),
                Command::OpenAttach(id) => self.open_attach(id),
                Command::ExecInput(bytes) => {
                    if let Some(exec) = &self.exec {
                        let _ = exec.input.send(ExecMsg::Data(bytes));
                    }
                }
                Command::ExecResize { cols, rows } => {
                    if let Some(exec) = &self.exec {
                        let _ = exec.input.send(ExecMsg::Resize { cols, rows });
                    }
                }
                Command::CloseExec => self.exec = None,
                Command::Shutdown => break,
            }
        }
    }

    async fn connect(&mut self, id: ConnectionId) {
        // Dropping the streams stops anything pointed at the old connection.
        self.logs = None;
        self.stats.clear();
        self.stats_order.clear();
        self.exec = None;
        self.connection = Some(id.clone());

        match LocalDocker::connect() {
            Ok(docker) => match docker.version().await {
                Ok(version) => {
                    self.docker = Some(docker);
                    self.emitter.emit(Event::Connected {
                        connection: id,
                        version,
                    });
                    self.refresh().await;
                }
                Err(e) => self.emitter.emit(Event::Disconnected {
                    connection: id,
                    reason: e.to_string(),
                }),
            },
            Err(e) => self.emitter.emit(Event::Disconnected {
                connection: id,
                reason: e.to_string(),
            }),
        }
    }

    async fn refresh(&mut self) {
        let Some(docker) = &self.docker else { return };
        match docker.list_containers().await {
            Ok(list) => self.emitter.emit(Event::Containers(list)),
            Err(e) => self.emitter.emit(Event::Error(e.to_string())),
        }
    }

    /// Query `/system/df` off the command loop and report the total back as a
    /// [`Event::DiskUsage`]. A failure is downgraded to `DiskUsage(None)` — the
    /// tray just shows "—", it is not worth an error banner.
    fn refresh_disk_usage(&self) {
        let Some(docker) = self.docker.clone() else {
            return;
        };
        let em = self.emitter.clone();
        tokio::spawn(async move {
            match docker.disk_usage().await {
                Ok(bytes) => em.emit(Event::DiskUsage(bytes)),
                Err(e) => {
                    tracing::debug!(error = %e, "disk usage query failed");
                    em.emit(Event::DiskUsage(None));
                }
            }
        });
    }

    fn lifecycle(&self, container: ContainerId, action: crate::protocol::LifecycleAction) {
        let Some(docker) = self.docker.clone() else {
            self.emitter.emit(Event::Error("not connected".into()));
            return;
        };
        let em = self.emitter.clone();
        tokio::spawn(async move {
            match docker.lifecycle(&container, action).await {
                Ok(()) => {
                    em.emit(Event::LifecycleDone {
                        container: container.clone(),
                        action,
                    });
                    match docker.list_containers().await {
                        Ok(list) => em.emit(Event::Containers(list)),
                        Err(e) => em.emit(Event::Error(e.to_string())),
                    }
                }
                Err(e) => em.emit(Event::Error(e.to_string())),
            }
        });
    }

    /// Run `action` against every id in `containers`, concurrently and
    /// independently — one container erroring doesn't stop the others — then
    /// refresh the list once the whole batch has landed.
    fn bulk_lifecycle(
        &self,
        containers: Vec<ContainerId>,
        action: crate::protocol::LifecycleAction,
    ) {
        let Some(docker) = self.docker.clone() else {
            self.emitter.emit(Event::Error("not connected".into()));
            return;
        };
        let em = self.emitter.clone();
        tokio::spawn(async move {
            let calls = containers.into_iter().map(|container| {
                let docker = docker.clone();
                let em = em.clone();
                async move {
                    match docker.lifecycle(&container, action).await {
                        Ok(()) => em.emit(Event::LifecycleDone { container, action }),
                        Err(e) => em.emit(Event::Error(format!(
                            "{} {}: {e}",
                            action.verb(),
                            container.short()
                        ))),
                    }
                }
            });
            futures_util::future::join_all(calls).await;
            match docker.list_containers().await {
                Ok(list) => em.emit(Event::Containers(list)),
                Err(e) => em.emit(Event::Error(e.to_string())),
            }
        });
    }

    fn inspect(&self, id: ContainerId) {
        let Some(docker) = self.docker.clone() else {
            self.emitter.emit(Event::Error("not connected".into()));
            return;
        };
        let em = self.emitter.clone();
        tokio::spawn(async move {
            match docker.inspect(&id).await {
                Ok(detail) => em.emit(Event::Inspected(Box::new(detail))),
                Err(e) => em.emit(Event::Error(e.to_string())),
            }
        });
    }

    fn open_logs(&mut self, id: ContainerId, tail: LogTail) {
        self.logs = None;
        let Some(docker) = self.docker.clone() else {
            self.emitter.emit(Event::LogsClosed {
                reason: Some("not connected".into()),
            });
            return;
        };
        let em = self.emitter.clone();
        let handle = tokio::spawn(async move { run_logs(docker.raw(), id, tail, em).await });
        self.logs = Some(StreamTask(handle));
    }

    /// Ensure a stream is running for `id`, incrementing its opener count if
    /// one already is. Idempotent, so the list view and a detail screen can
    /// both ask for the same container without duplicating the stream.
    fn open_stats(&mut self, id: ContainerId) {
        if let Some((_, refs)) = self.stats.get_mut(&id) {
            *refs += 1;
            return;
        }
        let Some(docker) = self.docker.clone() else {
            self.emitter.emit(Event::StatsClosed {
                container: id,
                reason: Some("not connected".into()),
            });
            return;
        };
        while self.stats.len() >= self.max_stats_streams {
            if !self.evict_oldest_stream("stats-stream cap reached") {
                break;
            }
        }
        let em = self.emitter.clone();
        let cid = id.clone();
        let history = self.history.clone();
        let handle = tokio::spawn(async move { run_stats(docker.raw(), cid, em, history).await });
        self.stats.insert(id.clone(), (StreamTask(handle), 1));
        self.stats_order.push_back(id);
    }

    /// Read `id`'s persisted usage history over the current retention window
    /// (down-sampled) and answer with [`Event::StatHistory`]. Empty answer if
    /// no store opened.
    fn load_stat_history(&self, id: ContainerId) {
        let Some(history) = self.history.clone() else {
            self.emitter.emit(Event::StatHistory {
                container: id,
                samples: Vec::new(),
            });
            return;
        };
        let since = now_ms().saturating_sub(self.retention_ms.load(Ordering::Relaxed));
        let em = self.emitter.clone();
        tokio::spawn(async move {
            let cid = id.0.clone();
            let samples = tokio::task::spawn_blocking(move || {
                history.samples_since(&cid, since, 3000).unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            em.emit(Event::StatHistory {
                container: id,
                samples,
            });
        });
    }

    /// Read the most recent exec-audit rows and answer with
    /// [`Event::ExecAuditLog`].
    fn load_exec_audit(&self) {
        let Some(history) = self.history.clone() else {
            self.emitter.emit(Event::ExecAuditLog(Vec::new()));
            return;
        };
        let em = self.emitter.clone();
        tokio::spawn(async move {
            let rows =
                tokio::task::spawn_blocking(move || history.recent_exec(200).unwrap_or_default())
                    .await
                    .unwrap_or_default();
            em.emit(Event::ExecAuditLog(rows));
        });
    }

    /// Drop one opener's claim on `id`'s stream, closing it once nobody else
    /// still wants it.
    fn close_stats(&mut self, id: ContainerId) {
        if let Some((_, refs)) = self.stats.get_mut(&id) {
            if *refs <= 1 {
                self.stats.remove(&id);
            } else {
                *refs -= 1;
            }
        }
    }

    fn set_max_stats_streams(&mut self, n: usize) {
        self.max_stats_streams = n.max(1);
        while self.stats.len() > self.max_stats_streams {
            if !self.evict_oldest_stream("stats-stream cap lowered") {
                break;
            }
        }
    }

    /// Close the least-recently-opened stream regardless of its opener count,
    /// emitting the `StatsClosed` its subscribers would see from a real
    /// disconnect. Returns `false` once there is nothing left to evict.
    fn evict_oldest_stream(&mut self, reason: &str) -> bool {
        while let Some(oldest) = self.stats_order.pop_front() {
            if self.stats.remove(&oldest).is_some() {
                self.emitter.emit(Event::StatsClosed {
                    container: oldest,
                    reason: Some(reason.to_string()),
                });
                return true;
            }
            // Stale order entry for an id `close_stats` already removed.
        }
        false
    }

    fn audit_ctx(&self) -> AuditCtx {
        AuditCtx {
            history: self.history.clone(),
            connection_id: self
                .connection
                .as_ref()
                .map(|c| c.0.clone())
                .unwrap_or_else(|| "local".into()),
        }
    }

    fn open_exec(&mut self, id: ContainerId) {
        self.exec = None;
        let Some(docker) = self.docker.clone() else {
            self.emitter.emit(Event::ExecClosed {
                reason: Some("not connected".into()),
            });
            return;
        };
        let em = self.emitter.clone();
        let (tx, rx) = unbounded_channel::<ExecMsg>();
        let audit = self.audit_ctx();
        let handle = tokio::spawn(async move { run_exec(docker.raw(), id, em, rx, audit).await });
        self.exec = Some(ExecTask {
            task: handle,
            input: tx,
        });
    }

    fn open_attach(&mut self, id: ContainerId) {
        self.exec = None;
        let Some(docker) = self.docker.clone() else {
            self.emitter.emit(Event::ExecClosed {
                reason: Some("not connected".into()),
            });
            return;
        };
        let em = self.emitter.clone();
        let (tx, rx) = unbounded_channel::<ExecMsg>();
        let audit = self.audit_ctx();
        let handle = tokio::spawn(async move { run_attach(docker.raw(), id, em, rx, audit).await });
        self.exec = Some(ExecTask {
            task: handle,
            input: tx,
        });
    }
}

/// What an interactive session needs to write an audit row when it ends.
struct AuditCtx {
    history: Option<Arc<HistoryStore>>,
    connection_id: String,
}

/// Record one finished-session audit row (best effort). `exit_code` comes from
/// the caller (an `inspect_exec` for a shell; `None` for an attach).
async fn write_audit(
    docker: &bollard::Docker,
    audit: AuditCtx,
    id: &ContainerId,
    started_ms: u64,
    argv: Vec<String>,
    exit_code: Option<i64>,
) {
    let Some(history) = audit.history else { return };
    let container_name = docker
        .inspect_container(
            &id.0,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .ok()
        .and_then(|r| r.name)
        .map(|n| n.trim_start_matches('/').to_string())
        .unwrap_or_else(|| id.short().to_string());
    let row = ExecAudit {
        ts_ms: started_ms,
        connection_id: audit.connection_id,
        container: id.0.clone(),
        container_name,
        argv,
        exit_code,
        duration_secs: now_ms().checked_sub(started_ms).map(|ms| ms / 1000),
    };
    let _ = tokio::task::spawn_blocking(move || history.record_exec(&row)).await;
}

async fn run_logs(docker: bollard::Docker, id: ContainerId, tail: LogTail, em: Emitter) {
    use bollard::query_parameters::LogsOptionsBuilder;

    // Timestamps are always requested; the UI decides whether to show them, so
    // toggling that never restarts the stream.
    let opts = LogsOptionsBuilder::new()
        .follow(true)
        .stdout(true)
        .stderr(true)
        .timestamps(true)
        .tail(&tail.as_param())
        .build();
    let mut stream = docker.logs(&id.0, Some(opts));

    // Coalesce lines so a chatty container is a few repaints a second, not
    // hundreds. Partial lines are held per stream until their newline arrives.
    let mut pending: Vec<LogLine> = Vec::new();
    let mut out_buf: Vec<u8> = Vec::new();
    let mut err_buf: Vec<u8> = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_millis(90));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            item = stream.next() => match item {
                Some(Ok(chunk)) => {
                    let (buf, stream_kind) = match &chunk {
                        bollard::container::LogOutput::StdErr { .. } => (&mut err_buf, LogStream::Stderr),
                        _ => (&mut out_buf, LogStream::Stdout),
                    };
                    buf.extend_from_slice(chunk.as_ref());
                    drain_lines(buf, stream_kind, &mut pending);
                    if pending.len() >= 400 {
                        flush_logs(&id, &mut pending, &em);
                    }
                }
                Some(Err(e)) => {
                    flush_logs(&id, &mut pending, &em);
                    em.emit(Event::LogsClosed { reason: Some(e.to_string()) });
                    return;
                }
                None => {
                    flush_tail(&mut out_buf, LogStream::Stdout, &mut pending);
                    flush_tail(&mut err_buf, LogStream::Stderr, &mut pending);
                    flush_logs(&id, &mut pending, &em);
                    em.emit(Event::LogsClosed { reason: None });
                    return;
                }
            },
            _ = tick.tick() => flush_logs(&id, &mut pending, &em),
        }
    }
}

fn drain_lines(buf: &mut Vec<u8>, stream: LogStream, out: &mut Vec<LogLine>) {
    while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
        let mut line: Vec<u8> = buf.drain(..=nl).collect();
        line.pop(); // '\n'
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        out.push(make_line(stream, &line));
    }
    // Guard against a single pathological line growing without bound.
    if buf.len() > 64 * 1024 {
        let line = std::mem::take(buf);
        out.push(make_line(stream, &line));
    }
}

fn flush_tail(buf: &mut Vec<u8>, stream: LogStream, out: &mut Vec<LogLine>) {
    if !buf.is_empty() {
        let line = std::mem::take(buf);
        out.push(make_line(stream, &line));
    }
}

/// Split Docker's leading RFC 3339 timestamp off a raw log line. With
/// `timestamps=true` every real line looks like
/// `2026-09-10T14:03:11.482331Z the message`; a line without that shape (a
/// client-synthesised note, or an image that writes its own odd prefix) keeps
/// `ts = None` and its full text.
fn make_line(stream: LogStream, raw: &[u8]) -> LogLine {
    let text = String::from_utf8_lossy(raw).into_owned();
    match text.split_once(' ') {
        Some((head, rest))
            if head.len() >= 20
                && head.as_bytes().get(10) == Some(&b'T')
                && head.as_bytes().get(4) == Some(&b'-') =>
        {
            LogLine {
                stream,
                ts: Some(head.to_string()),
                text: rest.to_string(),
            }
        }
        _ => LogLine {
            stream,
            ts: None,
            text,
        },
    }
}

fn flush_logs(id: &ContainerId, pending: &mut Vec<LogLine>, em: &Emitter) {
    if pending.is_empty() {
        return;
    }
    em.emit(Event::LogLines {
        container: id.clone(),
        lines: std::mem::take(pending),
    });
}

/// Unix milliseconds now, saturating to `0` if the clock is before the epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn run_stats(
    docker: bollard::Docker,
    id: ContainerId,
    em: Emitter,
    history: Option<Arc<HistoryStore>>,
) {
    use bollard::query_parameters::StatsOptionsBuilder;

    let opts = StatsOptionsBuilder::new().stream(true).build();
    let mut stream = docker.stats(&id.0, Some(opts));

    while let Some(item) = stream.next().await {
        match item {
            Ok(raw) => {
                let mut sample = service::reduce_stats(&raw);
                sample.ts_ms = now_ms();
                if let Some(h) = &history {
                    // `Durability::None` write: a quick in-memory btree insert,
                    // no fsync, so this stays off the critical path.
                    let _ = h.record_sample(&id.0, &sample);
                }
                em.emit(Event::Stat {
                    container: id.clone(),
                    sample,
                });
            }
            Err(e) => {
                em.emit(Event::StatsClosed {
                    container: id,
                    reason: Some(e.to_string()),
                });
                return;
            }
        }
    }
    em.emit(Event::StatsClosed {
        container: id,
        reason: None,
    });
}

async fn run_exec(
    docker: bollard::Docker,
    id: ContainerId,
    em: Emitter,
    mut input: UnboundedReceiver<ExecMsg>,
    audit: AuditCtx,
) {
    use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};

    // Prefer bash, fall back to sh — the vast majority of images have one.
    let argv: Vec<String> = vec![
        "/bin/sh".into(),
        "-c".into(),
        "[ -x /bin/bash ] && exec /bin/bash || exec /bin/sh".into(),
    ];
    let config: CreateExecOptions<String> = CreateExecOptions {
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        tty: Some(true),
        cmd: Some(argv.clone()),
        env: Some(vec!["TERM=xterm-256color".into()]),
        ..Default::default()
    };

    let exec_id = match docker.create_exec(&id.0, config).await {
        Ok(r) => r.id,
        Err(e) => {
            em.emit(Event::ExecClosed {
                reason: Some(e.to_string()),
            });
            return;
        }
    };

    let started = docker
        .start_exec(
            &exec_id,
            Some(StartExecOptions {
                detach: false,
                tty: true,
                output_capacity: Some(16 * 1024),
            }),
        )
        .await;

    let (mut output, mut writer) = match started {
        Ok(StartExecResults::Attached { output, input }) => (output, input),
        Ok(StartExecResults::Detached) => {
            em.emit(Event::ExecClosed {
                reason: Some("exec started detached".into()),
            });
            return;
        }
        Err(e) => {
            em.emit(Event::ExecClosed {
                reason: Some(e.to_string()),
            });
            return;
        }
    };

    em.emit(Event::ExecReady {
        container: id.clone(),
    });
    let started_ms = now_ms();

    let end_reason: Option<String> = loop {
        tokio::select! {
            msg = input.recv() => match msg {
                Some(ExecMsg::Data(bytes)) => {
                    if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
                        break Some("stdin closed".into());
                    }
                }
                Some(ExecMsg::Resize { cols, rows }) => {
                    use bollard::query_parameters::ResizeExecOptionsBuilder;
                    let _ = docker
                        .resize_exec(
                            &exec_id,
                            ResizeExecOptionsBuilder::new()
                                .w(cols as i32)
                                .h(rows as i32)
                                .build(),
                        )
                        .await;
                }
                None => break None,
            },
            out = output.next() => match out {
                Some(Ok(chunk)) => em.emit(Event::ExecOutput(chunk.into_bytes().to_vec())),
                Some(Err(e)) => break Some(e.to_string()),
                None => break None,
            },
        }
    };

    // Audit the finished session: exit code from a follow-up `inspect_exec`.
    let exit_code = docker
        .inspect_exec(&exec_id)
        .await
        .ok()
        .and_then(|r| r.exit_code);
    write_audit(&docker, audit, &id, started_ms, argv, exit_code).await;

    em.emit(Event::ExecClosed { reason: end_reason });
}

/// Attach to a container's main-process stdio (PLAN §5.3). No exec wrapper —
/// input, Ctrl-C and TTY resize go straight to the entrypoint. Otherwise the
/// same byte-stream loop and audit as [`run_exec`].
async fn run_attach(
    docker: bollard::Docker,
    id: ContainerId,
    em: Emitter,
    mut input: UnboundedReceiver<ExecMsg>,
    audit: AuditCtx,
) {
    use bollard::query_parameters::{
        AttachContainerOptionsBuilder, ResizeContainerTTYOptionsBuilder,
    };

    let opts = AttachContainerOptionsBuilder::new()
        .stream(true)
        .stdin(true)
        .stdout(true)
        .stderr(true)
        .logs(true)
        .build();

    let (mut output, mut writer) = match docker.attach_container(&id.0, Some(opts)).await {
        Ok(r) => (r.output, r.input),
        Err(e) => {
            em.emit(Event::ExecClosed {
                reason: Some(e.to_string()),
            });
            return;
        }
    };

    em.emit(Event::ExecReady {
        container: id.clone(),
    });
    let started_ms = now_ms();

    let end_reason: Option<String> = loop {
        tokio::select! {
            msg = input.recv() => match msg {
                Some(ExecMsg::Data(bytes)) => {
                    if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
                        break Some("stdin closed".into());
                    }
                }
                Some(ExecMsg::Resize { cols, rows }) => {
                    let _ = docker
                        .resize_container_tty(
                            &id.0,
                            ResizeContainerTTYOptionsBuilder::new()
                                .w(cols as i32)
                                .h(rows as i32)
                                .build(),
                        )
                        .await;
                }
                None => break None,
            },
            out = output.next() => match out {
                Some(Ok(chunk)) => em.emit(Event::ExecOutput(chunk.into_bytes().to_vec())),
                Some(Err(e)) => break Some(e.to_string()),
                None => break None,
            },
        }
    };

    write_audit(
        &docker,
        audit,
        &id,
        started_ms,
        vec!["<attach>".into()],
        None,
    )
    .await;
    em.emit(Event::ExecClosed { reason: end_reason });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_line_splits_docker_timestamp() {
        let l = make_line(
            LogStream::Stdout,
            b"2026-09-10T14:03:11.482331Z listening on :80",
        );
        assert_eq!(l.ts.as_deref(), Some("2026-09-10T14:03:11.482331Z"));
        assert_eq!(l.text, "listening on :80");
    }

    #[test]
    fn make_line_keeps_untimestamped_text_whole() {
        let l = make_line(LogStream::Stderr, b"panic: runtime error");
        assert_eq!(l.ts, None);
        assert_eq!(l.text, "panic: runtime error");
        // A leading token that only looks vaguely date-ish is not mistaken for one.
        let l = make_line(LogStream::Stdout, b"12:34:56 not-a-date message");
        assert_eq!(l.ts, None);
    }

    #[test]
    fn log_tail_param() {
        assert_eq!(LogTail::Lines(100).as_param(), "100");
        assert_eq!(LogTail::All.as_param(), "all");
        assert_eq!(LogTail::default(), LogTail::Lines(400));
    }
}
