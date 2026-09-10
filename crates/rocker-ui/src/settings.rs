//! The Settings view (PLAN §5.4: "settings system on the same tokens").
//!
//! One centered column of quiet, labelled sections. Every control is drawn from
//! the same token set as the rest of the UI — a segmented control for the
//! theme, a clamped stepper for the numeric limits — and edits are handed
//! straight back to the caller, which persists them to the TOML config and
//! re-applies the theme in place. No form-kit widgets, no rules between rows.

use rocker_store::Settings;

use crate::style::{self, Palette};
use crate::widgets::{segmented, stepper};

/// Read-only facts shown in the "About" section.
pub struct About<'a> {
    pub app_version: &'a str,
    pub engine: Option<&'a str>,
    pub config_path: &'a str,
}

/// What changed this frame. `None` from [`settings_screen`] means nothing did.
/// `theme_changed` tells the caller to re-apply the palette; `autostart_changed`
/// tells it to reconcile the "open at login" entry on disk.
pub struct Edit {
    pub theme_changed: bool,
    pub autostart_changed: bool,
}

/// Fixed column width, so the screen holds one measured measure regardless of
/// window size.
const COLUMN_W: f32 = 552.0;
/// Width reserved for every row's control cluster, so controls share a right
/// edge down the whole screen no matter how long the label is (anti-slop:
/// ragged parallel columns).
const CONTROL_W: f32 = 232.0;
/// Every row is this tall, and both columns centre within it, so titles and
/// controls sit on one shared grid regardless of description length.
const ROW_H: f32 = 44.0;

const THEME_OPTS: [&str; 3] = ["System", "Light", "Dark"];

fn theme_index(id: &str) -> usize {
    match id {
        "light" => 1,
        "dark" => 2,
        _ => 0,
    }
}

fn theme_id(index: usize) -> &'static str {
    match index {
        1 => "light",
        2 => "dark",
        _ => "system",
    }
}

pub fn settings_screen(
    ui: &mut egui::Ui,
    pal: &Palette,
    settings: &mut Settings,
    about: About<'_>,
) -> Option<Edit> {
    let mut theme_changed = false;
    let mut autostart_changed = false;
    let mut other_changed = false;

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
                    ui.label(
                        egui::RichText::new("Settings")
                            .size(19.0)
                            .strong()
                            .color(pal.text),
                    );

                    section(ui, pal, "Appearance");
                    row(ui, pal, "Theme", "Match your OS, or pick one.", |ui| {
                        let current = theme_index(&settings.theme);
                        if let Some(next) = segmented(ui, pal, "theme", &THEME_OPTS, current) {
                            settings.theme = theme_id(next).to_string();
                            theme_changed = true;
                        }
                    });

                    section(ui, pal, "Usage history");
                    row(
                        ui,
                        pal,
                        "Retention",
                        "How long usage history is kept.",
                        |ui| {
                            let v = settings.stats_retention_hours as i64;
                            if let Some(next) = stepper(ui, pal, "retention", v, 12..=336, 12, "h")
                            {
                                settings.stats_retention_hours = next as u32;
                                other_changed = true;
                            }
                        },
                    );
                    row(
                        ui,
                        pal,
                        "Live graphs",
                        "Cap on containers streaming stats at once.",
                        |ui| {
                            let v = settings.max_stats_streams as i64;
                            if let Some(next) = stepper(ui, pal, "streams", v, 4..=64, 4, "") {
                                settings.max_stats_streams = next as usize;
                                other_changed = true;
                            }
                        },
                    );

                    section(ui, pal, "System");
                    row(
                        ui,
                        pal,
                        "Minimize to tray",
                        "Closing or minimizing hides Rocker to the tray.",
                        |ui| {
                            if let Some(next) =
                                toggle(ui, pal, "min-to-tray", settings.minimize_to_tray)
                            {
                                settings.minimize_to_tray = next;
                                other_changed = true;
                            }
                        },
                    );
                    row(
                        ui,
                        pal,
                        "Start hidden",
                        "Launch straight to the tray, no window.",
                        |ui| {
                            if let Some(next) =
                                toggle(ui, pal, "start-hidden", settings.start_minimized)
                            {
                                settings.start_minimized = next;
                                other_changed = true;
                            }
                        },
                    );
                    row(
                        ui,
                        pal,
                        "Open at login",
                        "Start Rocker automatically when you sign in.",
                        |ui| {
                            if let Some(next) =
                                toggle(ui, pal, "open-at-login", settings.open_at_login)
                            {
                                settings.open_at_login = next;
                                autostart_changed = true;
                            }
                        },
                    );

                    section(ui, pal, "About");
                    row(ui, pal, "Version", "The build you're running.", |ui| {
                        value(ui, pal, about.app_version)
                    });
                    row(
                        ui,
                        pal,
                        "Docker Engine",
                        "Version reported by the daemon.",
                        |ui| value(ui, pal, about.engine.unwrap_or("not connected")),
                    );

                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("Config file")
                            .small()
                            .strong()
                            .color(pal.text_muted),
                    );
                    ui.add_space(3.0);
                    ui.label(
                        egui::RichText::new(about.config_path)
                            .monospace()
                            .size(11.5)
                            .color(pal.text_muted),
                    );
                    ui.add_space(30.0);
                });
            });
        });

    (theme_changed || autostart_changed || other_changed).then_some(Edit {
        theme_changed,
        autostart_changed,
    })
}

/// An Off/On segmented control for a boolean row. Returns the new value only
/// when it actually flips.
fn toggle(ui: &mut egui::Ui, pal: &Palette, id_salt: &str, value: bool) -> Option<bool> {
    segmented(ui, pal, id_salt, &["Off", "On"], value as usize).map(|i| i == 1)
}

/// A quiet section label with air above and below. Not a heading — the rows
/// under it carry the weight, so it stays small and muted.
fn section(ui: &mut egui::Ui, pal: &Palette, label: &str) {
    ui.add_space(20.0);
    ui.label(
        egui::RichText::new(label)
            .small()
            .strong()
            .color(pal.text_muted),
    );
    ui.add_space(6.0);
}

/// One setting: title + one-line description on the left, a control cluster of
/// fixed width on the right. Both columns are pinned to a fixed width and
/// centred in a fixed-height row, so every title and every control lands on one
/// shared grid (anti-slop: ragged parallel columns). Rows are set off by space
/// alone, never a rule.
fn row(
    ui: &mut egui::Ui,
    pal: &Palette,
    title: &str,
    desc: &str,
    control: impl FnOnce(&mut egui::Ui),
) {
    let text_w = (ui.available_width() - CONTROL_W - style::MD).max(160.0);
    ui.horizontal(|ui| {
        ui.set_min_height(ROW_H);
        ui.allocate_ui_with_layout(
            egui::vec2(text_w, ROW_H),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(text_w);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(egui::RichText::new(title).strong().color(pal.text));
                    ui.label(egui::RichText::new(desc).small().color(pal.text_muted));
                });
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(CONTROL_W, ROW_H),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.set_min_width(CONTROL_W);
                control(ui);
            },
        );
    });
}

/// A right-aligned read-only value: data, so it is set in mono.
fn value(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .monospace()
            .size(12.0)
            .color(pal.text_muted),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_id_and_index_round_trip() {
        for id in ["system", "light", "dark"] {
            assert_eq!(theme_id(theme_index(id)), id);
        }
        // An unknown / future file-stem id lands on the "System" cell rather
        // than dropping off the control.
        assert_eq!(theme_index("solarized"), 0);
        assert_eq!(theme_id(99), "system");
    }

    /// Lay the whole screen out headlessly at a few widths: catches layout-math
    /// panics (bad rects, negative sizes) and confirms a no-input frame reports
    /// no edit. Pointer-driven checks of the controls happen in the running app.
    #[test]
    fn screen_lays_out_without_panic() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());

        for width in [420.0_f32, 700.0, 1200.0] {
            let mut settings = rocker_store::Settings::default();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(width, 640.0),
                )),
                ..Default::default()
            };
            let mut edit = None;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    edit = settings_screen(
                        ui,
                        &pal,
                        &mut settings,
                        About {
                            app_version: "0.0.0",
                            engine: None,
                            config_path: "/tmp/rocker/config.toml",
                        },
                    );
                });
            });
            assert!(edit.is_none(), "no pointer input, so nothing should change");
            assert_eq!(settings.theme, rocker_store::Settings::default().theme);
        }
    }
}
