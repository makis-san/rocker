//! The container screen: everything about one container behind a tab strip —
//! Overview (inspect), Logs (follow), Stats (live usage), and an interactive
//! Terminal (`exec` shell).
//!
//! [`DetailScreen`] owns the per-container UI state and the ring buffers the
//! streams fill. It never touches the engine directly: [`DetailScreen::ui`]
//! returns a [`DetailResponse`] of commands the app forwards, so the screen
//! stays a pure view.

use std::collections::VecDeque;

use egui::{vec2, Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke};
use rocker_core::{Container, ContainerDetail, ContainerId, ContainerState, StatSample};
use rocker_engine::{Command, LifecycleAction, LogLine, LogStream};
use rocker_term::Screen;

use crate::icons::{self, Icon};
use crate::style::{self, Palette};
use crate::terminal;
use crate::{format, widgets};

const LOG_CAP: usize = 4000;
const STAT_CAP: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Logs,
    Stats,
    Terminal,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Overview, Tab::Logs, Tab::Stats, Tab::Terminal];

    fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Logs => "Logs",
            Tab::Stats => "Stats",
            Tab::Terminal => "Terminal",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Tab::Overview => Icon::Info,
            Tab::Logs => Icon::Lines,
            Tab::Stats => Icon::Pulse,
            Tab::Terminal => Icon::Terminal,
        }
    }
}

/// Commands the app should forward to the engine, plus a `back` flag asking it
/// to return to the list.
#[derive(Default)]
pub struct DetailResponse {
    pub back: bool,
    pub commands: Vec<Command>,
}

struct Logs {
    lines: VecDeque<LogLine>,
    filter: String,
    follow: bool,
    wrap: bool,
    /// `Some(reason)` once the stream ends (`reason` empty on a clean EOF).
    ended: Option<String>,
}

impl Default for Logs {
    fn default() -> Self {
        Self {
            lines: VecDeque::new(),
            filter: String::new(),
            follow: true,
            wrap: true,
            ended: None,
        }
    }
}

#[derive(Default)]
struct Stats {
    samples: VecDeque<StatSample>,
    ended: Option<String>,
}

struct Term {
    screen: Screen,
    started: bool,
    ready: bool,
    ended: Option<String>,
    grid: (u16, u16),
}

impl Default for Term {
    fn default() -> Self {
        Self {
            screen: Screen::new(80, 24),
            started: false,
            ready: false,
            ended: None,
            grid: (80, 24),
        }
    }
}

pub struct DetailScreen {
    id: ContainerId,
    name: String,
    image: String,
    summary_state: ContainerState,
    tab: Tab,
    detail: Option<ContainerDetail>,
    logs: Logs,
    stats: Stats,
    term: Term,
    /// `(field key, hide-after time)` for the transient "copied" confirmation.
    copied: Option<(&'static str, f64)>,
}

impl DetailScreen {
    pub fn new(c: &Container) -> Self {
        Self {
            id: c.id.clone(),
            name: c.name.clone(),
            image: c.image.clone(),
            summary_state: c.state,
            tab: Tab::Overview,
            detail: None,
            logs: Logs::default(),
            stats: Stats::default(),
            term: Term::default(),
            copied: None,
        }
    }

    pub fn id(&self) -> &ContainerId {
        &self.id
    }

    /// Debug/test hook: jump straight to a tab by name (unknown names land on
    /// Overview). Used by the `screenshot` example so it can capture a
    /// specific tab headlessly; the running app switches tabs from a click.
    pub fn debug_set_tab(&mut self, name: &str) {
        self.tab = match name {
            "logs" => Tab::Logs,
            "stats" => Tab::Stats,
            "terminal" => Tab::Terminal,
            _ => Tab::Overview,
        };
    }

    /// Debug/test hook: start the terminal session as the "Start session"
    /// button would, without requiring a click. Returns the command to send.
    pub fn debug_start_terminal(&mut self) -> Command {
        self.term.started = true;
        self.term.ended = None;
        Command::OpenExec(self.id.clone())
    }

    fn state(&self) -> ContainerState {
        self.detail
            .as_ref()
            .map(|d| d.state)
            .unwrap_or(self.summary_state)
    }

    // ---- stream inputs -----------------------------------------------------

    pub fn on_inspected(&mut self, d: ContainerDetail) {
        self.summary_state = d.state;
        self.name = d.name.clone();
        self.image = d.image.clone();
        self.detail = Some(d);
    }

    pub fn on_log_lines(&mut self, lines: Vec<LogLine>) {
        self.logs.ended = None;
        for line in lines {
            if self.logs.lines.len() >= LOG_CAP {
                self.logs.lines.pop_front();
            }
            self.logs.lines.push_back(line);
        }
    }

    pub fn on_logs_closed(&mut self, reason: Option<String>) {
        self.logs.ended = Some(reason.unwrap_or_default());
    }

    pub fn on_stat(&mut self, sample: StatSample) {
        self.stats.ended = None;
        if self.stats.samples.len() >= STAT_CAP {
            self.stats.samples.pop_front();
        }
        self.stats.samples.push_back(sample);
    }

    pub fn on_stats_closed(&mut self, reason: Option<String>) {
        self.stats.ended = Some(reason.unwrap_or_default());
    }

    pub fn on_exec_ready(&mut self) {
        self.term.ready = true;
        self.term.ended = None;
    }

    pub fn on_exec_output(&mut self, bytes: &[u8]) {
        self.term.screen.feed(bytes);
    }

    pub fn on_exec_closed(&mut self, reason: Option<String>) {
        self.term.ended = Some(reason.unwrap_or_default());
        self.term.ready = false;
    }

    /// Refresh name / state from a fresh container list.
    pub fn sync_state(&mut self, containers: &[Container]) {
        if let Some(c) = containers.iter().find(|c| c.id == self.id) {
            self.summary_state = c.state;
            self.name = c.name.clone();
            self.image = c.image.clone();
        }
    }

    // ---- rendering -------------------------------------------------------

    pub fn ui(&mut self, ui: &mut egui::Ui, pal: &Palette) -> DetailResponse {
        let mut out = DetailResponse::default();

        // Expire the copy confirmation.
        if let Some((_, until)) = self.copied {
            if ui.input(|i| i.time) > until {
                self.copied = None;
            }
        }

        self.subheader(ui, pal, &mut out);
        ui.add_space(8.0);
        self.tabstrip(ui, pal);
        ui.add_space(12.0);

        match self.tab {
            Tab::Overview => self.overview(ui, pal),
            Tab::Logs => self.logs_tab(ui, pal, &mut out),
            Tab::Stats => self.stats_tab(ui, pal),
            Tab::Terminal => self.terminal_tab(ui, pal, &mut out),
        }

        out
    }

    fn subheader(&mut self, ui: &mut egui::Ui, pal: &Palette, out: &mut DetailResponse) {
        ui.horizontal(|ui| {
            if icons::icon_button(ui, pal, Icon::Back, None, "Back to containers").clicked() {
                out.back = true;
            }
            ui.add_space(4.0);
            widgets::state_indicator(ui, pal, self.state());
            ui.add_space(6.0);
            ui.label(
                RichText::new(&self.name)
                    .size(15.0)
                    .strong()
                    .color(pal.text),
            );

            let status = self
                .detail
                .as_ref()
                .map(|d| d.status_line.clone())
                .unwrap_or_else(|| state_word(self.state()).to_string());
            ui.add_space(8.0);
            ui.label(RichText::new(status).small().color(pal.text_muted));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (icon, tint, tip, action) in lifecycle_actions(self.state()) {
                    if icons::icon_button(ui, pal, icon, tint.then_some(pal.accent), tip).clicked()
                    {
                        out.commands.push(Command::Lifecycle {
                            container: self.id.clone(),
                            action,
                        });
                    }
                }
            });
        });
    }

    fn tabstrip(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let strip = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let mut rects = Vec::new();
                for tab in Tab::ALL {
                    let rect = self.tab_button(ui, pal, tab);
                    rects.push((tab, rect));
                }
                rects
            })
            .inner;

        let baseline_y = strip
            .iter()
            .map(|(_, r)| r.bottom())
            .fold(f32::MIN, f32::max)
            + 4.0;
        let full = ui.max_rect();
        ui.painter().hline(
            full.x_range(),
            baseline_y,
            Stroke::new(1.0, pal.border.gamma_multiply(0.7)),
        );

        if let Some((_, active)) = strip.iter().find(|(t, _)| *t == self.tab) {
            let id = ui.make_persistent_id("tab-indicator");
            let x = ui
                .ctx()
                .animate_value_with_time(id.with("x"), active.left(), 0.14);
            let w = ui
                .ctx()
                .animate_value_with_time(id.with("w"), active.width(), 0.14);
            let seg = Rect::from_min_size(egui::pos2(x, baseline_y - 1.0), vec2(w, 2.0));
            ui.painter()
                .rect_filled(seg, style::radius(1.0), pal.accent);
        }
        ui.add_space(4.0);
    }

    fn tab_button(&mut self, ui: &mut egui::Ui, pal: &Palette, tab: Tab) -> Rect {
        let active = self.tab == tab;
        let label = tab.label();
        let galley = ui.painter().layout_no_wrap(
            label.to_owned(),
            FontId::proportional(12.5),
            Color32::WHITE,
        );
        let w = galley.rect.width() + 14.0 + 12.0; // icon + gaps
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
        let hot = ui.ctx().animate_bool(resp.id, resp.hovered());

        let color = if active {
            pal.text
        } else {
            pal.text_muted.lerp_to_gamma(pal.text, 0.35 * hot)
        };
        let icon_rect = Rect::from_min_size(
            egui::pos2(rect.left(), rect.center().y - 7.0),
            vec2(14.0, 14.0),
        );
        icons::draw(ui.painter(), tab.icon(), icon_rect, color);
        ui.painter().text(
            egui::pos2(icon_rect.right() + 6.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(12.5),
            color,
        );
        if resp.clicked() && !active {
            self.tab = tab;
        }
        rect
    }

    // ---- Overview --------------------------------------------------------

    fn overview(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let Some(detail) = self.detail.clone() else {
            waiting(ui, pal, "Loading details");
            return;
        };

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());

                section(ui, pal, "Status");
                kv(ui, pal, "State", state_word(detail.state));
                if !detail.status_line.is_empty() {
                    kv(ui, pal, "Summary", &detail.status_line);
                }
                if !detail.created.is_empty() {
                    kv(ui, pal, "Created", &format::timestamp(&detail.created));
                }
                if !detail.started_at.is_empty() && detail.state.is_active() {
                    kv(ui, pal, "Started", &format::timestamp(&detail.started_at));
                }
                if matches!(detail.state, ContainerState::Exited | ContainerState::Dead) {
                    if !detail.finished_at.is_empty() {
                        kv(ui, pal, "Finished", &format::timestamp(&detail.finished_at));
                    }
                    if let Some(code) = detail.exit_code {
                        kv(ui, pal, "Exit code", &code.to_string());
                    }
                }
                if detail.restart_count > 0 {
                    kv(ui, pal, "Restarts", &detail.restart_count.to_string());
                }
                kv(ui, pal, "Restart policy", &detail.restart_policy);
                if let Some(h) = &detail.health {
                    kv(
                        ui,
                        pal,
                        "Health",
                        &format!("{} ({} failing)", h.status, h.failing_streak),
                    );
                    if let Some(o) = &h.last_output {
                        let summary = probe_summary(o);
                        self.copy_row(ui, pal, "Last probe", &summary, "probe");
                    }
                }
                if let Some(err) = &detail.error {
                    kv(ui, pal, "Error", err);
                }

                section(ui, pal, "Image");
                self.copy_row(ui, pal, "Image", &detail.image, "img");
                self.copy_row(ui, pal, "Image ID", short_id(&detail.image_id), "imgid");
                if !detail.platform.is_empty() {
                    kv(ui, pal, "Platform", &detail.platform);
                }
                let cid = self.id.0.clone();
                self.copy_row(ui, pal, "Container ID", &cid, "cid");

                section(ui, pal, "Command");
                if !detail.command.is_empty() {
                    kv_mono(ui, pal, "Command", &detail.command);
                }
                if !detail.working_dir.is_empty() {
                    kv_mono(ui, pal, "Working dir", &detail.working_dir);
                }
                if !detail.user.is_empty() {
                    kv(ui, pal, "User", &detail.user);
                }

                section(ui, pal, "Ports");
                if detail.ports.is_empty() {
                    muted(ui, pal, "No published ports.");
                } else {
                    for p in &detail.ports {
                        let host = match (&p.host_ip, p.host_port) {
                            (Some(ip), Some(port)) => format!("{ip}:{port}"),
                            (None, Some(port)) => format!("0.0.0.0:{port}"),
                            _ => "—".to_string(),
                        };
                        mono_line(
                            ui,
                            pal,
                            &format!("{host}  \u{2192}  {}/{}", p.container_port, p.protocol),
                        );
                    }
                }

                section(ui, pal, "Networks");
                if detail.networks.is_empty() {
                    muted(ui, pal, "Not attached to any network.");
                } else {
                    for n in &detail.networks {
                        kv_mono(
                            ui,
                            pal,
                            &n.name,
                            &format!(
                                "{}{}",
                                if n.ip.is_empty() { "no address" } else { &n.ip },
                                if n.gateway.is_empty() {
                                    String::new()
                                } else {
                                    format!("  via {}", n.gateway)
                                }
                            ),
                        );
                    }
                }

                section(ui, pal, "Mounts");
                if detail.mounts.is_empty() {
                    muted(ui, pal, "No mounts.");
                } else {
                    for m in &detail.mounts {
                        let src = m.name.clone().unwrap_or_else(|| m.source.clone());
                        mono_line(
                            ui,
                            pal,
                            &format!(
                                "{}  \u{2190}  {}  ({}, {})",
                                m.destination,
                                src,
                                m.kind,
                                if m.read_write { "rw" } else { "ro" }
                            ),
                        );
                    }
                }

                if !detail.env.is_empty() {
                    section(ui, pal, &format!("Environment ({})", detail.env.len()));
                    for (k, v) in &detail.env {
                        mono_line(ui, pal, &format!("{k}={v}"));
                    }
                }

                if !detail.labels.is_empty() {
                    section(ui, pal, &format!("Labels ({})", detail.labels.len()));
                    for (k, v) in &detail.labels {
                        mono_line(ui, pal, &format!("{k} = {v}"));
                    }
                }

                ui.add_space(24.0);
            });
    }

    fn copy_row(
        &mut self,
        ui: &mut egui::Ui,
        pal: &Palette,
        key: &str,
        val: &str,
        id: &'static str,
    ) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add_sized(
                vec2(KEY_W, 18.0),
                egui::Label::new(RichText::new(key).small().color(pal.text_muted)),
            );
            ui.label(RichText::new(val).monospace().size(11.5).color(pal.text));
            if icons::icon_button(ui, pal, Icon::Copy, None, "Copy").clicked() {
                ui.ctx().copy_text(val.to_owned());
                self.copied = Some((id, ui.input(|i| i.time) + 1.2));
            }
            if matches!(self.copied, Some((k, _)) if k == id) {
                ui.label(RichText::new("copied").small().color(pal.accent));
            }
        });
    }

    // ---- Logs ----------------------------------------------------------

    fn logs_tab(&mut self, ui: &mut egui::Ui, pal: &Palette, out: &mut DetailResponse) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let field = egui::TextEdit::singleline(&mut self.logs.filter)
                .hint_text("Filter")
                .desired_width(200.0);
            ui.add(field);

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if icons::icon_button(ui, pal, Icon::Close, None, "Clear").clicked() {
                    self.logs.lines.clear();
                }
                if icons::icon_button(ui, pal, Icon::Copy, None, "Copy all").clicked() {
                    let joined: String = self
                        .logs
                        .lines
                        .iter()
                        .map(|l| l.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    ui.ctx().copy_text(joined);
                }
                if icons::toggle_icon_button(ui, pal, Icon::Lines, self.logs.wrap, "Wrap lines")
                    .clicked()
                {
                    self.logs.wrap = !self.logs.wrap;
                }
                if icons::toggle_icon_button(ui, pal, Icon::Pulse, self.logs.follow, "Follow tail")
                    .clicked()
                {
                    self.logs.follow = !self.logs.follow;
                }
            });
        });
        ui.add_space(6.0);

        let filter = self.logs.filter.to_lowercase();
        let matching: Vec<&LogLine> = self
            .logs
            .lines
            .iter()
            .filter(|l| filter.is_empty() || l.text.to_lowercase().contains(&filter))
            .collect();

        if self.logs.lines.is_empty() {
            waiting(ui, pal, "No output captured yet");
        } else {
            let frame = egui::Frame::new()
                .fill(pal.term_bg)
                .inner_margin(egui::Margin::symmetric(10, 8))
                .corner_radius(style::radius(pal.corner));
            frame.show(ui, |ui| {
                let scroll = egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .stick_to_bottom(self.logs.follow);
                let out_scroll = scroll.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for line in &matching {
                        let color = match line.stream {
                            LogStream::Stderr => pal.unhealthy.lerp_to_gamma(pal.term_fg, 0.25),
                            LogStream::Stdout => pal.term_fg.gamma_multiply(0.92),
                        };
                        let rt = RichText::new(&line.text)
                            .monospace()
                            .size(11.5)
                            .color(color);
                        let widget = egui::Label::new(rt).wrap_mode(if self.logs.wrap {
                            egui::TextWrapMode::Wrap
                        } else {
                            egui::TextWrapMode::Extend
                        });
                        ui.add(widget);
                    }
                });
                // Drop follow if the user scrolls off the bottom.
                let at_bottom = out_scroll.state.offset.y + out_scroll.inner_rect.height()
                    >= out_scroll.content_size.y - 8.0;
                if self.logs.follow && !at_bottom && ui.input(|i| i.raw_scroll_delta.y != 0.0) {
                    self.logs.follow = false;
                }
            });
        }

        if let Some(reason) = self.logs.ended.clone() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let msg = if reason.is_empty() {
                    "Log stream ended.".to_string()
                } else {
                    format!("Log stream ended: {reason}")
                };
                ui.label(RichText::new(msg).small().color(pal.text_muted));
                if ui
                    .add(
                        egui::Label::new(RichText::new("Reconnect").small().color(pal.accent))
                            .sense(Sense::click()),
                    )
                    .clicked()
                {
                    self.logs.ended = None;
                    out.commands.push(Command::OpenLogs(self.id.clone()));
                }
            });
        }
    }

    // ---- Stats -------------------------------------------------------

    fn stats_tab(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        if self.stats.samples.is_empty() {
            if !self.state().is_active() {
                muted(ui, pal, "Container isn't running — no live stats.");
            } else {
                waiting(ui, pal, "Waiting for the first sample");
            }
            return;
        }

        let samples: Vec<StatSample> = self.stats.samples.iter().copied().collect();
        let latest = *samples.last().unwrap();

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} PIDs  \u{00b7}  {:.0} cores",
                    latest.pids, latest.cpu_cores
                ))
                .small()
                .color(pal.text_muted),
            );
            if !self.state().is_active() {
                ui.label(
                    RichText::new("\u{00b7}  stopped")
                        .small()
                        .color(pal.text_faint),
                );
            }
        });
        ui.add_space(10.0);

        let cpu: Vec<f32> = samples.iter().map(|s| s.cpu_pct).collect();
        let mem: Vec<f32> = samples.iter().map(|s| s.mem_used as f32).collect();
        let rx = rates(&samples, |s| s.net_rx);
        let tx = rates(&samples, |s| s.net_tx);
        let rd = rates(&samples, |s| s.blk_read);
        let wr = rates(&samples, |s| s.blk_write);

        let cpu_scale = (latest.cpu_cores * 100.0).max(cpu.iter().copied().fold(1.0, f32::max));
        let mem_scale = if latest.mem_limit > 0 {
            latest.mem_limit as f32
        } else {
            mem.iter().copied().fold(1.0, f32::max)
        };
        let net_scale = rx.iter().chain(tx.iter()).copied().fold(1.0, f32::max);
        let blk_scale = rd.iter().chain(wr.iter()).copied().fold(1.0, f32::max);

        let avail_w = ui.available_width();
        let card_w = ((avail_w - style::MD) / 2.0).max(180.0);

        grid_2x2(ui, card_w, |ui, slot| match slot {
            0 => metric_card(
                ui,
                pal,
                MetricCard {
                    label: "CPU",
                    value: &format!("{:.1}%", latest.cpu_pct),
                    sub: &format!("of {:.0} cores", latest.cpu_cores),
                    series: &cpu,
                    scale: cpu_scale,
                    color: pal.accent,
                },
            ),
            1 => metric_card(
                ui,
                pal,
                MetricCard {
                    label: "Memory",
                    value: &format::bytes(latest.mem_used),
                    sub: &format!(
                        "{:.0}% of {}",
                        latest.mem_frac() * 100.0,
                        if latest.mem_limit > 0 {
                            format::bytes(latest.mem_limit)
                        } else {
                            "host".to_string()
                        }
                    ),
                    series: &mem,
                    scale: mem_scale,
                    color: pal.running,
                },
            ),
            2 => metric_card(
                ui,
                pal,
                MetricCard {
                    label: "Network",
                    value: &format!(
                        "\u{2193} {}",
                        format::rate(rx.last().copied().unwrap_or(0.0) as f64)
                    ),
                    sub: &format!(
                        "\u{2191} {}  \u{00b7}  {} total",
                        format::rate(tx.last().copied().unwrap_or(0.0) as f64),
                        format::bytes(latest.net_rx + latest.net_tx)
                    ),
                    series: &rx,
                    scale: net_scale,
                    color: pal.paused,
                },
            ),
            _ => metric_card(
                ui,
                pal,
                MetricCard {
                    label: "Block I/O",
                    value: &format!(
                        "\u{2193} {}",
                        format::rate(rd.last().copied().unwrap_or(0.0) as f64)
                    ),
                    sub: &format!(
                        "\u{2191} {}  \u{00b7}  {} total",
                        format::rate(wr.last().copied().unwrap_or(0.0) as f64),
                        format::bytes(latest.blk_read + latest.blk_write)
                    ),
                    series: &rd,
                    scale: blk_scale,
                    color: pal.text_muted,
                },
            ),
        });

        if let Some(reason) = self.stats.ended.clone() {
            ui.add_space(8.0);
            let msg = if reason.is_empty() {
                "Stats stream ended.".to_string()
            } else {
                format!("Stats stream ended: {reason}")
            };
            ui.label(RichText::new(msg).small().color(pal.text_muted));
        }
    }

    // ---- Terminal ---------------------------------------------------

    fn terminal_tab(&mut self, ui: &mut egui::Ui, pal: &Palette, out: &mut DetailResponse) {
        if !self.term.started {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                icons::draw(ui.painter(), Icon::Terminal, r, pal.text_faint);
                ui.add_space(12.0);
                ui.label(
                    RichText::new("Open a shell in this container")
                        .size(14.0)
                        .strong()
                        .color(pal.text),
                );
                ui.add_space(4.0);
                if self.state().is_active() {
                    ui.label(
                        RichText::new("Runs /bin/sh (or bash) with a pseudo-TTY.")
                            .color(pal.text_muted),
                    );
                    ui.add_space(14.0);
                    if icons::primary_button(ui, pal, "Start session").clicked() {
                        self.term.started = true;
                        self.term.ended = None;
                        out.commands.push(Command::OpenExec(self.id.clone()));
                    }
                } else {
                    ui.label(
                        RichText::new("The container must be running first.").color(pal.text_muted),
                    );
                }
            });
            return;
        }

        let cs = terminal::cell_size(ui);
        let avail = ui.available_rect_before_wrap();
        let cols = ((avail.width() - 12.0) / cs.x).floor().clamp(20.0, 400.0) as u16;
        let rows = ((avail.height() - 10.0) / cs.y).floor().clamp(4.0, 200.0) as u16;
        if (cols, rows) != self.term.grid {
            self.term.grid = (cols, rows);
            self.term.screen.resize(cols, rows);
            out.commands.push(Command::ExecResize { cols, rows });
        }

        let size = vec2(cols as f32 * cs.x + 12.0, rows as f32 * cs.y + 10.0);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let id = ui.make_persistent_id(("term-surface", &self.id.0));
        let (_resp, focused) = terminal::surface(ui, rect, id);
        terminal::paint(ui, pal, &self.term.screen, rect, focused);

        if focused {
            let bytes = terminal::take_input(ui);
            if !bytes.is_empty() {
                out.commands.push(Command::ExecInput(bytes));
                ui.ctx().request_repaint();
            }
            // Swallow text so egui doesn't also process it.
            ui.ctx().input_mut(|i| {
                i.events
                    .retain(|e| !matches!(e, egui::Event::Text(_) | egui::Event::Paste(_)))
            });
        } else {
            ui.painter().text(
                rect.right_bottom() - vec2(8.0, 6.0),
                Align2::RIGHT_BOTTOM,
                "click to type",
                FontId::proportional(11.0),
                pal.text_faint,
            );
        }

        if self.term.started && !self.term.ready && self.term.ended.is_none() {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "connecting…",
                FontId::proportional(12.0),
                pal.text_muted,
            );
            ui.ctx().request_repaint();
        }

        if let Some(reason) = self.term.ended.clone() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let msg = if reason.is_empty() {
                    "Session ended.".to_string()
                } else {
                    format!("Session ended: {reason}")
                };
                ui.label(RichText::new(msg).small().color(pal.text_muted));
                if icons::primary_button(ui, pal, "Start again").clicked() {
                    self.term.screen = Screen::new(self.term.grid.0, self.term.grid.1);
                    self.term.ended = None;
                    self.term.ready = false;
                    out.commands.push(Command::OpenExec(self.id.clone()));
                }
            });
        }
    }
}

// ---- lifecycle button set ---------------------------------------------

fn lifecycle_actions(state: ContainerState) -> Vec<(Icon, bool, &'static str, LifecycleAction)> {
    match state {
        ContainerState::Running | ContainerState::Restarting => vec![
            (Icon::Restart, false, "Restart", LifecycleAction::Restart),
            (Icon::Pause, false, "Pause", LifecycleAction::Pause),
            (Icon::Stop, false, "Stop", LifecycleAction::Stop),
        ],
        ContainerState::Paused => vec![
            (Icon::Play, true, "Resume", LifecycleAction::Unpause),
            (Icon::Stop, false, "Stop", LifecycleAction::Stop),
        ],
        _ => vec![(Icon::Play, true, "Start", LifecycleAction::Start)],
    }
}

fn state_word(state: ContainerState) -> &'static str {
    use ContainerState::*;
    match state {
        Created => "Created",
        Running => "Running",
        Paused => "Paused",
        Restarting => "Restarting",
        Removing => "Removing",
        Exited => "Exited",
        Dead => "Dead",
        Unknown => "Unknown",
    }
}

fn short_id(id: &str) -> &str {
    let n = id.strip_prefix("sha256:").unwrap_or(id);
    &n[..n.len().min(19)]
}

/// A healthcheck's raw probe output can be a multi-line curl trace or a whole
/// JSON body — real output seen in the wild ran to several kilobytes. Collapse
/// it to one line and cap its length so a single field never floods the whole
/// Overview tab; the full text is still one click away via the copy button
/// callers can add if that turns out to be needed.
const PROBE_MAX: usize = 100;

fn probe_summary(raw: &str) -> String {
    let one_line: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= PROBE_MAX {
        one_line
    } else {
        let truncated: String = one_line.chars().take(PROBE_MAX).collect();
        format!("{truncated}\u{2026}")
    }
}

// ---- overview field helpers -----------------------------------------

const KEY_W: f32 = 132.0;

fn section(ui: &mut egui::Ui, pal: &Palette, title: &str) {
    ui.add_space(18.0);
    ui.label(RichText::new(title).small().strong().color(pal.text_muted));
    ui.add_space(4.0);
}

fn kv(ui: &mut egui::Ui, pal: &Palette, key: &str, val: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add_sized(
            vec2(KEY_W, 18.0),
            egui::Label::new(RichText::new(key).small().color(pal.text_muted)),
        );
        ui.add(egui::Label::new(RichText::new(val).color(pal.text)).wrap());
    });
}

fn kv_mono(ui: &mut egui::Ui, pal: &Palette, key: &str, val: &str) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add_sized(
            vec2(KEY_W, 18.0),
            egui::Label::new(RichText::new(key).small().color(pal.text_muted)),
        );
        ui.add(egui::Label::new(RichText::new(val).monospace().size(11.5).color(pal.text)).wrap());
    });
}

fn mono_line(ui: &mut egui::Ui, pal: &Palette, val: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(val)
                .monospace()
                .size(11.5)
                .color(pal.text_muted),
        )
        .wrap(),
    );
}

fn muted(ui: &mut egui::Ui, pal: &Palette, val: &str) {
    ui.label(RichText::new(val).color(pal.text_muted));
}

fn waiting(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.add_space(56.0);
    ui.vertical_centered(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        let phase = ui.input(|i| i.time) as f32 * 3.0;
        let pulse = 0.5 - 0.5 * phase.cos();
        ui.painter().circle_filled(
            rect.center(),
            2.6 + 1.6 * pulse,
            pal.accent.gamma_multiply(0.4 + 0.5 * pulse),
        );
        ui.add_space(10.0);
        ui.label(RichText::new(text).color(pal.text_muted));
    });
    ui.ctx().request_repaint();
}

// ---- stats helpers ------------------------------------------------

/// Per-second deltas of a cumulative counter, assuming ~1 Hz samples.
fn rates(samples: &[StatSample], get: impl Fn(&StatSample) -> u64) -> Vec<f32> {
    samples
        .windows(2)
        .map(|w| get(&w[1]).saturating_sub(get(&w[0])) as f32)
        .collect()
}

fn grid_2x2(ui: &mut egui::Ui, card_w: f32, mut cell: impl FnMut(&mut egui::Ui, usize)) {
    for row in 0..2 {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = style::MD;
            for col in 0..2 {
                ui.allocate_ui(vec2(card_w, 96.0), |ui| {
                    ui.set_width(card_w);
                    cell(ui, row * 2 + col);
                });
            }
        });
        if row == 0 {
            ui.add_space(style::MD);
        }
    }
}

/// One Stats-tab tile: a label, a big current value, a quiet sub-line, and a
/// sparkline of recent history. Bundled into a struct rather than passed
/// positionally since a metric card is one cohesive "thing to show", not a
/// pile of independent parameters.
struct MetricCard<'a> {
    label: &'a str,
    value: &'a str,
    sub: &'a str,
    series: &'a [f32],
    scale: f32,
    color: Color32,
}

fn metric_card(ui: &mut egui::Ui, pal: &Palette, card: MetricCard<'_>) {
    let MetricCard {
        label,
        value,
        sub,
        series,
        scale,
        color,
    } = card;
    egui::Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(style::radius(pal.corner))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(label.to_uppercase())
                    .small()
                    .color(pal.text_muted),
            );
            ui.add_space(2.0);
            ui.label(RichText::new(value).monospace().size(20.0).color(pal.text));
            ui.add_space(1.0);
            ui.label(RichText::new(sub).small().color(pal.text_faint));
            ui.add_space(6.0);
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
            sparkline(ui.painter(), rect, series, scale, color);
        });
}

/// A filled area sparkline: no axes, tonal fill under a 1.5px line. Draws a
/// faint baseline when there is not yet enough data.
fn sparkline(painter: &egui::Painter, rect: Rect, series: &[f32], scale: f32, color: Color32) {
    let base = rect.bottom() - 1.0;
    if series.len() < 2 || scale <= 0.0 {
        painter.hline(
            rect.x_range(),
            base,
            Stroke::new(1.0, color.gamma_multiply(0.35)),
        );
        return;
    }
    let n = series.len();
    let dx = rect.width() / (n - 1) as f32;
    let pt = |i: usize| {
        let v = (series[i] / scale).clamp(0.0, 1.0);
        egui::pos2(
            rect.left() + i as f32 * dx,
            base - v * (rect.height() - 2.0),
        )
    };
    let line: Vec<egui::Pos2> = (0..n).map(pt).collect();

    let mut area = line.clone();
    area.push(egui::pos2(rect.right(), base));
    area.push(egui::pos2(rect.left(), base));
    painter.add(egui::Shape::convex_polygon(
        area,
        color.gamma_multiply(0.14),
        Stroke::NONE,
    ));
    painter.add(egui::Shape::line(line, Stroke::new(1.5, color)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocker_core::{
        ContainerId, ContainerState, HealthInfo, MountInfo, NetworkInfo, PortBinding,
    };
    use rocker_engine::LogStream;

    fn fake_container(id: &str) -> Container {
        Container {
            id: ContainerId::new(id),
            name: "web-1".into(),
            image: "ghcr.io/example/web:1.4.0".into(),
            state: ContainerState::Running,
            status: "Up 3 hours (healthy)".into(),
            ports: vec![PortBinding {
                container_port: 80,
                protocol: "tcp".into(),
                host_ip: Some("0.0.0.0".into()),
                host_port: Some(8080),
            }],
            compose_project: Some("example".into()),
            compose_service: Some("web".into()),
        }
    }

    fn fake_detail(id: &ContainerId) -> ContainerDetail {
        ContainerDetail {
            id: id.clone(),
            name: "web-1".into(),
            image: "ghcr.io/example/web:1.4.0".into(),
            image_id: "sha256:abcdef0123456789abcdef0123456789".into(),
            state: ContainerState::Running,
            status_line: "Up (since 2026-09-08 12:00)".into(),
            created: "2026-09-08T12:00:00.000Z".into(),
            started_at: "2026-09-08T12:00:01.000Z".into(),
            finished_at: String::new(),
            restart_count: 1,
            exit_code: None,
            error: None,
            command: "/bin/web-server --port 80".into(),
            working_dir: "/app".into(),
            user: "app".into(),
            restart_policy: "unless-stopped".into(),
            platform: "linux".into(),
            log_path: "/var/lib/docker/containers/x/x.log".into(),
            env: vec![
                ("PATH".into(), "/usr/bin".into()),
                ("PORT".into(), "80".into()),
            ],
            labels: vec![("com.docker.compose.project".into(), "example".into())],
            ports: vec![PortBinding {
                container_port: 80,
                protocol: "tcp".into(),
                host_ip: Some("0.0.0.0".into()),
                host_port: Some(8080),
            }],
            mounts: vec![MountInfo {
                kind: "volume".into(),
                name: Some("web-data".into()),
                source: "/var/lib/docker/volumes/web-data/_data".into(),
                destination: "/data".into(),
                read_write: true,
            }],
            networks: vec![NetworkInfo {
                name: "bridge".into(),
                ip: "172.17.0.2".into(),
                gateway: "172.17.0.1".into(),
                mac: "02:42:ac:11:00:02".into(),
            }],
            health: Some(HealthInfo {
                status: "healthy".into(),
                failing_streak: 0,
                last_output: Some("ok".into()),
            }),
            compose_project: Some("example".into()),
            compose_service: Some("web".into()),
        }
    }

    /// Lay every tab out headlessly, with representative data loaded through
    /// the same `on_*` path the app uses, at a few sizes. Catches layout-math
    /// panics (bad rects, division by zero in the sparkline/tab-strip math,
    /// negative sizes) well before they'd show up in the running app.
    #[test]
    fn all_tabs_lay_out_without_panic() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());

        let container = fake_container("deadbeefcafe");
        let mut screen = DetailScreen::new(&container);
        screen.on_inspected(fake_detail(&container.id));
        for i in 0..30u64 {
            screen.on_log_lines(vec![
                LogLine {
                    stream: LogStream::Stdout,
                    text: format!("line {i}: listening on :80"),
                },
                LogLine {
                    stream: LogStream::Stderr,
                    text: format!("line {i}: warning: slow query"),
                },
            ]);
            screen.on_stat(StatSample {
                cpu_pct: (i as f32) * 3.3 % 180.0,
                cpu_cores: 2.0,
                mem_used: 64_000_000 + i * 1_000_000,
                mem_limit: 512_000_000,
                net_rx: i * 4096,
                net_tx: i * 1024,
                blk_read: i * 8192,
                blk_write: i * 2048,
                pids: 7,
            });
        }
        screen.on_exec_ready();
        screen.on_exec_output(b"$ echo hi\r\nhi\r\n\x1b[32mgreen\x1b[0m\r\n");

        for size in [egui::vec2(420.0, 500.0), egui::vec2(900.0, 700.0)] {
            for tab in Tab::ALL {
                screen.tab = tab;
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), size)),
                    ..Default::default()
                };
                let mut response = DetailResponse::default();
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        response = screen.ui(ui, &pal);
                    });
                });
                assert!(!response.back, "no click happened, so back must stay false");
            }
        }
    }

    /// The unstarted terminal tab must not push an `OpenExec` on its own —
    /// only an explicit "Start session" click should.
    #[test]
    fn terminal_tab_does_not_autostart() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());
        let container = fake_container("cafefeed0001");
        let mut screen = DetailScreen::new(&container);
        screen.tab = Tab::Terminal;

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(600.0, 400.0),
            )),
            ..Default::default()
        };
        let mut response = DetailResponse::default();
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                response = screen.ui(ui, &pal);
            });
        });
        assert!(
            !response
                .commands
                .iter()
                .any(|c| matches!(c, Command::OpenExec(_))),
            "the terminal must wait for an explicit Start click"
        );
    }
}
