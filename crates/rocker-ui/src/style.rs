//! The resolved design system: [`Palette`] (theme tokens turned into concrete
//! `egui` colors, plus the derived tones the widgets need) and [`install`],
//! which maps the whole thing onto `egui::Style`.
//!
//! Phase 1 (PLAN §5.4) grows this into the full token schema and adds a
//! self-hosted display font via `FontDefinitions`. Until then the type scale is
//! tuned here and depth comes entirely from tone, never a drop shadow.

use egui::{Color32, CornerRadius, FontId, Margin, Stroke};
use rocker_core::ContainerState;
use rocker_theme::{Hex, Mode, Theme};

/// Spacing steps. One rhythm, so nothing looks shoved.
pub const SM: f32 = 8.0;
pub const MD: f32 = 12.0;

/// A square icon button's outer box.
pub const ICON_BTN: f32 = 28.0;
/// The glyph drawn inside a small inline icon.
pub const ICON_SM: f32 = 15.0;
/// Reserved width for a row's action cluster, so clusters align across rows
/// regardless of how long the container name is (anti-slop: ragged parallel
/// columns).
pub const ACTION_W: f32 = 68.0;

fn c(h: &Hex) -> Color32 {
    match h.rgba() {
        Some((r, g, b, a)) => Color32::from_rgba_unmultiplied(r, g, b, a),
        None => Color32::from_rgb(128, 128, 128),
    }
}

/// The theme's 16-entry ANSI ramp, resolved to concrete colours. Falls back to
/// a mid grey for a short or malformed palette so the terminal never panics.
fn term_ansi(palette: &[Hex]) -> [Color32; 16] {
    std::array::from_fn(|i| palette.get(i).map(c).unwrap_or(Color32::from_gray(150)))
}

/// Terminal substrate: on a dark theme, the palette's own darkest colour (index
/// 0); on a light theme, the app background, so the shell isn't a black slab in
/// a light window.
fn term_bg(palette: &[Hex], app_bg: Color32, dark: bool) -> Color32 {
    if dark {
        palette.first().map(c).unwrap_or(Color32::from_gray(18))
    } else {
        app_bg
    }
}

fn term_fg(palette: &[Hex], app_text: Color32, dark: bool) -> Color32 {
    if dark {
        palette.get(15).map(c).unwrap_or(Color32::from_gray(230))
    } else {
        app_text
    }
}

/// Round a token radius into an `egui` corner radius.
pub fn radius(r: f32) -> CornerRadius {
    CornerRadius::from(r)
}

/// Every color the UI draws with, resolved once per theme change. `Copy` so it
/// threads through widget calls without ceremony.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// App / panel base.
    pub bg: Color32,
    /// A card or row at rest.
    pub surface: Color32,
    /// Same, hovered.
    pub surface_hover: Color32,
    /// Same, pressed.
    pub surface_active: Color32,
    /// Hairline, mixed from the surface's own ink — a lip you sense, not a line.
    pub border: Color32,
    /// The hairline lifted for a hovered / focused edge.
    pub border_strong: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,
    /// Tonal accent (a quiet teal in the built-ins), never a saturated pop.
    pub accent: Color32,
    /// Ink that sits legibly on an accent fill.
    pub on_accent: Color32,
    pub running: Color32,
    pub paused: Color32,
    pub exited: Color32,
    pub unhealthy: Color32,
    /// Terminal substrate and default ink for the in-app shell.
    pub term_bg: Color32,
    pub term_fg: Color32,
    /// The 16 ANSI colours (0..=7 normal, 8..=15 bright) from the theme.
    pub term_ansi: [Color32; 16],
    pub corner: f32,
    dark: bool,
}

impl Palette {
    pub fn from_theme(theme: &Theme) -> Self {
        let t = &theme.tokens;
        let dark = matches!(theme.mode, Mode::Dark | Mode::Either);
        let bg = c(&t.surface);
        let surface = c(&t.surface_raised);
        let text = c(&t.text);

        Self {
            bg,
            surface,
            surface_hover: surface.lerp_to_gamma(text, 0.05),
            surface_active: surface.lerp_to_gamma(text, 0.10),
            border: surface.lerp_to_gamma(text, 0.11),
            border_strong: surface.lerp_to_gamma(text, 0.22),
            text,
            text_muted: c(&t.text_muted),
            text_faint: c(&t.text_muted).lerp_to_gamma(bg, 0.42),
            accent: c(&t.accent),
            on_accent: if dark {
                Color32::from_rgb(24, 23, 21)
            } else {
                Color32::from_rgb(250, 249, 245)
            },
            running: c(&t.status.running),
            paused: c(&t.status.paused),
            exited: c(&t.status.exited),
            unhealthy: c(&t.status.unhealthy),
            term_bg: term_bg(&t.terminal_palette, bg, dark),
            term_fg: term_fg(&t.terminal_palette, text, dark),
            term_ansi: term_ansi(&t.terminal_palette),
            corner: t.radius,
            dark,
        }
    }

    /// Color for a container state, used by the drawn status indicator and the
    /// status line.
    pub fn state(&self, state: ContainerState) -> Color32 {
        use ContainerState::*;
        match state {
            Running | Restarting => self.running,
            Paused => self.paused,
            Unknown => self.text_muted,
            Created | Removing | Exited | Dead => self.exited,
        }
    }

    /// Mix `frac` of the ink into a surface tone (0 = surface, 1 = ink).
    pub fn tint(&self, frac: f32) -> Color32 {
        self.surface.lerp_to_gamma(self.text, frac)
    }
}

/// Build the full `egui::Style` from a theme and return the resolved palette.
/// Called on startup and again whenever the theme changes (Phase 4 hot reload).
pub fn install(ctx: &egui::Context, theme: &Theme) -> Palette {
    let p = Palette::from_theme(theme);
    let mut style = (*ctx.style()).clone();

    // Type scale: tighter and flatter than egui's defaults, so the heading
    // leads without shouting and the secondary line stays quiet.
    use egui::{FontFamily::Monospace, FontFamily::Proportional, TextStyle};
    style.text_styles = [
        (TextStyle::Heading, FontId::new(17.0, Proportional)),
        (TextStyle::Body, FontId::new(13.0, Proportional)),
        (TextStyle::Button, FontId::new(13.0, Proportional)),
        (TextStyle::Small, FontId::new(11.5, Proportional)),
        (TextStyle::Monospace, FontId::new(12.0, Monospace)),
    ]
    .into();

    let v = &mut style.visuals;
    v.dark_mode = p.dark;
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.faint_bg_color = p.surface_hover;
    v.extreme_bg_color = p.bg.lerp_to_gamma(
        if p.dark {
            Color32::BLACK
        } else {
            Color32::WHITE
        },
        0.5,
    );
    v.override_text_color = None;
    v.hyperlink_color = p.accent;
    v.window_stroke = Stroke::new(1.0_f32, p.border);
    v.window_corner_radius = radius(p.corner);
    v.menu_corner_radius = radius(p.corner);
    v.selection.bg_fill = p.accent.gamma_multiply(0.30);
    v.selection.stroke = Stroke::new(1.0_f32, p.accent);

    // Depth is tone here, not a bloom. Kill the window shadow; keep the popup
    // shadow tight, low-offset and near-black rather than a fat halo.
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 3],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(if p.dark { 96 } else { 32 }),
    };

    // Widget states: tonal fills, self-colored hairline edges, a single corner
    // radius, and no grow-on-hover (`expansion = 0`) so nothing boops.
    let cr = radius((p.corner - 1.0).max(0.0));
    let w = &mut v.widgets;

    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.noninteractive.corner_radius = cr;

    w.inactive.bg_fill = p.surface;
    w.inactive.weak_bg_fill = p.surface;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, p.text_muted);
    w.inactive.corner_radius = cr;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = p.surface_hover;
    w.hovered.weak_bg_fill = p.surface_hover;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, p.border_strong);
    w.hovered.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.hovered.corner_radius = cr;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.surface_active;
    w.active.weak_bg_fill = p.surface_active;
    w.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    w.active.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.active.corner_radius = cr;
    w.active.expansion = 0.0;

    w.open = w.hovered;

    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(SM, SM);
    s.button_padding = egui::vec2(10.0, 6.0);
    s.interact_size.y = 26.0;
    s.window_margin = Margin::ZERO;
    s.menu_margin = Margin::same(6);
    s.scroll = egui::style::ScrollStyle::thin();

    ctx.set_style(style);
    p
}
