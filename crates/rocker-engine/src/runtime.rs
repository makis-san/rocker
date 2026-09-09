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
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use rocker_core::{ConnectionId, ContainerId};

use crate::protocol::{Command, Event, LogLine, LogStream};
use crate::service::{self, DockerService, LocalDocker};

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
/// `egui::Context::request_repaint`.
pub fn start(
    rt: &tokio::runtime::Handle,
    repaint: impl Fn() + Send + Sync + 'static,
) -> EngineHandle {
    let (cmd_tx, cmd_rx) = unbounded_channel::<Command>();
    let (evt_tx, evt_rx) = unbounded_channel::<Event>();

    let emitter = Emitter {
        tx: evt_tx,
        repaint: Arc::new(repaint),
    };

    rt.spawn(async move {
        let mut task = EngineTask {
            docker: None,
            emitter,
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
    emitter: Emitter,
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
                Command::Lifecycle { container, action } => self.lifecycle(container, action),
                Command::BulkLifecycle { containers, action } => {
                    self.bulk_lifecycle(containers, action)
                }
                Command::Inspect(id) => self.inspect(id),
                Command::OpenLogs(id) => self.open_logs(id),
                Command::CloseLogs => self.logs = None,
                Command::OpenStats(id) => self.open_stats(id),
                Command::CloseStats(id) => self.close_stats(id),
                Command::SetMaxStatsStreams(n) => self.set_max_stats_streams(n),
                Command::OpenExec(id) => self.open_exec(id),
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

    fn open_logs(&mut self, id: ContainerId) {
        self.logs = None;
        let Some(docker) = self.docker.clone() else {
            self.emitter.emit(Event::LogsClosed {
                reason: Some("not connected".into()),
            });
            return;
        };
        let em = self.emitter.clone();
        let handle = tokio::spawn(async move { run_logs(docker.raw(), id, em).await });
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
        let handle = tokio::spawn(async move { run_stats(docker.raw(), cid, em).await });
        self.stats.insert(id.clone(), (StreamTask(handle), 1));
        self.stats_order.push_back(id);
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
        let handle = tokio::spawn(async move { run_exec(docker.raw(), id, em, rx).await });
        self.exec = Some(ExecTask {
            task: handle,
            input: tx,
        });
    }
}

async fn run_logs(docker: bollard::Docker, id: ContainerId, em: Emitter) {
    use bollard::query_parameters::LogsOptionsBuilder;

    let opts = LogsOptionsBuilder::new()
        .follow(true)
        .stdout(true)
        .stderr(true)
        .tail("400")
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
        out.push(LogLine {
            stream,
            text: String::from_utf8_lossy(&line).into_owned(),
        });
    }
    // Guard against a single pathological line growing without bound.
    if buf.len() > 64 * 1024 {
        let line = std::mem::take(buf);
        out.push(LogLine {
            stream,
            text: String::from_utf8_lossy(&line).into_owned(),
        });
    }
}

fn flush_tail(buf: &mut Vec<u8>, stream: LogStream, out: &mut Vec<LogLine>) {
    if !buf.is_empty() {
        let line = std::mem::take(buf);
        out.push(LogLine {
            stream,
            text: String::from_utf8_lossy(&line).into_owned(),
        });
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

async fn run_stats(docker: bollard::Docker, id: ContainerId, em: Emitter) {
    use bollard::query_parameters::StatsOptionsBuilder;

    let opts = StatsOptionsBuilder::new().stream(true).build();
    let mut stream = docker.stats(&id.0, Some(opts));

    while let Some(item) = stream.next().await {
        match item {
            Ok(raw) => em.emit(Event::Stat {
                container: id.clone(),
                sample: service::reduce_stats(&raw),
            }),
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
) {
    use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};

    // Prefer bash, fall back to sh — the vast majority of images have one.
    let config: CreateExecOptions<String> = CreateExecOptions {
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        tty: Some(true),
        cmd: Some(vec![
            "/bin/sh".into(),
            "-c".into(),
            "[ -x /bin/bash ] && exec /bin/bash || exec /bin/sh".into(),
        ]),
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

    loop {
        tokio::select! {
            msg = input.recv() => match msg {
                Some(ExecMsg::Data(bytes)) => {
                    if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
                        em.emit(Event::ExecClosed { reason: Some("stdin closed".into()) });
                        return;
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
                None => {
                    em.emit(Event::ExecClosed { reason: None });
                    return;
                }
            },
            out = output.next() => match out {
                Some(Ok(chunk)) => em.emit(Event::ExecOutput(chunk.into_bytes().to_vec())),
                Some(Err(e)) => {
                    em.emit(Event::ExecClosed { reason: Some(e.to_string()) });
                    return;
                }
                None => {
                    em.emit(Event::ExecClosed { reason: None });
                    return;
                }
            },
        }
    }
}
