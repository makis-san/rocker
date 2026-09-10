//! The container screen: everything about one container behind a tab strip —
//! Overview (inspect), Logs (follow), Stats (live usage), and an interactive
//! Terminal (`exec` shell).
//!
//! [`DetailScreen`] owns the per-container UI state and the ring buffers the
//! streams fill. It never touches the engine directly: [`DetailScreen::ui`]
//! returns a [`DetailResponse`] of commands the app forwards, so the screen
//! stays a pure view.

use std::collections::{HashSet, VecDeque};

use egui::text::{LayoutJob, TextFormat};
use egui::{vec2, Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke};
use egui_plot::{Corner, Legend, Line, Plot, PlotPoints};
use regex_lite::Regex;
use rocker_core::{Container, ContainerDetail, ContainerId, ContainerState, ExecAudit, StatSample};
use rocker_engine::{Command, LifecycleAction, LogLine, LogStream, LogTail};
use rocker_term::Screen;

use crate::icons::{self, Icon};
use crate::style::{self, Palette};
use crate::terminal;
use crate::{format, widgets};

const LOG_CAP: usize = 4000;
/// Live samples kept in memory for the open container screen. ~1 Hz, so this is
/// an hour of history for the Stats tab's chart; the `redb` store (PLAN §6)
/// holds the longer retention window and seeds this on open.
const STAT_CAP: usize = 3600;

/// The tail sizes offered in the Logs tab, paired with the label on the
/// segmented control. `None` == "all".
const TAIL_CHOICES: [(&str, Option<u32>); 4] = [
    ("100", Some(100)),
    ("1k", Some(1_000)),
    ("10k", Some(10_000)),
    ("All", None),
];

fn tail_to_index(tail: LogTail) -> usize {
    match tail {
        LogTail::Lines(100) => 0,
        LogTail::Lines(1_000) => 1,
        LogTail::Lines(10_000) => 2,
        LogTail::All => 3,
        // Any other line count (the default 400) sits closest to "1k".
        LogTail::Lines(_) => 1,
    }
}

fn index_to_tail(i: usize) -> LogTail {
    match TAIL_CHOICES.get(i).and_then(|&(_, n)| n) {
        Some(n) => LogTail::Lines(n),
        None => LogTail::All,
    }
}

/// A log dump the app should write to disk (the Logs tab's "Export").
pub struct LogExport {
    pub container: String,
    pub body: String,
}

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
/// to return to the list and an optional log dump to write out.
#[derive(Default)]
pub struct DetailResponse {
    pub back: bool,
    pub commands: Vec<Command>,
    pub export: Option<LogExport>,
}

struct Logs {
    lines: VecDeque<LogLine>,
    filter: String,
    /// Treat `filter` as a regular expression rather than a case-insensitive
    /// substring.
    use_regex: bool,
    /// Compiled matcher for the current `(filter, use_regex)`. `Ok(None)` when
    /// the filter is empty (everything matches); `Err` carries the regex
    /// compile error to show under the field.
    matcher: Result<Option<Regex>, String>,
    matcher_key: (String, bool),
    follow: bool,
    wrap: bool,
    /// Show Docker's per-line timestamp. Pure view state — the engine always
    /// streams timestamps, so toggling this never restarts anything.
    timestamps: bool,
    tail: LogTail,
    /// Lines dropped off the front of the ring since the stream last
    /// (re)started, so the view can mark that the history is clipped.
    trimmed: u64,
    /// `Some(reason)` once the stream ends (`reason` empty on a clean EOF).
    ended: Option<String>,
}

impl Default for Logs {
    fn default() -> Self {
        Self {
            lines: VecDeque::new(),
            filter: String::new(),
            use_regex: false,
            matcher: Ok(None),
            matcher_key: (String::new(), false),
            follow: true,
            wrap: true,
            timestamps: false,
            tail: LogTail::default(),
            trimmed: 0,
            ended: None,
        }
    }
}

impl Logs {
    /// Recompile `matcher` if the filter text or the regex toggle changed since
    /// last frame. Substring mode is a case-insensitive literal; regex mode is
    /// verbatim (the user adds `(?i)` themselves).
    fn sync_matcher(&mut self) {
        let key = (self.filter.clone(), self.use_regex);
        if key == self.matcher_key {
            return;
        }
        self.matcher_key = key;
        self.matcher = if self.filter.is_empty() {
            Ok(None)
        } else {
            let pattern = if self.use_regex {
                self.filter.clone()
            } else {
                format!("(?i){}", regex_lite::escape(&self.filter))
            };
            Regex::new(&pattern).map(Some).map_err(|e| e.to_string())
        };
    }

    /// The compiled regex, if the filter is active and valid.
    fn active_matcher(&self) -> Option<&Regex> {
        match &self.matcher {
            Ok(Some(re)) => Some(re),
            _ => None,
        }
    }

    fn line_matches(&self, line: &LogLine) -> bool {
        match self.active_matcher() {
            Some(re) => re.is_match(&line.text),
            None => true,
        }
    }
}

#[derive(Default)]
struct Stats {
    samples: VecDeque<StatSample>,
    ended: Option<String>,
}

/// Which kind of interactive session the Terminal tab opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionMode {
    /// A fresh `exec` shell (bash → sh).
    Shell,
    /// Attach to the container's main process stdio.
    Attach,
}

struct Term {
    screen: Screen,
    mode: SessionMode,
    started: bool,
    ready: bool,
    ended: Option<String>,
    grid: (u16, u16),
}

impl Term {
    /// The command that opens a session in the current mode.
    fn open_command(&self, id: &ContainerId) -> Command {
        match self.mode {
            SessionMode::Shell => Command::OpenExec(id.clone()),
            SessionMode::Attach => Command::OpenAttach(id.clone()),
        }
    }
}

impl Default for Term {
    fn default() -> Self {
        Self {
            screen: Screen::new(80, 24),
            mode: SessionMode::Shell,
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
    /// Overview sections the user has folded away, by stable key. A key that is
    /// absent means the section is expanded, so the default state is all-open.
    ov_collapsed: HashSet<&'static str>,
    /// Recent terminal-session audit rows (newest first), filtered to this
    /// container for the Terminal tab's "recent sessions" list.
    exec_audit: Vec<ExecAudit>,
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
            ov_collapsed: HashSet::new(),
            exec_audit: Vec::new(),
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
                self.logs.trimmed += 1;
            }
            self.logs.lines.push_back(line);
        }
    }

    pub fn on_logs_closed(&mut self, reason: Option<String>) {
        self.logs.ended = Some(reason.unwrap_or_default());
    }

    /// Clear the buffer and re-open the stream — used when the tail size
    /// changes and on an explicit reconnect. The fresh stream re-delivers its
    /// own tail, so keeping the old lines would just double them up.
    fn restart_logs(&mut self, out: &mut DetailResponse) {
        self.logs.lines.clear();
        self.logs.trimmed = 0;
        self.logs.ended = None;
        out.commands.push(Command::OpenLogs {
            container: self.id.clone(),
            tail: self.logs.tail,
        });
    }

    pub fn on_stat(&mut self, sample: StatSample) {
        self.stats.ended = None;
        if self.stats.samples.len() >= STAT_CAP {
            self.stats.samples.pop_front();
        }
        self.stats.samples.push_back(sample);
    }

    /// Seed the Stats tab with persisted history (from `redb`) on open. Merged
    /// with whatever the live stream has already delivered, sorted by time and
    /// de-duplicated on `ts_ms`, capped to `STAT_CAP`.
    pub fn on_stat_history(&mut self, history: Vec<StatSample>) {
        if history.is_empty() {
            return;
        }
        let mut merged: Vec<StatSample> = history;
        merged.extend(self.stats.samples.iter().copied());
        merged.sort_by_key(|s| s.ts_ms);
        merged.dedup_by_key(|s| s.ts_ms);
        if merged.len() > STAT_CAP {
            merged.drain(0..merged.len() - STAT_CAP);
        }
        self.stats.samples = merged.into();
    }

    pub fn on_stats_closed(&mut self, reason: Option<String>) {
        self.stats.ended = Some(reason.unwrap_or_default());
    }

    /// Receive the recent exec-audit rows; keep only this container's, newest
    /// first, for the Terminal tab's session history.
    pub fn on_exec_audit(&mut self, rows: Vec<ExecAudit>) {
        self.exec_audit = rows
            .into_iter()
            .filter(|r| r.container == self.id.0)
            .collect();
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
            Stroke::new(1.0_f32, pal.border.gamma_multiply(0.7)),
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

        // Lifted out of `self` for the duration of the layout so the section
        // bodies can still take `&mut self` (for `copy_row`); put back below.
        let mut collapsed = std::mem::take(&mut self.ov_collapsed);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());

                collapsing_section(ui, pal, &mut collapsed, "status", "Status", |ui| {
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
                });

                collapsing_section(ui, pal, &mut collapsed, "image", "Image", |ui| {
                    self.copy_row(ui, pal, "Image", &detail.image, "img");
                    self.copy_row(ui, pal, "Image ID", short_id(&detail.image_id), "imgid");
                    if !detail.platform.is_empty() {
                        kv(ui, pal, "Platform", &detail.platform);
                    }
                    let cid = self.id.0.clone();
                    self.copy_row(ui, pal, "Container ID", &cid, "cid");
                });

                collapsing_section(ui, pal, &mut collapsed, "command", "Command", |ui| {
                    if !detail.command.is_empty() {
                        kv_mono(ui, pal, "Command", &detail.command);
                    }
                    if !detail.working_dir.is_empty() {
                        kv_mono(ui, pal, "Working dir", &detail.working_dir);
                    }
                    if !detail.user.is_empty() {
                        kv(ui, pal, "User", &detail.user);
                    }
                });

                collapsing_section(ui, pal, &mut collapsed, "ports", "Ports", |ui| {
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
                });

                collapsing_section(ui, pal, &mut collapsed, "networks", "Networks", |ui| {
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
                });

                collapsing_section(ui, pal, &mut collapsed, "mounts", "Mounts", |ui| {
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
                });

                if !detail.env.is_empty() {
                    let title = format!("Environment ({})", detail.env.len());
                    collapsing_section(ui, pal, &mut collapsed, "env", &title, |ui| {
                        for (k, v) in &detail.env {
                            mono_line(ui, pal, &format!("{k}={v}"));
                        }
                    });
                }

                if !detail.labels.is_empty() {
                    let title = format!("Labels ({})", detail.labels.len());
                    collapsing_section(ui, pal, &mut collapsed, "labels", &title, |ui| {
                        for (k, v) in &detail.labels {
                            mono_line(ui, pal, &format!("{k} = {v}"));
                        }
                    });
                }

                ui.add_space(24.0);
            });

        self.ov_collapsed = collapsed;
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
        self.logs.sync_matcher();

        // Row 1 — filter and its regex switch on the left, buffer actions right.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let hint = if self.logs.use_regex {
                "Filter (regex)"
            } else {
                "Filter"
            };
            ui.add(
                egui::TextEdit::singleline(&mut self.logs.filter)
                    .hint_text(hint)
                    .desired_width(196.0),
            );
            if icons::toggle_text_button(ui, pal, ".*", self.logs.use_regex, "Match as a regex")
                .clicked()
            {
                self.logs.use_regex = !self.logs.use_regex;
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if icons::icon_button(ui, pal, Icon::Download, None, "Export visible lines")
                    .clicked()
                {
                    out.export = Some(self.log_export());
                }
                if icons::icon_button(ui, pal, Icon::Copy, None, "Copy visible lines").clicked() {
                    ui.ctx().copy_text(self.log_export().body);
                }
                if icons::icon_button(ui, pal, Icon::Close, None, "Clear the buffer").clicked() {
                    self.logs.lines.clear();
                    self.logs.trimmed = 0;
                }
            });
        });

        if let Err(err) = &self.logs.matcher {
            ui.add_space(3.0);
            ui.label(
                RichText::new(format!("Invalid regex: {err}"))
                    .small()
                    .color(pal.unhealthy),
            );
        }
        ui.add_space(6.0);

        // Row 2 — tail size on the left, view toggles right.
        ui.horizontal(|ui| {
            let idx = tail_to_index(self.logs.tail);
            if let Some(new_idx) =
                widgets::segmented(ui, pal, "log-tail", &["100", "1k", "10k", "All"], idx)
            {
                self.logs.tail = index_to_tail(new_idx);
                self.restart_logs(out);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if icons::icon_button(ui, pal, Icon::JumpDown, None, "Jump to newest").clicked() {
                    self.logs.follow = true;
                }
                if icons::toggle_icon_button(ui, pal, Icon::Pulse, self.logs.follow, "Follow tail")
                    .clicked()
                {
                    self.logs.follow = !self.logs.follow;
                }
                if icons::toggle_icon_button(ui, pal, Icon::Lines, self.logs.wrap, "Wrap lines")
                    .clicked()
                {
                    self.logs.wrap = !self.logs.wrap;
                }
                if icons::toggle_icon_button(
                    ui,
                    pal,
                    Icon::Clock,
                    self.logs.timestamps,
                    "Show timestamps",
                )
                .clicked()
                {
                    self.logs.timestamps = !self.logs.timestamps;
                }
            });
        });
        ui.add_space(8.0);

        // Borrow only the individual fields the render closure needs, so it can
        // still flip `self.logs.follow` when the user scrolls off the bottom.
        let matcher: Option<&Regex> = match &self.logs.matcher {
            Ok(Some(re)) => Some(re),
            _ => None,
        };
        let show_ts = self.logs.timestamps;
        let wrap = self.logs.wrap;
        let trimmed = self.logs.trimmed;
        let matching: Vec<&LogLine> = self
            .logs
            .lines
            .iter()
            .filter(|l| matcher.is_none_or(|re| re.is_match(&l.text)))
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
                    ui.spacing_mut().item_spacing.y = 1.0;
                    if trimmed > 0 {
                        ui.label(
                            RichText::new(format!(
                                "\u{2014} {trimmed} earlier line{} trimmed \u{2014}",
                                if trimmed == 1 { "" } else { "s" }
                            ))
                            .monospace()
                            .size(11.0)
                            .color(pal.text_faint),
                        );
                    }
                    let max_w = if wrap {
                        ui.available_width()
                    } else {
                        f32::INFINITY
                    };
                    for line in &matching {
                        let mut job = log_line_job(pal, line, matcher, show_ts);
                        job.wrap.max_width = max_w;
                        ui.add(egui::Label::new(job));
                    }
                    if matching.is_empty() {
                        ui.label(
                            RichText::new("No lines match the filter.")
                                .monospace()
                                .size(11.0)
                                .color(pal.text_faint),
                        );
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
                    self.restart_logs(out);
                }
            });
        }
    }

    /// The currently-visible (filtered) lines as plain text — timestamps
    /// prefixed when the toggle is on. Backs both "Copy" and "Export".
    fn log_export(&self) -> LogExport {
        let mut body = String::new();
        for line in self.logs.lines.iter().filter(|l| self.logs.line_matches(l)) {
            if self.logs.timestamps {
                if let Some(ts) = &line.ts {
                    body.push_str(ts);
                    body.push(' ');
                }
            }
            body.push_str(&line.text);
            body.push('\n');
        }
        LogExport {
            container: self.name.clone(),
            body,
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

        if samples.len() >= 2 {
            ui.add_space(style::MD + 2.0);
            history_plot(ui, pal, &self.id.0, &samples);
        }

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
                    RichText::new("Open an interactive session")
                        .size(14.0)
                        .strong()
                        .color(pal.text),
                );
                ui.add_space(4.0);
                if self.state().is_active() {
                    let mode_idx = match self.term.mode {
                        SessionMode::Shell => 0,
                        SessionMode::Attach => 1,
                    };
                    if let Some(i) =
                        widgets::segmented(ui, pal, "term-mode", &["Shell", "Attach"], mode_idx)
                    {
                        self.term.mode = if i == 0 {
                            SessionMode::Shell
                        } else {
                            SessionMode::Attach
                        };
                    }
                    ui.add_space(6.0);
                    let blurb = match self.term.mode {
                        SessionMode::Shell => "A fresh /bin/sh (or bash) on a pseudo-TTY.",
                        SessionMode::Attach => {
                            "The main process's own stdio — Ctrl-C, Ctrl-D and \
                             resize reach it directly."
                        }
                    };
                    ui.label(RichText::new(blurb).color(pal.text_muted));
                    ui.add_space(14.0);
                    if icons::primary_button(ui, pal, "Start session").clicked() {
                        self.term.started = true;
                        self.term.ended = None;
                        out.commands.push(self.term.open_command(&self.id));
                    }
                } else {
                    ui.label(
                        RichText::new("The container must be running first.").color(pal.text_muted),
                    );
                }
            });

            if !self.exec_audit.is_empty() {
                ui.add_space(28.0);
                let inset = (ui.available_width() - 360.0).max(0.0) / 2.0;
                ui.horizontal(|ui| {
                    ui.add_space(inset);
                    ui.vertical(|ui| {
                        ui.set_width(360.0);
                        ui.label(
                            RichText::new("Recent sessions")
                                .small()
                                .strong()
                                .color(pal.text_muted),
                        );
                        ui.add_space(4.0);
                        for row in self.exec_audit.iter().take(6) {
                            session_row(ui, pal, row);
                        }
                    });
                });
            }
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
                    out.commands.push(self.term.open_command(&self.id));
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

/// A collapsible Overview section: the same quiet small-caps head as the old
/// static `section`, now fronted by a disclosure chevron that rotates with an
/// animated open/closed `t`, and a full-width hit target that takes a tonal
/// hover wash (no divider line) — matching `widgets::group_header`. `collapsed`
/// carries the folded-away keys, so the state lives on [`DetailScreen`] and
/// survives redraws and tab switches. The body is only laid out while open.
fn collapsing_section(
    ui: &mut egui::Ui,
    pal: &Palette,
    collapsed: &mut HashSet<&'static str>,
    key: &'static str,
    title: &str,
    body: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(18.0);
    let open = !collapsed.contains(key);

    let full_w = ui.available_width();
    let bg_idx = ui.painter().add(egui::Shape::Noop);
    let head = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(4, 3))
        .show(ui, |ui| {
            ui.set_width(full_w - 8.0);
            ui.horizontal(|ui| {
                let (chev, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                let open_t = ui
                    .ctx()
                    .animate_bool(ui.make_persistent_id(("ov-sec", key)), open);
                icons::chevron(ui.painter(), chev, pal.text_muted, open_t);
                ui.add_space(6.0);
                ui.label(RichText::new(title).small().strong().color(pal.text_muted));
            });
        });

    let rect = head.response.rect;
    let id = ui.make_persistent_id(("ov-sec-hit", key));
    let resp = ui.interact(rect, id, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let t = ui.ctx().animate_bool(id, resp.hovered());
    if t > 0.0 {
        ui.painter().set(
            bg_idx,
            egui::epaint::RectShape::new(
                rect,
                style::radius(pal.corner - 2.0),
                pal.tint(0.04 * t),
                Stroke::NONE,
                egui::StrokeKind::Inside,
            ),
        );
    }
    if resp.clicked() {
        if open {
            collapsed.insert(key);
        } else {
            collapsed.remove(key);
        }
    }

    ui.add_space(4.0);
    if open {
        body(ui);
    }
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

// ---- logs helpers -----------------------------------------------

/// Build the styled galley for one log line: an optional faint timestamp, then
/// the message with any filter matches lifted onto a faint accent ground.
/// stderr lines carry a warm tint, stdout a slightly dimmed foreground —
/// matching the old `RichText` styling, now as a `LayoutJob` so matched spans
/// can be recoloured inline.
fn log_line_job(
    pal: &Palette,
    line: &LogLine,
    matcher: Option<&Regex>,
    show_ts: bool,
) -> LayoutJob {
    let font = FontId::monospace(11.5);
    let base = match line.stream {
        LogStream::Stderr => pal.unhealthy.lerp_to_gamma(pal.term_fg, 0.25),
        LogStream::Stdout => pal.term_fg.gamma_multiply(0.92),
    };
    let plain = TextFormat {
        font_id: font.clone(),
        color: base,
        ..Default::default()
    };
    let hit = TextFormat {
        font_id: font.clone(),
        color: pal.text,
        background: pal.accent.gamma_multiply(0.22),
        ..Default::default()
    };

    let mut job = LayoutJob::default();
    if show_ts {
        if let Some(ts) = &line.ts {
            job.append(
                &format!("{}  ", format::log_time(ts)),
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: pal.text_faint,
                    ..Default::default()
                },
            );
        }
    }

    match matcher {
        Some(re) => {
            let mut last = 0usize;
            for m in re.find_iter(&line.text) {
                if m.start() > last {
                    job.append(&line.text[last..m.start()], 0.0, plain.clone());
                }
                if m.end() > m.start() {
                    job.append(&line.text[m.start()..m.end()], 0.0, hit.clone());
                }
                last = m.end().max(m.start());
            }
            if last < line.text.len() {
                job.append(&line.text[last..], 0.0, plain);
            } else if line.text.is_empty() {
                job.append(" ", 0.0, plain);
            }
        }
        None if line.text.is_empty() => job.append(" ", 0.0, plain),
        None => job.append(&line.text, 0.0, plain),
    }
    job
}

// ---- terminal helpers -------------------------------------------

/// One row of the Terminal tab's "Recent sessions" list: a quiet mark, a
/// relative time, then the duration and a non-zero exit code (tinted).
fn session_row(ui: &mut egui::Ui, pal: &Palette, row: &ExecAudit) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (ic, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
        icons::draw(ui.painter(), Icon::Terminal, ic, pal.text_faint);
        ui.label(
            RichText::new(format::ago(row.ts_ms))
                .small()
                .color(pal.text_muted),
        );
        if let Some(d) = row.duration_secs {
            ui.label(
                RichText::new(format!("{d}s"))
                    .small()
                    .monospace()
                    .color(pal.text_faint),
            );
        }
        match row.exit_code {
            Some(0) | None => {}
            Some(code) => {
                ui.label(
                    RichText::new(format!("exit {code}"))
                        .small()
                        .monospace()
                        .color(pal.unhealthy.lerp_to_gamma(pal.text, 0.15)),
                );
            }
        }
    });
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
        .stroke(Stroke::new(1.0_f32, pal.border))
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
            Stroke::new(1.0_f32, color.gamma_multiply(0.35)),
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
    painter.add(egui::Shape::line(line, Stroke::new(1.5_f32, color)));
}

/// Format a signed second offset from "now" as a short relative label
/// (`now`, `-45s`, `-8m`, `-2.5h`). `dt` is expected to be <= 0.
fn rel_time(dt: f64) -> String {
    let ago = -dt;
    if ago < 1.0 {
        "now".to_string()
    } else if ago < 90.0 {
        format!("-{ago:.0}s")
    } else if ago < 5400.0 {
        format!("-{:.0}m", ago / 60.0)
    } else {
        format!("-{:.1}h", ago / 3600.0)
    }
}

/// The Stats tab's history chart: CPU and memory as a percentage over the
/// retained window, on a shared 0–100 axis. `egui_plot` (PLAN §2) so the line
/// is pannable/zoomable on the x-axis and hovering reads out a value; styling
/// stays in the app's language — hairline grid on the levels only, a whisper of
/// tonal fill, quiet legend, no boxed-zoom rectangle.
fn history_plot(ui: &mut egui::Ui, pal: &Palette, container_id: &str, samples: &[StatSample]) {
    let timed = samples.iter().all(|s| s.ts_ms > 0);
    let x_of = |i: usize, s: &StatSample| {
        if timed {
            s.ts_ms as f64 / 1000.0
        } else {
            i as f64
        }
    };
    let latest_x = samples
        .last()
        .map(|s| x_of(samples.len() - 1, s))
        .unwrap_or(0.0);

    let has_mem_limit = samples.iter().any(|s| s.mem_limit > 0);
    let cpu: Vec<[f64; 2]> = samples
        .iter()
        .enumerate()
        .map(|(i, s)| [x_of(i, s), s.cpu_pct as f64])
        .collect();
    let mem: Vec<[f64; 2]> = samples
        .iter()
        .enumerate()
        .map(|(i, s)| [x_of(i, s), (s.mem_frac() * 100.0) as f64])
        .collect();

    let x_fmt = move |mark: egui_plot::GridMark, _r: &std::ops::RangeInclusive<f64>| {
        if timed {
            rel_time(mark.value - latest_x)
        } else {
            format!("{:.0}", mark.value)
        }
    };

    Plot::new(("stats-history", container_id))
        .height(184.0)
        .legend(
            Legend::default()
                .position(Corner::LeftTop)
                .background_alpha(0.0)
                .text_style(egui::TextStyle::Small),
        )
        .show_axes([true, true])
        .show_grid([false, true])
        .allow_zoom([true, false])
        .allow_drag([true, false])
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .set_margin_fraction(vec2(0.0, 0.12))
        .include_y(0.0)
        .include_y(100.0)
        .x_axis_formatter(x_fmt)
        .y_axis_formatter(|m, _| format!("{:.0}%", m.value))
        .label_formatter(move |name, p| {
            let when = if timed {
                rel_time(p.x - latest_x)
            } else {
                format!("#{:.0}", p.x)
            };
            if name.is_empty() {
                format!("{when}\n{:.1}%", p.y)
            } else {
                format!("{name}\n{when} · {:.1}%", p.y)
            }
        })
        .show(ui, |plot_ui| {
            plot_ui.line(
                Line::new("CPU %", PlotPoints::from(cpu))
                    .color(pal.accent)
                    .width(1.5)
                    .fill(0.0)
                    .fill_alpha(0.05),
            );
            if has_mem_limit {
                plot_ui.line(
                    Line::new("Mem %", PlotPoints::from(mem))
                        .color(pal.running)
                        .width(1.5)
                        .fill(0.0)
                        .fill_alpha(0.05),
                );
            }
        });
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
            labels: vec![("com.docker.compose.project".into(), "example".into())],
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
                    ts: Some(format!("2026-09-10T14:{:02}:00.000000Z", i % 60)),
                    text: format!("line {i}: listening on :80"),
                },
                LogLine {
                    stream: LogStream::Stderr,
                    ts: None,
                    text: format!("line {i}: warning: slow query"),
                },
            ]);
            screen.on_stat(StatSample {
                ts_ms: 1_757_512_000_000 + i * 1_000,
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

    /// Overview lays out cleanly with every section folded away — exercising
    /// the collapsed branch of `collapsing_section`, where the bodies (and
    /// their `copy_row` borrows of `&mut self`) are skipped entirely.
    #[test]
    fn overview_with_all_sections_collapsed_lays_out() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());
        let container = fake_container("beadfeed0002");
        let mut screen = DetailScreen::new(&container);
        screen.on_inspected(fake_detail(&container.id));
        screen.tab = Tab::Overview;
        for key in [
            "status", "image", "command", "ports", "networks", "mounts", "env", "labels",
        ] {
            screen.ov_collapsed.insert(key);
        }

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(700.0, 600.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = screen.ui(ui, &pal);
            });
        });
        assert_eq!(
            screen.ov_collapsed.len(),
            8,
            "no click, so the set is unchanged"
        );
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
                .any(|c| matches!(c, Command::OpenExec(_) | Command::OpenAttach(_))),
            "the terminal must wait for an explicit Start click"
        );
    }

    #[test]
    fn session_mode_picks_the_open_command() {
        let container = fake_container("cafefeed0002");
        let mut screen = DetailScreen::new(&container);
        assert!(matches!(
            screen.term.open_command(&container.id),
            Command::OpenExec(_)
        ));
        screen.term.mode = SessionMode::Attach;
        assert!(matches!(
            screen.term.open_command(&container.id),
            Command::OpenAttach(_)
        ));
    }

    #[test]
    fn stat_history_merges_dedups_and_caps() {
        let container = fake_container("aa11bb22cc33");
        let mut screen = DetailScreen::new(&container);
        let s = |ts: u64| StatSample {
            ts_ms: ts,
            cpu_pct: ts as f32,
            cpu_cores: 1.0,
            mem_used: 1,
            mem_limit: 2,
            net_rx: 0,
            net_tx: 0,
            blk_read: 0,
            blk_write: 0,
            pids: 1,
        };
        // Live stream has delivered two samples already.
        screen.on_stat(s(3_000));
        screen.on_stat(s(4_000));
        // History overlaps one of them and adds older points.
        screen.on_stat_history(vec![s(1_000), s(2_000), s(3_000)]);

        let got: Vec<u64> = screen.stats.samples.iter().map(|x| x.ts_ms).collect();
        assert_eq!(got, vec![1_000, 2_000, 3_000, 4_000], "sorted, no dupes");

        // An empty history answer is a no-op.
        screen.on_stat_history(vec![]);
        assert_eq!(screen.stats.samples.len(), 4);
    }

    #[test]
    fn exec_audit_is_filtered_to_this_container() {
        let container = fake_container("dead00beef11");
        let mut screen = DetailScreen::new(&container);
        let row = |cid: &str| ExecAudit {
            ts_ms: 1_000,
            connection_id: "local".into(),
            container: cid.into(),
            container_name: "n".into(),
            argv: vec!["/bin/sh".into()],
            exit_code: Some(0),
            duration_secs: Some(5),
        };
        screen.on_exec_audit(vec![row("dead00beef11"), row("other"), row("dead00beef11")]);
        assert_eq!(screen.exec_audit.len(), 2);
    }

    #[test]
    fn tail_index_round_trips() {
        for t in [
            LogTail::Lines(100),
            LogTail::Lines(1_000),
            LogTail::Lines(10_000),
            LogTail::All,
        ] {
            assert_eq!(index_to_tail(tail_to_index(t)), t);
        }
        // The default (400) has no cell of its own; it snaps to "1k".
        assert_eq!(tail_to_index(LogTail::default()), 1);
    }

    #[test]
    fn logs_regex_filter_drives_export_and_lays_out() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());
        let container = fake_container("f00dcafe0003");
        let mut screen = DetailScreen::new(&container);
        screen.tab = Tab::Logs;
        screen.on_log_lines(vec![
            LogLine {
                stream: LogStream::Stdout,
                ts: Some("2026-09-10T14:03:11.000000Z".into()),
                text: "GET /health 200".into(),
            },
            LogLine {
                stream: LogStream::Stderr,
                ts: Some("2026-09-10T14:03:12.000000Z".into()),
                text: "GET /login 500".into(),
            },
        ]);
        screen.logs.use_regex = true;
        screen.logs.filter = r"\s5\d\d$".into();
        screen.logs.timestamps = true;
        screen.logs.sync_matcher();

        // Only the 5xx line survives the regex, and the export carries its
        // timestamp because the toggle is on.
        let export = screen.log_export();
        assert!(export.body.contains("GET /login 500"));
        assert!(!export.body.contains("/health"));
        assert!(export.body.contains("2026-09-10T14:03:12"));

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(480.0, 520.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = screen.ui(ui, &pal);
            });
        });

        // A broken pattern is surfaced as an error and fails open (every line
        // shows) rather than hiding output or panicking.
        screen.logs.filter = "(unclosed".into();
        screen.logs.sync_matcher();
        assert!(screen.logs.matcher.is_err());
        assert_eq!(screen.log_export().body.lines().count(), 2);
    }
}
