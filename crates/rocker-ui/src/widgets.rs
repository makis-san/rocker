//! Composed widgets: the drawn status indicator, the container row, the group
//! header, and the connection-status cluster. All status is drawn, never pulled
//! from an icon pack (PLAN §9).

use rocker_core::{Container, ContainerState};
use rocker_engine::LifecycleAction;

use crate::format;
use crate::icons::{self, Icon};
use crate::style::{self, Palette};

/// A drawn state indicator: a filled core for a live container, a hollow ring
/// for a stopped one, and a soft halo that breathes while the container is
/// mid-transition (restarting / removing). The pulse is the only always-running
/// motion, and only while something is actually transitioning.
pub fn state_indicator(ui: &mut egui::Ui, pal: &Palette, state: ContainerState) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let center = rect.center();
    let color = pal.state(state);
    let core = 4.0;

    // A halo only while something is genuinely mid-transition — never a resting
    // glow on every row.
    if matches!(state, ContainerState::Restarting | ContainerState::Removing) {
        let phase = ui.input(|i| i.time) as f32 * 2.6;
        let pulse = 0.5 - 0.5 * phase.cos();
        ui.painter().circle_filled(
            center,
            core + 2.0 + 2.0 * pulse,
            color.gamma_multiply(0.10 + 0.16 * pulse),
        );
        ui.ctx().request_repaint();
    }

    match state {
        // Hollow ring = not running.
        ContainerState::Created | ContainerState::Exited | ContainerState::Dead => {
            ui.painter()
                .circle_stroke(center, core, egui::Stroke::new(1.6, color));
        }
        ContainerState::Unknown => {
            ui.painter().circle_stroke(
                center,
                core,
                egui::Stroke::new(1.6, color.gamma_multiply(0.7)),
            );
        }
        // Filled = live.
        _ => {
            ui.painter().circle_filled(center, core, color);
        }
    }
}

/// Short, human status: keep Docker's own phrasing but trim the trailing
/// "X minutes ago" tail so the secondary line stays quiet.
fn status_phrase(container: &Container) -> String {
    let s = container.status.trim();
    if s.is_empty() {
        return container_state_word(container.state).to_string();
    }
    match s.split_once(" ago") {
        Some((head, _)) => head.trim().to_string(),
        None => s.to_string(),
    }
}

fn container_state_word(state: ContainerState) -> &'static str {
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

/// `:8080 -> 80` for the first published ports, `+N` for the rest. Empty until
/// the engine layer fills `ports` (PLAN §5.3).
fn ports_summary(container: &Container) -> Option<String> {
    let published: Vec<String> = container
        .ports
        .iter()
        .filter_map(|p| {
            p.host_port
                .map(|h| format!("{h}\u{2192}{}", p.container_port))
        })
        .collect();
    if published.is_empty() {
        return None;
    }
    let shown = &published[..published.len().min(2)];
    let mut out = shown.join(", ");
    if published.len() > shown.len() {
        out.push_str(&format!("  +{}", published.len() - shown.len()));
    }
    Some(out)
}

/// What a click on a container row asked for: open its screen, or run a
/// lifecycle action from the inline cluster.
pub enum RowOutcome {
    Open,
    Act(LifecycleAction),
}

/// One container row. Tonal card that lifts on hover by tone alone (no
/// translate, no shadow). A press on the inline cluster returns [`RowOutcome::Act`];
/// a press anywhere else on the row returns [`RowOutcome::Open`].
pub fn container_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    container: &Container,
) -> Option<RowOutcome> {
    let mut action = None;
    let mut actions_rect = egui::Rect::NOTHING;
    let id = ui.make_persistent_id(("row", &container.id.0));

    // Reserve the background shape now; fill it once we know the hover state,
    // so the tone is correct on the same frame (no one-frame lag).
    let bg_idx = ui.painter().add(egui::Shape::Noop);

    let inner = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(
            style::MD as i8,
            (style::SM + 2.0) as i8,
        ))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                state_indicator(ui, pal, container.state);
                ui.add_space(style::SM);

                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.label(
                        egui::RichText::new(&container.name)
                            .strong()
                            .color(pal.text),
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(
                            egui::RichText::new(&container.image)
                                .small()
                                .color(pal.text_muted),
                        );
                        dot_sep(ui, pal);
                        ui.label(
                            egui::RichText::new(status_phrase(container))
                                .small()
                                .color(pal.state(container.state)),
                        );
                        if let Some(ports) = ports_summary(container) {
                            dot_sep(ui, pal);
                            ui.label(
                                egui::RichText::new(ports)
                                    .small()
                                    .monospace()
                                    .color(pal.text_muted),
                            );
                        }
                    });
                });

                let cluster = ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.add_space(2.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(style::ACTION_W, style::ICON_BTN),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                if container.state.is_active() {
                                    if icons::icon_button(ui, pal, Icon::Restart, None, "Restart")
                                        .clicked()
                                    {
                                        action = Some(LifecycleAction::Restart);
                                    }
                                    if icons::icon_button(ui, pal, Icon::Stop, None, "Stop")
                                        .clicked()
                                    {
                                        action = Some(LifecycleAction::Stop);
                                    }
                                } else if icons::icon_button(
                                    ui,
                                    pal,
                                    Icon::Play,
                                    Some(pal.accent),
                                    "Start",
                                )
                                .clicked()
                                {
                                    action = Some(LifecycleAction::Start);
                                }
                            },
                        );
                    },
                );
                actions_rect = cluster.response.rect;
            });
        });

    let row_rect = inner.response.rect;
    // The "open on any other press" hit-zone must not overlap the action
    // cluster: egui breaks a tie between two perfectly-overlapping click
    // senses by picking whichever was registered last, which would always be
    // this outer interact (it's added after the buttons) and would swallow
    // every button click before it ever reaches Restart/Stop/Start.
    let open_rect = if actions_rect.is_finite() {
        egui::Rect::from_min_max(
            row_rect.min,
            egui::pos2(actions_rect.left().min(row_rect.max.x), row_rect.max.y),
        )
    } else {
        row_rect
    };
    let resp = ui
        .interact(open_rect, id, egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let t = ui.ctx().animate_bool(id, resp.hovered());

    ui.painter().set(
        bg_idx,
        egui::epaint::RectShape::new(
            row_rect,
            style::radius(pal.corner),
            pal.tint(0.045 * t),
            egui::Stroke::new(1.0, pal.border.lerp_to_gamma(pal.border_strong, t)),
            egui::StrokeKind::Inside,
        ),
    );

    // A cluster press wins; otherwise a press anywhere on the row opens it.
    match action {
        Some(a) => Some(RowOutcome::Act(a)),
        None if resp.clicked() => Some(RowOutcome::Open),
        None => None,
    }
}

/// A low, round-capped middle dot used to separate metadata on the secondary
/// line — a deliberate glyph, not a hairline rule.
fn dot_sep(ui: &mut egui::Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(3.0, 12.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 1.4, pal.text_faint);
}

/// What a click in a group header asked for: toggle the section open/closed,
/// or run one lifecycle action against every container in it.
pub enum GroupOutcome {
    Toggle,
    BulkAct(LifecycleAction),
}

/// A group's combined CPU/mem, summed from whichever of its running
/// containers currently have a live sample. `partial` marks a total that
/// isn't the true combined figure — some active member isn't streaming yet
/// (just subscribed) or is sitting outside the stats-stream cap — so the
/// header reads it as a floor rather than an exact sum.
pub struct GroupUsage {
    pub cpu_pct: f32,
    pub mem_used: u64,
    pub partial: bool,
}

/// Collapsible section head for a Compose project (or the ungrouped remainder):
/// a disclosure chevron that rotates with `open_t` (0 closed, 1 open), the group
/// icon, the name, a count, and — trailing, so every group's cluster lands on
/// the same right edge — the group-level bulk actions (start all / stop all /
/// delete all). The strip (minus the bulk cluster) is one hit target with a
/// tonal hover wash — no divider line. A bulk-button press wins over the
/// toggle, exactly like a row's inline action cluster.
///
/// `any_stopped` / `any_active` say whether "start all" / "stop all" would
/// actually do anything to this group right now; when one wouldn't, that
/// button renders disabled rather than looking live and silently no-op'ing
/// (anti-slop: dead controls). "Delete all" has no such state — it always
/// applies to a non-empty group.
///
/// `usage`, when there's at least one live sample to sum, renders the group's
/// combined CPU/mem right after the count — each figure led by a small line
/// icon (`Cpu` / `Memory`), then quiet monospace data in faint ink, never a
/// colored chip.
#[allow(clippy::too_many_arguments)]
pub fn group_header(
    ui: &mut egui::Ui,
    pal: &Palette,
    project: Option<&str>,
    count: usize,
    open_t: f32,
    any_stopped: bool,
    any_active: bool,
    usage: Option<GroupUsage>,
) -> Option<GroupOutcome> {
    let full_w = ui.available_width();
    let bg_idx = ui.painter().add(egui::Shape::Noop);
    let mut action = None;
    let mut actions_rect = egui::Rect::NOTHING;

    let inner = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(4, 4))
        .show(ui, |ui| {
            ui.set_width(full_w - 8.0);
            ui.horizontal(|ui| {
                let (chev, _) =
                    ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                icons::chevron(ui.painter(), chev, pal.text_muted, open_t);
                ui.add_space(5.0);
                let (icon, _) =
                    ui.allocate_exact_size(egui::vec2(13.0, 13.0), egui::Sense::hover());
                icons::draw(ui.painter(), Icon::Stack, icon, pal.text_faint);
                ui.add_space(7.0);
                ui.label(
                    egui::RichText::new(project.unwrap_or("Ungrouped"))
                        .small()
                        .strong()
                        .color(pal.text_muted),
                );
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(count.to_string())
                        .small()
                        .color(pal.text_faint),
                );

                if let Some(usage) = usage {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(6.0);
                    dot_sep(ui, pal);
                    ui.add_space(6.0);
                    let flag = if usage.partial { "~" } else { "" };

                    let (cpu_icon, _) =
                        ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    icons::draw(ui.painter(), Icon::Cpu, cpu_icon, pal.text_faint);
                    ui.add_space(3.0);
                    ui.label(
                        egui::RichText::new(format!("{flag}{:.1}%", usage.cpu_pct))
                            .small()
                            .monospace()
                            .color(pal.text_faint),
                    )
                    .on_hover_text("Combined CPU across the group's running containers");

                    ui.add_space(10.0);
                    let (mem_icon, _) =
                        ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    icons::draw(ui.painter(), Icon::Memory, mem_icon, pal.text_faint);
                    ui.add_space(3.0);
                    ui.label(
                        egui::RichText::new(format!("{flag}{}", format::bytes(usage.mem_used)))
                            .small()
                            .monospace()
                            .color(pal.text_faint),
                    )
                    .on_hover_text("Combined memory across the group's running containers");
                }

                let cluster = ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if icons::icon_button(ui, pal, Icon::Trash, None, "Delete all").clicked() {
                            action = Some(GroupOutcome::BulkAct(LifecycleAction::Remove));
                        }
                        if icons::icon_button_enabled(
                            ui,
                            pal,
                            Icon::Stop,
                            None,
                            "Stop all",
                            any_active,
                        )
                        .clicked()
                        {
                            action = Some(GroupOutcome::BulkAct(LifecycleAction::Stop));
                        }
                        if icons::icon_button_enabled(
                            ui,
                            pal,
                            Icon::Play,
                            Some(pal.accent),
                            "Start all",
                            any_stopped,
                        )
                        .clicked()
                        {
                            action = Some(GroupOutcome::BulkAct(LifecycleAction::Start));
                        }
                    },
                );
                actions_rect = cluster.response.rect;
            });
        });

    let rect = inner.response.rect;
    // Exclude the bulk-action cluster from the toggle hit-zone for the same
    // reason as `container_row`: an overlapping click sense registered after
    // the buttons would win every tie and swallow their clicks.
    let toggle_rect = if actions_rect.is_finite() {
        egui::Rect::from_min_max(
            rect.min,
            egui::pos2(actions_rect.left().min(rect.max.x), rect.max.y),
        )
    } else {
        rect
    };
    let id = ui.make_persistent_id(("group-hit", project.unwrap_or("")));
    let resp = ui.interact(toggle_rect, id, egui::Sense::click());
    let t = ui.ctx().animate_bool(id, resp.hovered());
    if t > 0.0 {
        ui.painter().set(
            bg_idx,
            egui::epaint::RectShape::new(
                rect,
                style::radius(pal.corner - 2.0),
                pal.tint(0.04 * t),
                egui::Stroke::NONE,
                egui::StrokeKind::Inside,
            ),
        );
    }
    ui.add_space(6.0);

    // A bulk-button press wins; otherwise a press anywhere else on the strip
    // toggles the section, mirroring `container_row`'s cluster-vs-row rule.
    match action {
        Some(a) => Some(a),
        None if resp.clicked() => Some(GroupOutcome::Toggle),
        None => None,
    }
}

/// A centered confirmation for a destructive bulk action: a dim scrim over the
/// whole window (also the "click outside to cancel" target) and a small tonal
/// card with a title, a detail line, and a status-tinted confirm button beside
/// a quiet "Cancel". Returns `Some(true)` on confirm, `Some(false)` on cancel
/// or an outside click, `None` while the choice is still pending.
pub fn confirm_dialog(
    ctx: &egui::Context,
    pal: &Palette,
    title: &str,
    detail: &str,
    confirm_label: &str,
) -> Option<bool> {
    let screen = ctx.content_rect();
    let mut result = None;

    let scrim = egui::Area::new(egui::Id::new("confirm-scrim"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(120));
            resp
        })
        .inner;
    if scrim.clicked() {
        result = Some(false);
    }

    egui::Area::new(egui::Id::new("confirm-card"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(pal.surface)
                .stroke(egui::Stroke::new(1.0, pal.border_strong))
                .corner_radius(style::radius(pal.corner))
                .inner_margin(egui::Margin::same(18))
                .show(ui, |ui| {
                    ui.set_width(300.0);
                    ui.label(
                        egui::RichText::new(title)
                            .size(14.5)
                            .strong()
                            .color(pal.text),
                    );
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(detail).small().color(pal.text_muted));
                    ui.add_space(16.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::filled_button(ui, pal, confirm_label, pal.unhealthy).clicked() {
                            result = Some(true);
                        }
                        ui.add_space(6.0);
                        if icons::text_button(ui, pal, "Cancel").clicked() {
                            result = Some(false);
                        }
                    });
                });
        });

    result
}

/// A segmented control: one rounded track split into equal cells, the selected
/// cell carrying a tonal thumb that slides between positions (animated, no
/// fade-from-nothing). Returns the new index when a different cell is clicked —
/// every cell is a live hit target.
pub fn segmented(
    ui: &mut egui::Ui,
    pal: &Palette,
    id_salt: &str,
    options: &[&str],
    selected: usize,
) -> Option<usize> {
    let n = options.len().max(1);
    let h = 28.0;
    let w = ui.available_width().min(232.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());

    ui.painter().rect(
        rect,
        style::radius(pal.corner),
        pal.surface,
        egui::Stroke::new(1.0, pal.border),
        egui::StrokeKind::Inside,
    );

    let cell_w = rect.width() / n as f32;
    let sel = selected.min(n - 1);
    let thumb_t = ui.ctx().animate_value_with_time(
        ui.make_persistent_id((id_salt, "seg-thumb")),
        sel as f32,
        0.12,
    );

    let thumb = egui::Rect::from_min_size(
        rect.left_top() + egui::vec2(thumb_t * cell_w, 0.0),
        egui::vec2(cell_w, h),
    )
    .shrink(3.0);
    ui.painter().rect(
        thumb,
        style::radius((pal.corner - 2.0).max(1.0)),
        pal.tint(0.10),
        egui::Stroke::new(1.0, pal.border_strong),
        egui::StrokeKind::Inside,
    );

    let mut out = None;
    for (i, label) in options.iter().enumerate() {
        let cell = egui::Rect::from_min_size(
            rect.left_top() + egui::vec2(i as f32 * cell_w, 0.0),
            egui::vec2(cell_w, h),
        );
        let resp = ui.interact(
            cell,
            ui.make_persistent_id((id_salt, "seg-cell", i)),
            egui::Sense::click(),
        );
        let hot = ui.ctx().animate_bool(resp.id, resp.hovered());
        let color = if i == sel {
            pal.text
        } else {
            pal.text_muted.lerp_to_gamma(pal.text, 0.35 * hot)
        };
        ui.painter().text(
            cell.center(),
            egui::Align2::CENTER_CENTER,
            *label,
            egui::FontId::proportional(12.5),
            color,
        );
        if resp.clicked() && i != sel {
            out = Some(i);
        }
    }
    out
}

/// A clamped numeric stepper: `[−]  value unit  [+]` on one rounded track. The
/// button at a reached bound is drawn dimmed and stops responding, so there is
/// no dead control. Returns the new value when it changes.
pub fn stepper(
    ui: &mut egui::Ui,
    pal: &Palette,
    id_salt: &str,
    value: i64,
    range: std::ops::RangeInclusive<i64>,
    step: i64,
    unit: &str,
) -> Option<i64> {
    let (min, max) = (*range.start(), *range.end());
    let h = 28.0;
    let btn = 30.0;
    let mid = 64.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(btn * 2.0 + mid, h), egui::Sense::hover());

    ui.painter().rect(
        rect,
        style::radius(pal.corner),
        pal.surface,
        egui::Stroke::new(1.0, pal.border),
        egui::StrokeKind::Inside,
    );

    let minus = egui::Rect::from_min_size(rect.left_top(), egui::vec2(btn, h));
    let plus =
        egui::Rect::from_min_size(rect.right_top() - egui::vec2(btn, 0.0), egui::vec2(btn, h));
    let mid_rect =
        egui::Rect::from_min_size(rect.left_top() + egui::vec2(btn, 0.0), egui::vec2(mid, h));

    let mut out = None;
    if step_btn(ui, pal, minus, Icon::Minus, id_salt, "dec", value > min) {
        out = Some((value - step).max(min));
    }
    if step_btn(ui, pal, plus, Icon::Plus, id_salt, "inc", value < max) {
        out = Some((value + step).min(max));
    }

    let text = if unit.is_empty() {
        value.to_string()
    } else {
        format!("{value} {unit}")
    };
    ui.painter().text(
        mid_rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::monospace(12.0),
        pal.text,
    );

    out
}

fn step_btn(
    ui: &mut egui::Ui,
    pal: &Palette,
    rect: egui::Rect,
    icon: Icon,
    id_salt: &str,
    part: &str,
    enabled: bool,
) -> bool {
    let resp = ui.interact(
        rect,
        ui.make_persistent_id((id_salt, "step", part)),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let t = if enabled {
        ui.ctx().animate_bool(resp.id, resp.hovered())
    } else {
        0.0
    };
    if t > 0.0 {
        ui.painter().rect_filled(
            rect.shrink(3.0),
            style::radius((pal.corner - 3.0).max(1.0)),
            pal.tint(0.10 * t),
        );
    }
    let color = if enabled {
        pal.text_muted.lerp_to_gamma(pal.text, 0.35 + 0.5 * t)
    } else {
        pal.text_faint
    };
    icons::draw(ui.painter(), icon, rect.shrink(8.0), color);
    enabled && resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lays the group header out at a few widths with no pointer input: catches
    /// layout-math panics from the added bulk-action cluster, and confirms an
    /// untouched header reports no outcome (pointer-driven checks happen in the
    /// running app, same convention as `settings::tests`).
    #[test]
    fn group_header_lays_out_without_panic() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());

        for width in [220.0_f32, 420.0, 900.0] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(width, 200.0),
                )),
                ..Default::default()
            };
            let mut outcome_was_none = false;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let outcome = group_header(ui, &pal, Some("demo"), 3, 1.0, true, true, None);
                    outcome_was_none = outcome.is_none();
                });
            });
            assert!(outcome_was_none, "no pointer input, so nothing should fire");
        }
    }

    /// The combined CPU/mem total is optional and must not upset the layout
    /// (or the group hit-zone) when it's present, partial flag included.
    #[test]
    fn group_header_with_usage_lays_out_without_panic() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());

        for width in [220.0_f32, 420.0, 900.0] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(width, 200.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    group_header(
                        ui,
                        &pal,
                        Some("demo"),
                        3,
                        1.0,
                        true,
                        true,
                        Some(GroupUsage {
                            cpu_pct: 234.5,
                            mem_used: 1_073_741_824,
                            partial: true,
                        }),
                    );
                });
            });
        }
    }

    /// Same no-panic guarantee for the destructive confirm overlay, at a
    /// couple of window sizes, plus: with no pointer input it must not report
    /// a decision either way (a stray click can't slip past `None`).
    #[test]
    fn confirm_dialog_lays_out_without_panic() {
        for (w, h) in [(600.0_f32, 400.0), (1040.0, 720.0)] {
            let ctx = egui::Context::default();
            let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());
            let mut result = None;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(w, h),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    result = confirm_dialog(ctx, &pal, "Delete 3 containers?", "Detail.", "Delete");
                },
            );
            assert!(result.is_none(), "no pointer input, so no decision yet");
        }
    }
}
