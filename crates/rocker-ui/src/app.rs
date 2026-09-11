use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rocker_core::{Connection, Container, ContainerId, ContainerState, StatSample};
use rocker_engine::{start, Command, EngineHandle, Event, LifecycleAction};
use rocker_store::{AppPaths, Config};
use rocker_theme::Theme;

use crate::detail::DetailScreen;
use crate::extensions::{self, ExtensionsScreen};
use crate::groups;
use crate::icons::{self, Icon};
use crate::registries::{self, RegistriesScreen};
use crate::settings::{self, About};
use crate::style::{self, Palette};
use crate::tray::{Tray, TrayAction, TraySummary};
use crate::widgets::{
    confirm_dialog, container_row, group_header, GroupOutcome, GroupUsage, RowOutcome, LIST_ROW_H,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConnStatus {
    Connecting,
    Connected { version: String },
    Failed { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Containers,
    Groups,
    Extensions,
    Registries,
    Settings,
}

/// A click in the header, applied after the panel closure returns so the header
/// can stay `&self`.
enum HeaderAction {
    Refresh,
    ToggleGroups,
    ToggleExtensions,
    ToggleRegistries,
    ToggleSettings,
}

/// What a click in the container list asked for.
enum ListHit {
    Open(Container),
    Act(Container, LifecycleAction),
    /// A group header's bulk action, already narrowed to the containers it
    /// actually applies to (e.g. "start all" only names the stopped ones).
    BulkAct(Vec<ContainerId>, LifecycleAction),
}

/// A bulk delete waiting on the confirm dialog before it is sent.
struct PendingDelete {
    title: String,
    detail: String,
    containers: Vec<ContainerId>,
}

pub struct RockerApp {
    engine: EngineHandle,
    paths: AppPaths,
    config: Config,
    pal: Palette,
    view: View,
    status: ConnStatus,
    containers: Vec<Container>,
    /// Latest resource sample per container, kept independent of whichever
    /// (if any) is open in the detail screen so a group header's combined
    /// total stays live even with nothing expanded.
    stats: HashMap<ContainerId, StatSample>,
    /// Container ids the list view currently has an open stats stream for —
    /// every active container, subject to the engine's stream cap. Diffed
    /// against on each container-list refresh rather than resent every
    /// frame.
    stats_subscribed: HashSet<ContainerId>,
    last_error: Option<String>,
    /// A transient, non-error confirmation (e.g. "logs exported to …"), shown
    /// in a quiet accent banner and dismissable like the error one.
    last_notice: Option<String>,
    /// The open container screen, if any. Lives alongside `view` rather than
    /// inside it so the list's scroll position and group state survive a trip
    /// into a container and back.
    detail: Option<DetailScreen>,
    /// A group's "delete all" waiting on confirmation before it is sent —
    /// destructive and irreversible, so it never fires straight off the click.
    pending_delete: Option<PendingDelete>,
    /// The local extension registry and its last discovery pass. Lives
    /// alongside `view` rather than inside it, same as `detail`, so a
    /// discovery re-scan survives navigating away and back.
    extensions: ExtensionsScreen,
    /// The Registries screen's form state, in-flight test-connection probes,
    /// and the keychain handle they run against (PLAN §5.2). Lives alongside
    /// `view` like `extensions`, so a probe survives navigating away and back.
    registries: RegistriesScreen,
    /// Docker's total on-disk usage (bytes) from `/system/df`, polled on a
    /// timer for the tray summary. `None` until the first answer, or when the
    /// daemon doesn't report it.
    disk_usage: Option<u64>,
    /// UI-clock time of the last `/system/df` query, so it runs on a cadence
    /// rather than every frame.
    last_disk_poll: f64,
    /// The system-tray icon, when a setting calls for it and the platform let
    /// us create one.
    tray: Option<Tray>,
    /// Stop flag for the tray heartbeat thread, set when the tray is torn down.
    tray_stop: Option<Arc<AtomicBool>>,
    /// Shared with the heartbeat thread: `true` while the window is hidden to
    /// the tray, so it repaints often enough to keep tray clicks responsive.
    hidden: Arc<AtomicBool>,
    /// A real quit is underway (the tray's "Quit"), so a close request is let
    /// through instead of being turned into a hide-to-tray.
    quitting: bool,
}

/// Resolve a stored theme id (`system` / `light` / `dark`, or a future file
/// stem) into a concrete [`Theme`]. `system` follows the OS light/dark setting,
/// falling back to dark when the platform doesn't report one.
fn resolve_theme(ctx: &egui::Context, id: &str) -> Theme {
    match id {
        "light" => Theme::light(),
        "dark" => Theme::dark(),
        _ => match ctx.input(|i| i.raw.system_theme) {
            Some(egui::Theme::Light) => Theme::light(),
            _ => Theme::dark(),
        },
    }
}

/// Parse a `#rrggbb` string into a colour, tolerating a missing `#`.
fn hex_color(s: &str) -> Option<egui::Color32> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(egui::Color32::from_rgb(r, g, b))
}

/// Whether a group-level bulk action is meaningful for a container currently
/// in `state` — e.g. "start all" only ever targets the ones not already
/// running, so a bulk press never spends a call on a no-op.
fn bulk_action_applies(state: ContainerState, action: LifecycleAction) -> bool {
    match action {
        LifecycleAction::Start => !state.is_active(),
        LifecycleAction::Stop => state.is_active(),
        // Delete (and anything else added later) applies regardless of state.
        _ => true,
    }
}

impl RockerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Handle) -> Self {
        let paths = AppPaths::resolve();
        let config = Config::load(&paths).unwrap_or_else(|err| {
            tracing::warn!(%err, "config load failed; starting from defaults");
            Config::default()
        });

        let theme = resolve_theme(&cc.egui_ctx, &config.settings.theme);
        let pal = style::install(&cc.egui_ctx, &theme);

        // Open the history store; if it can't open, the app still runs — stats
        // just don't persist and the audit log stays empty.
        let history = match rocker_store::HistoryStore::open(&paths.history_db()) {
            Ok(h) => Some(std::sync::Arc::new(h)),
            Err(err) => {
                tracing::warn!(%err, "history store unavailable; not persisting usage/audit");
                None
            }
        };

        let ctx = cc.egui_ctx.clone();
        let engine = start(&rt, move || ctx.request_repaint(), history);
        engine.send(Command::Connect(Connection::local_default().id));
        engine.send(Command::SetMaxStatsStreams(
            config.settings.max_stats_streams,
        ));
        engine.send(Command::SetStatsRetentionHours(
            config.settings.stats_retention_hours,
        ));

        // The tray only exists when a setting actually calls for it: either
        // "minimize to tray" (close/minimize hides instead of quitting) or
        // "start hidden" (which needs somewhere to be). With both off there is
        // no tray icon at all and the window close button just quits.
        let want_tray = config.settings.minimize_to_tray || config.settings.start_minimized;
        let hidden = Arc::new(AtomicBool::new(false));
        let extensions = ExtensionsScreen::new(&paths);
        let registries = RegistriesScreen::new(Arc::new(rocker_secrets::KeyringSecretStore::new()));

        let mut app = Self {
            engine,
            paths,
            config,
            pal,
            view: View::Containers,
            status: ConnStatus::Connecting,
            containers: Vec::new(),
            stats: HashMap::new(),
            stats_subscribed: HashSet::new(),
            last_error: None,
            last_notice: None,
            detail: None,
            pending_delete: None,
            extensions,
            registries,
            disk_usage: None,
            last_disk_poll: f64::NEG_INFINITY,
            tray: None,
            tray_stop: None,
            hidden,
            quitting: false,
        };

        if want_tray {
            app.set_tray_enabled(true, &cc.egui_ctx);
        }

        if app.config.settings.start_minimized {
            if app.tray.is_some() {
                // main() already built the window hidden; just record that.
                app.hidden.store(true, Ordering::Relaxed);
            } else {
                // Asked to start hidden, but there's no tray to hide in — show
                // the window so Rocker isn't left invisible and unreachable.
                cc.egui_ctx
                    .send_viewport_cmd(egui::ViewportCommand::Visible(true));
            }
        }

        app
    }

    /// Create or tear down the system tray (and its heartbeat thread) so it
    /// matches `wanted`. Called at startup and whenever the "minimize to tray"
    /// setting is toggled. Tearing it down removes the icon from the panel.
    fn set_tray_enabled(&mut self, wanted: bool, ctx: &egui::Context) {
        if wanted == self.tray.is_some() {
            return;
        }

        if wanted {
            self.tray = Tray::new();
            if self.tray.is_none() {
                return;
            }
            // A hidden window gets no redraws from the OS, so nothing would
            // drain the tray's click channel. This wakes the frame loop on a
            // slow tick — quick while hidden, lazy while the window is up — and
            // stops itself when the tray goes away.
            let stop = Arc::new(AtomicBool::new(false));
            self.tray_stop = Some(stop.clone());
            let ctx = ctx.clone();
            let hidden = self.hidden.clone();
            std::thread::Builder::new()
                .name("rocker-tray-heartbeat".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let nap = if hidden.load(Ordering::Relaxed) {
                            std::time::Duration::from_millis(500)
                        } else {
                            std::time::Duration::from_secs(2)
                        };
                        std::thread::sleep(nap);
                        ctx.request_repaint();
                    }
                })
                .ok();
        } else {
            self.tray = None;
            if let Some(stop) = self.tray_stop.take() {
                stop.store(true, Ordering::Relaxed);
            }
            // Nothing to hide into anymore; make sure the window is on screen.
            if self.hidden.swap(false, Ordering::Relaxed) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            }
        }
    }

    /// Open the settings view. Used by the `screenshot` example; the running app
    /// toggles the view from the header instead.
    pub fn open_settings(&mut self) {
        self.view = View::Settings;
    }

    /// Open the registries view. Used by the `screenshot` example; the running
    /// app toggles the view from the header instead.
    pub fn open_registries(&mut self) {
        self.view = View::Registries;
    }

    /// Debug/test hook: seed a registry entry (no keychain write) so the
    /// `screenshot` example can capture a populated card, not just the empty
    /// state.
    pub fn debug_seed_registry(&mut self, host: &str, username: &str, verified: bool) {
        let mut r = rocker_core::Registry::basic(host, username);
        if verified {
            r.verified_at_ms = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            );
        }
        self.config.registries.push(r);
    }

    /// Open the container screen for the first container whose name contains
    /// `needle`. Used by the `screenshot` example to drive to a specific
    /// screen headlessly; the running app opens it from a row click instead.
    /// Returns `false` if nothing matched (the container list may not have
    /// loaded yet).
    pub fn open_container(&mut self, needle: &str) -> bool {
        let Some(container) = self
            .containers
            .iter()
            .find(|c| c.name.contains(needle))
            .cloned()
        else {
            return false;
        };
        self.open_detail(&container);
        true
    }

    /// Debug/test hook: jump the open container screen to a tab by name
    /// (`"logs"`, `"stats"`, `"terminal"`; anything else lands on Overview).
    pub fn debug_set_tab(&mut self, name: &str) {
        if let Some(d) = &mut self.detail {
            d.debug_set_tab(name);
        }
    }

    /// Debug/test hook: start the terminal session in the open container
    /// screen as the "Start session" button would, without a click.
    pub fn debug_start_terminal(&mut self) {
        if let Some(d) = &mut self.detail {
            let cmd = d.debug_start_terminal();
            self.engine.send(cmd);
        }
    }

    fn drain_events(&mut self) {
        while let Some(event) = self.engine.try_recv() {
            match event {
                Event::Connected { version, .. } => {
                    self.status = ConnStatus::Connected { version };
                    self.last_error = None;
                    // Pull a fresh disk figure for the tray on the next frame.
                    self.last_disk_poll = f64::NEG_INFINITY;
                }
                Event::Disconnected { reason, .. } => {
                    self.status = ConnStatus::Failed { reason };
                    self.disk_usage = None;
                }
                Event::DiskUsage(bytes) => self.disk_usage = bytes,
                Event::Containers(mut list) => {
                    // Grouped containers first (by Compose project, then name),
                    // ungrouped last, so the list reads as sections.
                    list.sort_by(|a, b| {
                        let ka = (
                            a.compose_project.is_none(),
                            a.compose_project.clone(),
                            a.name.clone(),
                        );
                        let kb = (
                            b.compose_project.is_none(),
                            b.compose_project.clone(),
                            b.name.clone(),
                        );
                        ka.cmp(&kb)
                    });
                    self.containers = list;
                    if let Some(d) = &mut self.detail {
                        d.sync_state(&self.containers);
                    }
                    self.sync_stats_subscriptions();
                }
                Event::LifecycleDone { container, .. } => {
                    // Refresh the Overview tab once an action the sub-header
                    // triggered has actually landed.
                    if matches!(&self.detail, Some(d) if *d.id() == container) {
                        self.engine.send(Command::Inspect(container));
                    }
                }
                Event::Error(message) => self.last_error = Some(message),
                Event::Inspected(d) => {
                    if let Some(screen) = &mut self.detail {
                        if *screen.id() == d.id {
                            screen.on_inspected(*d);
                        }
                    }
                }
                Event::LogLines { container, lines } => {
                    if let Some(d) = &mut self.detail {
                        if *d.id() == container {
                            d.on_log_lines(lines);
                        }
                    }
                }
                Event::LogsClosed { reason } => {
                    if let Some(d) = &mut self.detail {
                        d.on_logs_closed(reason);
                    }
                }
                Event::Stat { container, sample } => {
                    if let Some(d) = &mut self.detail {
                        if *d.id() == container {
                            d.on_stat(sample);
                        }
                    }
                    self.stats.insert(container, sample);
                }
                Event::StatHistory { container, samples } => {
                    if let Some(d) = &mut self.detail {
                        if *d.id() == container {
                            d.on_stat_history(samples);
                        }
                    }
                }
                Event::ExecAuditLog(rows) => {
                    if let Some(d) = &mut self.detail {
                        d.on_exec_audit(rows);
                    }
                }
                Event::StatsClosed { container, reason } => {
                    if let Some(d) = &mut self.detail {
                        if *d.id() == container {
                            d.on_stats_closed(reason);
                        }
                    }
                    // Stale either way: a closed stream (deliberate, an
                    // error, or LRU eviction) has nothing current to report,
                    // and `sync_stats_subscriptions` will re-open it next
                    // list refresh if it's still wanted.
                    self.stats.remove(&container);
                    self.stats_subscribed.remove(&container);
                }
                Event::ExecReady { container } => {
                    if let Some(d) = &mut self.detail {
                        if *d.id() == container {
                            d.on_exec_ready();
                        }
                    }
                }
                Event::ExecOutput(bytes) => {
                    if let Some(d) = &mut self.detail {
                        d.on_exec_output(&bytes);
                    }
                }
                Event::ExecClosed { reason } => {
                    if let Some(d) = &mut self.detail {
                        d.on_exec_closed(reason);
                    }
                }
            }
        }
    }

    /// Keep the engine's stats streams in sync with which containers are
    /// currently active, so every group header's combined total stays live
    /// without the user needing to expand or open anything. Runs whenever the
    /// container list itself changes rather than every frame.
    fn sync_stats_subscriptions(&mut self) {
        let desired: HashSet<ContainerId> = self
            .containers
            .iter()
            .filter(|c| c.state.is_active())
            .map(|c| c.id.clone())
            .collect();

        let stale: Vec<ContainerId> = self
            .stats_subscribed
            .iter()
            .filter(|id| !desired.contains(*id))
            .cloned()
            .collect();
        for id in stale {
            self.stats_subscribed.remove(&id);
            self.stats.remove(&id);
            self.engine.send(Command::CloseStats(id));
        }

        for id in &desired {
            if self.stats_subscribed.insert(id.clone()) {
                self.engine.send(Command::OpenStats(id.clone()));
            }
        }
    }

    fn reconnect(&self) {
        self.engine
            .send(Command::Connect(Connection::local_default().id));
    }

    fn act(&self, container: &Container, action: LifecycleAction) {
        self.engine.send(Command::Lifecycle {
            container: container.id.clone(),
            action,
        });
    }

    fn bulk_act(&self, containers: Vec<ContainerId>, action: LifecycleAction) {
        self.engine
            .send(Command::BulkLifecycle { containers, action });
    }

    /// Enter the container screen: opens logs and stats streams immediately so
    /// they are warm no matter which tab the user starts on; the terminal is
    /// opened explicitly instead, since it runs a live shell.
    fn open_detail(&mut self, container: &Container) {
        let mut screen = DetailScreen::new(container);
        screen.sync_state(&self.containers);
        self.engine.send(Command::Inspect(container.id.clone()));
        self.engine.send(Command::OpenLogs {
            container: container.id.clone(),
            tail: rocker_engine::LogTail::default(),
        });
        self.engine.send(Command::OpenStats(container.id.clone()));
        self.engine
            .send(Command::LoadStatHistory(container.id.clone()));
        self.engine.send(Command::LoadExecAudit);
        self.detail = Some(screen);
        self.view = View::Containers;
    }

    /// Leave the container screen, tearing down whatever streams were open.
    fn close_detail(&mut self) {
        if let Some(screen) = self.detail.take() {
            self.engine.send(Command::CloseLogs);
            // The list view may still want this container's stats for its
            // group's combined total — the engine reference-counts opens, so
            // this only tears the stream down once nobody else holds it.
            self.engine.send(Command::CloseStats(screen.id().clone()));
            self.engine.send(Command::CloseExec);
            self.engine.send(Command::RefreshContainers);
        }
    }

    /// Write the config back to disk, surfacing any failure in the error banner.
    fn persist(&mut self) {
        if let Err(err) = self.config.save(&self.paths) {
            self.last_error = Some(format!("Couldn't save settings: {err}"));
        }
    }

    /// Rebuild the palette from the current `settings.theme` and re-install the
    /// `egui` style (Phase 4 hot-reload uses the same path).
    fn apply_theme(&mut self, ctx: &egui::Context) {
        let theme = resolve_theme(ctx, &self.config.settings.theme);
        self.pal = style::install(ctx, &theme);
    }

    /// The numbers behind the tray menu: combined CPU and memory across the
    /// running containers (the only ones with an open stats stream), Docker's
    /// disk footprint, and the running / stopped split.
    fn tray_summary(&self) -> TraySummary {
        let running = self
            .containers
            .iter()
            .filter(|c| c.state.is_active())
            .count();
        TraySummary {
            connected: matches!(self.status, ConnStatus::Connected { .. }),
            cpu_pct: self.stats.values().map(|s| s.cpu_pct).sum(),
            mem_used: self.stats.values().map(|s| s.mem_used).sum(),
            disk_bytes: self.disk_usage,
            running,
            stopped: self.containers.len().saturating_sub(running),
        }
    }

    /// Pull the window off-screen into the tray (not a real close).
    fn hide_to_tray(&mut self, ctx: &egui::Context) {
        self.hidden.store(true, Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    /// Bring the window back from the tray and focus it.
    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden.store(false, Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Turn a close or minimize into a hide-to-tray when that's what the user
    /// asked for, and keep the tray menu's numbers current. Runs every frame.
    fn service_tray(&mut self, ctx: &egui::Context) {
        if self.tray.is_none() {
            return;
        }

        if self.config.settings.minimize_to_tray && !self.quitting {
            let (close_requested, minimized) = ctx.input(|i| {
                (
                    i.viewport().close_requested(),
                    i.viewport().minimized == Some(true),
                )
            });
            if close_requested {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.hide_to_tray(ctx);
            } else if minimized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                self.hide_to_tray(ctx);
            }
        }

        let now = ctx.input(|i| i.time);
        if matches!(self.status, ConnStatus::Connected { .. }) && now - self.last_disk_poll >= 15.0
        {
            self.engine.send(Command::RefreshDiskUsage);
            self.last_disk_poll = now;
        }

        let summary = self.tray_summary();
        let action = {
            let tray = self.tray.as_mut().expect("tray present, checked above");
            tray.render(&summary);
            tray.poll()
        };
        match action {
            Some(TrayAction::Show) => self.show_window(ctx),
            Some(TrayAction::Quit) => {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            None => {}
        }
    }

    fn header(&self, ctx: &egui::Context) -> Option<HeaderAction> {
        let pal = &self.pal;
        let mut action = None;
        let resp = egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(pal.surface)
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (mark, _) =
                        ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
                    icons::draw(ui.painter(), Icon::Cube, mark, pal.accent);
                    ui.add_space(9.0);
                    ui.label(
                        egui::RichText::new("Rocker")
                            .size(15.0)
                            .strong()
                            .color(pal.text),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.view == View::Containers && self.detail.is_none() {
                            if icons::icon_button(ui, pal, Icon::Refresh, None, "Refresh").clicked()
                            {
                                action = Some(HeaderAction::Refresh);
                            }
                            ui.add_space(2.0);
                        }
                        let settings_open = self.view == View::Settings;
                        if icons::toggle_icon_button(
                            ui,
                            pal,
                            Icon::Sliders,
                            settings_open,
                            "Settings",
                        )
                        .clicked()
                        {
                            action = Some(HeaderAction::ToggleSettings);
                        }
                        ui.add_space(2.0);
                        let groups_open = self.view == View::Groups;
                        if icons::toggle_icon_button(ui, pal, Icon::Stack, groups_open, "Groups")
                            .clicked()
                        {
                            action = Some(HeaderAction::ToggleGroups);
                        }
                        ui.add_space(2.0);
                        let extensions_open = self.view == View::Extensions;
                        if icons::toggle_icon_button(
                            ui,
                            pal,
                            Icon::Puzzle,
                            extensions_open,
                            "Extensions",
                        )
                        .clicked()
                        {
                            action = Some(HeaderAction::ToggleExtensions);
                        }
                        ui.add_space(2.0);
                        let registries_open = self.view == View::Registries;
                        if icons::toggle_icon_button(
                            ui,
                            pal,
                            Icon::Key,
                            registries_open,
                            "Registries",
                        )
                        .clicked()
                        {
                            action = Some(HeaderAction::ToggleRegistries);
                        }
                        ui.add_space(style::SM);
                        self.conn_status(ui);
                    });
                });
            });

        // A single crisp hairline under the header — not a full box stroke that
        // would draw lines up the window edges too.
        let r = resp.response.rect;
        ctx.layer_painter(egui::LayerId::background()).hline(
            r.x_range(),
            r.max.y,
            egui::Stroke::new(1.0_f32, pal.border),
        );

        action
    }

    fn conn_status(&self, ui: &mut egui::Ui) {
        let pal = &self.pal;
        match &self.status {
            ConnStatus::Connecting => {
                ui.label(
                    egui::RichText::new("Connecting")
                        .small()
                        .color(pal.text_muted),
                );
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                let phase = ui.input(|i| i.time) as f32 * 3.2;
                let pulse = 0.5 - 0.5 * phase.cos();
                ui.painter().circle_filled(
                    rect.center(),
                    2.6 + 1.4 * pulse,
                    pal.accent.gamma_multiply(0.5 + 0.5 * pulse),
                );
                ui.ctx().request_repaint();
            }
            ConnStatus::Connected { version } => {
                ui.label(
                    egui::RichText::new(format!("Local \u{00b7} Engine {version}"))
                        .small()
                        .color(pal.text_muted),
                );
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 3.2, pal.running);
            }
            ConnStatus::Failed { .. } => {
                ui.label(egui::RichText::new("Offline").small().color(pal.unhealthy));
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                icons::draw(ui.painter(), Icon::Alert, rect, pal.unhealthy);
            }
        }
    }

    fn error_banner(&mut self, ui: &mut egui::Ui) {
        let Some(message) = self.last_error.clone() else {
            return;
        };
        let pal = self.pal;
        let mut dismiss = false;
        egui::Frame::new()
            .fill(pal.unhealthy.gamma_multiply(0.12))
            .stroke(egui::Stroke::new(
                1.0_f32,
                pal.unhealthy.gamma_multiply(0.42),
            ))
            .corner_radius(style::radius(pal.corner))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .outer_margin(egui::Margin {
                bottom: 10,
                ..egui::Margin::ZERO
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                    icons::draw(ui.painter(), Icon::Alert, rect, pal.unhealthy);
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(message).color(pal.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::icon_button(ui, &pal, Icon::Close, None, "Dismiss").clicked() {
                            dismiss = true;
                        }
                    });
                });
            });
        if dismiss {
            self.last_error = None;
        }
    }

    /// A quiet accent-tinted confirmation banner, same shape as
    /// [`Self::error_banner`] but for a non-error result the user asked for.
    fn notice_banner(&mut self, ui: &mut egui::Ui) {
        let Some(message) = self.last_notice.clone() else {
            return;
        };
        let pal = self.pal;
        let mut dismiss = false;
        egui::Frame::new()
            .fill(pal.accent.gamma_multiply(0.10))
            .stroke(egui::Stroke::new(1.0_f32, pal.accent.gamma_multiply(0.38)))
            .corner_radius(style::radius(pal.corner))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .outer_margin(egui::Margin {
                bottom: 10,
                ..egui::Margin::ZERO
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                    icons::draw(ui.painter(), Icon::Info, rect, pal.accent);
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(message).color(pal.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::icon_button(ui, &pal, Icon::Close, None, "Dismiss").clicked() {
                            dismiss = true;
                        }
                    });
                });
            });
        if dismiss {
            self.last_notice = None;
        }
    }

    fn centered_state(
        &self,
        ui: &mut egui::Ui,
        icon: Icon,
        icon_color: egui::Color32,
        title: &str,
        detail: &str,
        retry: bool,
    ) -> bool {
        let pal = &self.pal;
        let mut clicked = false;
        ui.add_space(72.0);
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
            icons::draw(ui.painter(), icon, rect, icon_color);
            ui.add_space(14.0);
            ui.label(
                egui::RichText::new(title)
                    .size(15.0)
                    .strong()
                    .color(pal.text),
            );
            ui.add_space(4.0);
            ui.label(egui::RichText::new(detail).color(pal.text_muted));
            if retry {
                ui.add_space(16.0);
                clicked = icons::primary_button(ui, pal, "Retry connection").clicked();
            }
        });
        clicked
    }

    /// Sum the latest known CPU/mem for a group's running containers. `None`
    /// when nothing running in the group has reported a sample yet (e.g. the
    /// whole group is stopped, or the streams just opened); `partial` marks a
    /// sum that's missing a running member — still warming up, or bumped out
    /// by the stats-stream cap — so it reads as a floor rather than exact.
    fn group_usage(&self, items: &[&Container]) -> Option<GroupUsage> {
        let active = items.iter().filter(|c| c.state.is_active()).count();
        if active == 0 {
            return None;
        }
        let mut cpu_pct = 0.0;
        let mut mem_used = 0u64;
        let mut counted = 0;
        for c in items.iter().filter(|c| c.state.is_active()) {
            if let Some(sample) = self.stats.get(&c.id) {
                cpu_pct += sample.cpu_pct;
                mem_used += sample.mem_used;
                counted += 1;
            }
        }
        if counted == 0 {
            return None;
        }
        Some(GroupUsage {
            cpu_pct,
            mem_used,
            partial: counted < active,
        })
    }

    /// What the container list asked for this frame.
    ///
    /// Virtualized (PLAN §8): the groups and their rows are flattened into one
    /// list of fixed-height slots and only the slots inside the viewport are
    /// laid out, so a 500-container host still renders a handful of rows per
    /// frame. A collapsed group contributes just its header slot.
    fn container_list(&self, ui: &mut egui::Ui) -> Option<ListHit> {
        use egui::collapsing_header::CollapsingState;

        let pal = &self.pal;
        let mut pending = None;

        // User groups first (PLAN §5.1), then Compose smart-grouping for
        // whatever's left, then the ungrouped remainder.
        let sections = rocker_core::resolve_groups(&self.containers, &self.config.groups);
        let ids: Vec<egui::Id> = sections
            .iter()
            .map(|s| match &s.id {
                rocker_core::SectionId::User(g) => {
                    ui.make_persistent_id(("sect-user", g.0.as_str()))
                }
                rocker_core::SectionId::Compose(p) => {
                    ui.make_persistent_id(("sect-compose", p.as_str()))
                }
                rocker_core::SectionId::Ungrouped => ui.make_persistent_id("sect-ungrouped"),
            })
            .collect();

        // Flatten to one slot per visible row: every section's header, then its
        // container rows while the section is open.
        enum Slot<'a> {
            Header(usize),
            Row(&'a Container),
        }
        let mut slots: Vec<Slot> = Vec::new();
        for (si, sec) in sections.iter().enumerate() {
            let open = CollapsingState::load_with_default_open(ui.ctx(), ids[si], true).is_open();
            slots.push(Slot::Header(si));
            if open {
                slots.extend(sec.containers.iter().map(|c| Slot::Row(c)));
            }
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, LIST_ROW_H, slots.len(), |ui, range| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                for idx in range {
                    match &slots[idx] {
                        Slot::Header(si) => {
                            let sec = &sections[*si];
                            let sid = ids[*si];
                            let mut state =
                                CollapsingState::load_with_default_open(ui.ctx(), sid, true);
                            let open_t = ui.ctx().animate_bool(sid.with("open"), state.is_open());
                            let any_stopped = sec.containers.iter().any(|c| !c.state.is_active());
                            let any_active = sec.containers.iter().any(|c| c.state.is_active());
                            let usage = self.group_usage(&sec.containers);
                            let accent = sec.color.as_deref().and_then(hex_color);
                            ui.allocate_ui(egui::vec2(ui.available_width(), LIST_ROW_H), |ui| {
                                // Header sits at the bottom of its slot; the
                                // slack above is the section's breathing room.
                                ui.add_space((LIST_ROW_H - 40.0).max(0.0));
                                match group_header(
                                    ui,
                                    pal,
                                    &sec.label,
                                    accent,
                                    sec.containers.len(),
                                    open_t,
                                    any_stopped,
                                    any_active,
                                    usage,
                                ) {
                                    Some(GroupOutcome::Toggle) => {
                                        state.toggle(ui);
                                        state.store(ui.ctx());
                                    }
                                    Some(GroupOutcome::BulkAct(action)) => {
                                        let targets: Vec<ContainerId> = sec
                                            .containers
                                            .iter()
                                            .filter(|c| bulk_action_applies(c.state, action))
                                            .map(|c| c.id.clone())
                                            .collect();
                                        if !targets.is_empty() {
                                            pending = Some(ListHit::BulkAct(targets, action));
                                        }
                                    }
                                    None => {}
                                }
                            });
                        }
                        Slot::Row(container) => {
                            ui.allocate_ui(egui::vec2(ui.available_width(), LIST_ROW_H), |ui| {
                                match container_row(ui, pal, container) {
                                    Some(RowOutcome::Act(action)) => {
                                        pending = Some(ListHit::Act((*container).clone(), action));
                                    }
                                    Some(RowOutcome::Open) => {
                                        pending = Some(ListHit::Open((*container).clone()));
                                    }
                                    None => {}
                                }
                            });
                        }
                    }
                }
            });
        pending
    }

    fn settings_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let cfg_path = self.paths.config_file().display().to_string();
        let engine = match &self.status {
            ConnStatus::Connected { version } => Some(version.clone()),
            _ => None,
        };
        let about = About {
            app_version: env!("CARGO_PKG_VERSION"),
            engine: engine.as_deref(),
            config_path: &cfg_path,
        };

        let tray_wanted_before =
            self.config.settings.minimize_to_tray || self.config.settings.start_minimized;

        if let Some(edit) =
            settings::settings_screen(ui, &self.pal, &mut self.config.settings, about)
        {
            self.engine.send(Command::SetMaxStatsStreams(
                self.config.settings.max_stats_streams,
            ));
            self.engine.send(Command::SetStatsRetentionHours(
                self.config.settings.stats_retention_hours,
            ));
            self.persist();
            let tray_wanted_now =
                self.config.settings.minimize_to_tray || self.config.settings.start_minimized;
            if tray_wanted_now != tray_wanted_before {
                self.set_tray_enabled(tray_wanted_now, ctx);
            }
            if edit.theme_changed {
                self.apply_theme(ctx);
            }
            if edit.autostart_changed {
                crate::autostart::sync(self.config.settings.open_at_login);
            }
        }
    }

    fn containers_view(&mut self, ui: &mut egui::Ui) {
        match &self.status {
            ConnStatus::Failed { reason } => {
                let reason = reason.clone();
                if self.centered_state(
                    ui,
                    Icon::Alert,
                    self.pal.text_muted,
                    "Can't reach the Docker daemon",
                    &reason,
                    true,
                ) {
                    self.reconnect();
                }
            }
            _ if self.containers.is_empty() => {
                self.centered_state(
                    ui,
                    Icon::Cube,
                    self.pal.text_faint,
                    "No containers",
                    "Run one with docker run, or refresh to check again.",
                    false,
                );
            }
            _ => match self.container_list(ui) {
                Some(ListHit::Act(container, action)) => self.act(&container, action),
                Some(ListHit::Open(container)) => self.open_detail(&container),
                Some(ListHit::BulkAct(containers, LifecycleAction::Remove)) => {
                    let n = containers.len();
                    self.pending_delete = Some(PendingDelete {
                        title: format!("Delete {n} container{}?", if n == 1 { "" } else { "s" }),
                        detail: "This force-removes them (and their anonymous volumes) from \
                                 Docker. It can't be undone."
                            .to_string(),
                        containers,
                    });
                }
                Some(ListHit::BulkAct(containers, action)) => self.bulk_act(containers, action),
                None => {}
            },
        }
    }

    fn groups_view(&mut self, ui: &mut egui::Ui) {
        if groups::groups_screen(ui, &self.pal, &mut self.config.groups, &self.containers) {
            self.persist();
        }
    }

    /// Render the Registries screen. A change here is either an edit (add,
    /// remove, import) or a background probe landing — either way it's
    /// config-shaped (PLAN §5.2's `Registry` metadata; the secret itself
    /// already went straight to the keychain) and gets persisted the same
    /// way `groups_view` does.
    fn registries_view(&mut self, ui: &mut egui::Ui) {
        if registries::registries_screen(
            ui,
            &self.pal,
            &mut self.registries,
            &mut self.config.registries,
        ) {
            self.persist();
        }
    }

    /// Render the Extensions screen. Unlike `groups_view`/`settings_view`,
    /// grant and toggle edits persist themselves straight through the
    /// extension registry — this only needs to surface a save failure.
    fn extensions_view(&mut self, ui: &mut egui::Ui) {
        if let Some(edit) = extensions::extensions_screen(ui, &self.pal, &mut self.extensions) {
            if let Some(err) = edit.error {
                self.last_error = Some(format!("Couldn't save extension settings: {err}"));
            }
        }
    }

    /// Render the open container screen and act on whatever it asks for.
    fn detail_view(&mut self, ui: &mut egui::Ui) {
        let response = self
            .detail
            .as_mut()
            .expect("detail_view called with no open screen")
            .ui(ui, &self.pal);
        for cmd in response.commands {
            self.engine.send(cmd);
        }
        if let Some(export) = response.export {
            self.write_log_export(export);
        }
        if response.back {
            self.close_detail();
        }
    }

    /// Write a Logs-tab export under `data/rocker/exports/` and report the path
    /// in the notice banner. Kept off the UI thread's hot path — it only runs
    /// on an explicit button press.
    fn write_log_export(&mut self, export: crate::detail::LogExport) {
        let dir = self.paths.exports_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let safe: String = export
            .container
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let name = if safe.is_empty() {
            "container".into()
        } else {
            safe
        };
        let path = dir.join(format!("{name}-{stamp}.log"));
        match std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, export.body.as_bytes()))
        {
            Ok(()) => self.last_notice = Some(format!("Logs exported to {}", path.display())),
            Err(e) => self.last_error = Some(format!("Couldn't export logs: {e}")),
        }
    }
}

impl eframe::App for RockerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.service_tray(ctx);

        if self.view != View::Containers && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.view = View::Containers;
        }

        if let Some(action) = self.header(ctx) {
            match action {
                HeaderAction::Refresh => self.engine.send(Command::RefreshContainers),
                HeaderAction::ToggleSettings => {
                    if self.view == View::Containers {
                        self.close_detail();
                    }
                    self.view = if self.view == View::Settings {
                        View::Containers
                    } else {
                        View::Settings
                    };
                }
                HeaderAction::ToggleGroups => {
                    if self.view == View::Containers {
                        self.close_detail();
                    }
                    self.view = if self.view == View::Groups {
                        View::Containers
                    } else {
                        View::Groups
                    };
                }
                HeaderAction::ToggleExtensions => {
                    if self.view == View::Containers {
                        self.close_detail();
                    }
                    self.view = if self.view == View::Extensions {
                        View::Containers
                    } else {
                        View::Extensions
                    };
                }
                HeaderAction::ToggleRegistries => {
                    if self.view == View::Containers {
                        self.close_detail();
                    }
                    self.view = if self.view == View::Registries {
                        View::Containers
                    } else {
                        View::Registries
                    };
                }
            }
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(self.pal.bg)
                    .inner_margin(egui::Margin::symmetric(14, 12)),
            )
            .show(ctx, |ui| {
                self.error_banner(ui);
                self.notice_banner(ui);

                match self.view {
                    View::Settings => self.settings_view(ui, ctx),
                    View::Groups => self.groups_view(ui),
                    View::Extensions => self.extensions_view(ui),
                    View::Registries => self.registries_view(ui),
                    View::Containers if self.detail.is_some() => self.detail_view(ui),
                    View::Containers => self.containers_view(ui),
                }
            });

        if let Some(pending) = &self.pending_delete {
            match confirm_dialog(ctx, &self.pal, &pending.title, &pending.detail, "Delete") {
                Some(true) => {
                    let PendingDelete { containers, .. } = self.pending_delete.take().unwrap();
                    self.bulk_act(containers, LifecycleAction::Remove);
                }
                Some(false) => self.pending_delete = None,
                None => {}
            }
        }
    }
}
