//! The Groups view (PLAN §5.1: manual + rule groups).
//!
//! One centered column of group cards. Each card is always editable — a colour
//! swatch row, a name field, and the kind-specific selector (a member checklist
//! for a manual group, name/image/label globs for a rule group). Edits are
//! handed straight back to the caller, which persists them to the TOML config;
//! the container list re-resolves from `config.groups` every frame, so changes
//! show immediately.

use egui::{vec2, Align, Layout, RichText, Sense, Stroke};
use rocker_core::{Container, Group, GroupId, GroupKind, GroupRule};

use crate::icons::{self, Icon};
use crate::style::{self, Palette};
use crate::widgets::segmented;

const COLUMN_W: f32 = 560.0;

/// Muted, earthy swatches — deliberately not the blue-purple default. Stored on
/// a group as a `#rrggbb` string.
const SWATCHES: [&str; 6] = [
    "#4a9e8f", "#7fa650", "#c98a3c", "#b9603f", "#7a8290", "#8a6d8f",
];

fn hex_color(s: &str) -> egui::Color32 {
    let h = s.strip_prefix('#').unwrap_or(s);
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("88"), 16).unwrap_or(0x88);
    if h.len() == 6 {
        egui::Color32::from_rgb(byte(0), byte(2), byte(4))
    } else {
        egui::Color32::from_gray(0x88)
    }
}

/// Render the Groups screen. Returns `true` if `groups` changed this frame.
pub fn groups_screen(
    ui: &mut egui::Ui,
    pal: &Palette,
    groups: &mut Vec<Group>,
    containers: &[Container],
) -> bool {
    let mut changed = false;
    let mut delete: Option<usize> = None;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let full = ui.available_width();
            let col = COLUMN_W.min(full - 4.0);
            let side = ((full - col) * 0.5).max(0.0);

            ui.horizontal(|ui| {
                ui.add_space(side);
                ui.vertical(|ui| {
                    ui.set_width(col);
                    ui.add_space(6.0);
                    ui.label(RichText::new("Groups").size(19.0).strong().color(pal.text));
                    ui.add_space(3.0);
                    ui.label(
                        RichText::new(
                            "Your groups sort the container list first; Compose \
                             projects fill in the rest.",
                        )
                        .small()
                        .color(pal.text_muted),
                    );

                    ui.add_space(16.0);
                    if new_group_row(ui, pal, groups) {
                        changed = true;
                    }

                    for (gi, g) in groups.iter_mut().enumerate() {
                        ui.add_space(10.0);
                        if group_card(ui, pal, g, containers, &mut delete, gi) {
                            changed = true;
                        }
                    }

                    if groups.is_empty() {
                        ui.add_space(28.0);
                        ui.label(RichText::new("No groups yet.").color(pal.text_faint));
                    }
                    ui.add_space(30.0);
                });
            });
        });

    if let Some(i) = delete {
        if i < groups.len() {
            groups.remove(i);
            changed = true;
        }
    }
    changed
}

/// The "add a group" control: a Manual/Rule segmented plus an Add button.
fn new_group_row(ui: &mut egui::Ui, pal: &Palette, groups: &mut Vec<Group>) -> bool {
    let mut added = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let salt = ui.make_persistent_id("new-group-kind");
        let idx = ui.data(|d| d.get_temp::<usize>(salt)).unwrap_or(0).min(1);
        if let Some(next) = segmented(ui, pal, "new-group-kind", &["Manual", "Rule"], idx) {
            ui.data_mut(|d| d.insert_temp(salt, next));
        }
        let idx = ui.data(|d| d.get_temp::<usize>(salt)).unwrap_or(0).min(1);
        if icons::primary_button(ui, pal, "Add group").clicked() {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let color = SWATCHES[groups.len() % SWATCHES.len()].to_string();
            let kind = if idx == 0 {
                GroupKind::Manual {
                    member_keys: Vec::new(),
                }
            } else {
                GroupKind::Rule {
                    rule: GroupRule::default(),
                }
            };
            groups.push(Group {
                id: GroupId::new(format!("g{ts}")),
                name: if idx == 0 { "New group" } else { "New rule" }.to_string(),
                color,
                icon: String::new(),
                kind,
            });
            added = true;
        }
    });
    added
}

/// One group's editable card. Pushes `gi` into `delete` if its trash is pressed.
fn group_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    g: &mut Group,
    containers: &[Container],
    delete: &mut Option<usize>,
    gi: usize,
) -> bool {
    let mut changed = false;
    egui::Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .corner_radius(style::radius(pal.corner))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            // Row 1: swatches · name · kind tag · delete.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                for hex in SWATCHES {
                    if swatch(ui, hex, g.color == hex) {
                        g.color = hex.to_string();
                        changed = true;
                    }
                }
                ui.add_space(4.0);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut g.name)
                        .hint_text("Name")
                        .desired_width(150.0),
                );
                if resp.changed() {
                    changed = true;
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icons::icon_button(ui, pal, Icon::Trash, None, "Delete group").clicked() {
                        *delete = Some(gi);
                    }
                    let tag = match g.kind {
                        GroupKind::Manual { .. } => "manual",
                        GroupKind::Rule { .. } => "rule",
                    };
                    ui.label(RichText::new(tag).small().color(pal.text_faint));
                });
            });

            ui.add_space(8.0);

            match &mut g.kind {
                GroupKind::Manual { member_keys } => {
                    if manual_members(ui, pal, member_keys, containers) {
                        changed = true;
                    }
                }
                GroupKind::Rule { rule } => {
                    if rule_fields(ui, pal, rule) {
                        changed = true;
                    }
                }
            }

            // Live match count.
            let n = containers.iter().filter(|c| g.contains(c)).count();
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "matches {n} container{}",
                    if n == 1 { "" } else { "s" }
                ))
                .small()
                .color(pal.text_faint),
            );
        });
    changed
}

/// A member checklist for a manual group, keyed by [`Group::member_key`] so a
/// recreated container keeps its membership.
fn manual_members(
    ui: &mut egui::Ui,
    pal: &Palette,
    member_keys: &mut Vec<String>,
    containers: &[Container],
) -> bool {
    let mut changed = false;
    if containers.is_empty() {
        ui.label(
            RichText::new("No containers to add.")
                .small()
                .color(pal.text_faint),
        );
        return false;
    }
    egui::ScrollArea::vertical()
        .id_salt("members")
        .max_height(168.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for c in containers {
                let key = Group::member_key(c);
                let on = member_keys.iter().any(|k| k == &key);
                if let Some(next) = check(ui, pal, &c.name, on) {
                    if next {
                        if !on {
                            member_keys.push(key);
                        }
                    } else {
                        member_keys.retain(|k| k != &key);
                    }
                    changed = true;
                }
            }
        });
    changed
}

/// Name / image / label selectors for a rule group.
fn rule_fields(ui: &mut egui::Ui, pal: &Palette, rule: &mut GroupRule) -> bool {
    let mut changed = false;

    let mut glob_row = |ui: &mut egui::Ui, label: &str, field: &mut Option<String>| {
        ui.horizontal(|ui| {
            ui.add_sized(
                vec2(84.0, 20.0),
                egui::Label::new(RichText::new(label).small().color(pal.text_muted)),
            );
            let mut text = field.clone().unwrap_or_default();
            let resp = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .hint_text("* ? wildcards")
                    .desired_width(220.0),
            );
            if resp.changed() {
                *field = if text.is_empty() { None } else { Some(text) };
                changed = true;
            }
        });
    };
    glob_row(ui, "Name", &mut rule.name_glob);
    ui.add_space(4.0);
    glob_row(ui, "Image", &mut rule.image_glob);
    ui.add_space(4.0);

    // Labels as a single "k=v, k2=v2" line (k alone matches any value).
    ui.horizontal(|ui| {
        ui.add_sized(
            vec2(84.0, 20.0),
            egui::Label::new(RichText::new("Labels").small().color(pal.text_muted)),
        );
        let mut text = labels_to_str(&rule.labels);
        let resp = ui.add(
            egui::TextEdit::singleline(&mut text)
                .hint_text("tier=backend, owned")
                .desired_width(220.0),
        );
        if resp.changed() {
            rule.labels = str_to_labels(&text);
            changed = true;
        }
    });

    changed
}

fn labels_to_str(labels: &[(String, String)]) -> String {
    labels
        .iter()
        .map(|(k, v)| {
            if v.is_empty() {
                k.clone()
            } else {
                format!("{k}={v}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn str_to_labels(s: &str) -> Vec<(String, String)> {
    s.split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            Some(match part.split_once('=') {
                Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
                None => (part.to_string(), String::new()),
            })
        })
        .collect()
}

/// A colour swatch: a filled disc, ringed when selected. Returns `true` on click.
fn swatch(ui: &mut egui::Ui, hex: &str, selected: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    let c = hex_color(hex);
    ui.painter().circle_filled(rect.center(), 6.0, c);
    if selected {
        ui.painter()
            .circle_stroke(rect.center(), 8.0, Stroke::new(1.5_f32, c));
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// A drawn checkbox row: a rounded box that fills with a tick when on, then the
/// label. No form-kit checkbox — same tonal language as the rest of the UI.
/// Returns the new value only when it flips.
fn check(ui: &mut egui::Ui, pal: &Palette, label: &str, on: bool) -> Option<bool> {
    let resp = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (b, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
            let t = ui
                .ctx()
                .animate_bool(ui.make_persistent_id(("chk", label)), on);
            ui.painter().rect(
                b.shrink(1.0),
                style::radius((pal.corner - 3.0).max(1.0)),
                pal.surface.lerp_to_gamma(pal.accent, 0.9 * t),
                Stroke::new(1.0_f32, pal.border_strong.lerp_to_gamma(pal.accent, t)),
                egui::StrokeKind::Inside,
            );
            if t > 0.0 {
                let c = b.center();
                ui.painter().add(egui::Shape::line(
                    vec![
                        egui::pos2(c.x - 3.0, c.y),
                        egui::pos2(c.x - 0.8, c.y + 2.4),
                        egui::pos2(c.x + 3.4, c.y - 2.8),
                    ],
                    Stroke::new(1.6_f32 * t, pal.on_accent),
                ));
            }
            ui.label(RichText::new(label).color(pal.text));
        })
        .response;

    let row = ui.interact(
        resp.rect,
        ui.make_persistent_id(("chk-hit", label)),
        Sense::click(),
    );
    if row.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    row.clicked().then_some(!on)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_string_round_trips() {
        let s = "tier=backend, owned, region = eu";
        let parsed = str_to_labels(s);
        assert_eq!(
            parsed,
            vec![
                ("tier".to_string(), "backend".to_string()),
                ("owned".to_string(), String::new()),
                ("region".to_string(), "eu".to_string()),
            ]
        );
        assert_eq!(labels_to_str(&parsed), "tier=backend, owned, region=eu");
    }

    #[test]
    fn screen_lays_out_without_panic() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());
        let containers: Vec<Container> = (0..4)
            .map(|i| Container {
                id: rocker_core::ContainerId::new(format!("c{i}")),
                name: format!("svc-{i}"),
                image: "img".into(),
                state: rocker_core::ContainerState::Running,
                status: "Up".into(),
                ports: vec![],
                compose_project: None,
                compose_service: None,
                labels: vec![],
            })
            .collect();

        for width in [420.0_f32, 720.0, 1200.0] {
            let mut groups = vec![
                Group {
                    id: GroupId::new("g1"),
                    name: "Manual".into(),
                    color: SWATCHES[0].into(),
                    icon: String::new(),
                    kind: GroupKind::Manual {
                        member_keys: vec!["name:svc-1".into()],
                    },
                },
                Group {
                    id: GroupId::new("g2"),
                    name: "Rule".into(),
                    color: SWATCHES[1].into(),
                    icon: String::new(),
                    kind: GroupKind::Rule {
                        rule: GroupRule {
                            name_glob: Some("svc-*".into()),
                            image_glob: None,
                            labels: vec![],
                        },
                    },
                },
            ];
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(width, 640.0),
                )),
                ..Default::default()
            };
            let mut changed = true;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    changed = groups_screen(ui, &pal, &mut groups, &containers);
                });
            });
            assert!(!changed, "no pointer input, so nothing should change");
            assert_eq!(groups.len(), 2);
        }
    }
}
