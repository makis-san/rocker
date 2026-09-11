//! Rocker's own icon set: geometric line marks drawn on a 16-unit grid with a
//! single stroke weight, round joins, and one construction idea reused across
//! the set. Not an icon pack (PLAN §9, design bar) — each glyph is drawn here so
//! the family has a point of view and stays consistent with the rest of the UI.

use egui::{
    epaint::PathShape, vec2, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui,
    Vec2,
};

use crate::style::{self, Palette};

/// One mark. `Cube` is the signature (app mark and the empty-state glyph).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    /// Isometric container — the Rocker mark.
    Cube,
    Play,
    Stop,
    Restart,
    Refresh,
    /// Stacked planes: a Compose project / group.
    Stack,
    /// Warning / unreachable.
    Alert,
    /// Dismiss.
    Close,
    /// Two control rails with a knob each — the settings toggle. Deliberately
    /// not a cog: it reuses the set's hollow-ring motif.
    Sliders,
    /// Stepper decrement.
    Minus,
    /// Stepper increment.
    Plus,
    /// Pause — two bars, the counterpart to `Play`.
    Pause,
    /// Back to the list: a left arrow.
    Back,
    /// Overview tab — the hollow-ring motif as an info disc.
    Info,
    /// Logs tab — three left-set lines of unequal length.
    Lines,
    /// Stats tab — a plotted pulse.
    Pulse,
    /// Terminal tab — a framed prompt caret.
    Terminal,
    /// Copy to clipboard — two offset leaves.
    Copy,
    /// Delete / remove — an open-lidded bin with two ridge lines, built from
    /// the same single-stroke lines as the rest of the set (not a pack glyph).
    Trash,
    /// CPU usage — a processor die with a hollow core and two short pins on
    /// every edge. Reuses the set's hollow-centre motif.
    Cpu,
    /// Memory usage — a module board split into three cells, with two contact
    /// tabs on the pin edge.
    Memory,
    /// Timestamps toggle — the hollow-ring motif as a clock face with two hands.
    Clock,
    /// Export to file — a down stroke and arrowhead landing on a short tray.
    Download,
    /// Jump to newest — a double chevron settling onto a baseline.
    JumpDown,
    /// Extensions — a single puzzle piece: a squared outline with one
    /// outward tab and one inward notch, built from the same straight-edge
    /// lines as `Cube` and `Stack` rather than a curved pack glyph.
    Puzzle,
    /// Open in file manager — a folder silhouette (body + top tab), traced as
    /// one closed straight-edge outline like the rest of the set.
    Folder,
}

/// Maps 0..16 grid coordinates into a centered square inside `rect`.
struct Grid {
    origin: Pos2,
    unit: f32,
}

impl Grid {
    fn new(rect: Rect) -> Self {
        let side = rect.width().min(rect.height());
        Self {
            origin: rect.center() - vec2(side, side) * 0.5,
            unit: side / 16.0,
        }
    }

    fn at(&self, x: f32, y: f32) -> Pos2 {
        self.origin + vec2(x * self.unit, y * self.unit)
    }

    /// Stroke weight scaled to the mark size, so a 14px glyph and a 44px glyph
    /// read at the same visual weight.
    fn stroke(&self, color: Color32) -> Stroke {
        Stroke::new((self.unit * 1.35).max(1.25), color)
    }
}

/// Draw `icon` centered in `rect`, in `color`.
pub fn draw(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32) {
    let g = Grid::new(rect);
    let stroke = g.stroke(color);
    let line = |pts: &[(f32, f32)], closed: bool| {
        let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| g.at(x, y)).collect();
        let shape = if closed {
            PathShape::closed_line(pts, stroke)
        } else {
            PathShape::line(pts, stroke)
        };
        painter.add(Shape::Path(shape));
    };
    let fill_poly = |pts: &[(f32, f32)]| {
        let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| g.at(x, y)).collect();
        painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
    };

    match icon {
        Icon::Cube => {
            // A hexagon silhouette with three edges meeting at the center — the
            // reused construction idea for the whole set.
            line(
                &[
                    (8.0, 1.5),
                    (14.0, 5.0),
                    (14.0, 11.0),
                    (8.0, 14.5),
                    (2.0, 11.0),
                    (2.0, 5.0),
                ],
                true,
            );
            line(&[(8.0, 8.0), (8.0, 14.5)], false);
            line(&[(8.0, 8.0), (14.0, 5.0)], false);
            line(&[(8.0, 8.0), (2.0, 5.0)], false);
        }
        Icon::Play => fill_poly(&[(4.5, 3.0), (13.5, 8.0), (4.5, 13.0)]),
        Icon::Stop => {
            painter.rect_filled(
                Rect::from_min_max(g.at(3.5, 3.5), g.at(12.5, 12.5)),
                style::radius(2.0),
                color,
            );
        }
        Icon::Restart => {
            // ~270 deg arc opening at the top-right, with a tick arrowhead.
            arc(painter, &g, color, 5.8, 305.0..=40.0);
            line(&[(11.8, 1.8), (13.6, 5.2), (10.0, 6.2)], false);
        }
        Icon::Refresh => {
            // Two opposed half-arcs — distinct from the single-arrow Restart.
            arc(painter, &g, color, 5.5, 130.0..=-20.0);
            arc(painter, &g, color, 5.5, -50.0..=-200.0);
            line(&[(12.4, 2.6), (13.4, 5.0), (10.9, 5.6)], false);
            line(&[(3.6, 13.4), (2.6, 11.0), (5.1, 10.4)], false);
        }
        Icon::Stack => {
            line(&[(2.5, 5.5), (8.0, 2.5), (13.5, 5.5), (8.0, 8.5)], true);
            line(&[(2.5, 8.5), (8.0, 11.5), (13.5, 8.5)], false);
            line(&[(2.5, 11.3), (8.0, 14.3), (13.5, 11.3)], false);
        }
        Icon::Alert => {
            line(&[(8.0, 2.0), (14.5, 13.5), (1.5, 13.5)], true);
            line(&[(8.0, 6.0), (8.0, 9.8)], false);
            painter.circle_filled(g.at(8.0, 11.6), (g.unit * 0.9).max(1.0), color);
        }
        Icon::Close => {
            line(&[(4.0, 4.0), (12.0, 12.0)], false);
            line(&[(12.0, 4.0), (4.0, 12.0)], false);
        }
        Icon::Sliders => {
            let knob = (g.unit * 1.7).max(2.0);
            // Upper rail: knob toward the right, a gap in the rail behind it.
            line(&[(2.5, 5.5), (8.3, 5.5)], false);
            line(&[(11.7, 5.5), (13.5, 5.5)], false);
            painter.circle_stroke(g.at(10.0, 5.5), knob, stroke);
            // Lower rail: knob toward the left.
            line(&[(2.5, 10.5), (3.8, 10.5)], false);
            line(&[(7.2, 10.5), (13.5, 10.5)], false);
            painter.circle_stroke(g.at(5.5, 10.5), knob, stroke);
        }
        Icon::Minus => line(&[(4.0, 8.0), (12.0, 8.0)], false),
        Icon::Plus => {
            line(&[(8.0, 4.0), (8.0, 12.0)], false);
            line(&[(4.0, 8.0), (12.0, 8.0)], false);
        }
        Icon::Pause => {
            painter.rect_filled(
                Rect::from_min_max(g.at(4.5, 3.5), g.at(7.0, 12.5)),
                style::radius(1.5),
                color,
            );
            painter.rect_filled(
                Rect::from_min_max(g.at(9.0, 3.5), g.at(11.5, 12.5)),
                style::radius(1.5),
                color,
            );
        }
        Icon::Back => {
            line(&[(9.5, 3.5), (4.5, 8.0), (9.5, 12.5)], false);
            line(&[(4.5, 8.0), (13.0, 8.0)], false);
        }
        Icon::Info => {
            painter.circle_stroke(g.at(8.0, 8.0), 6.0 * g.unit, stroke);
            line(&[(8.0, 7.4), (8.0, 11.5)], false);
            painter.circle_filled(g.at(8.0, 4.7), (g.unit * 0.85).max(1.0), color);
        }
        Icon::Lines => {
            line(&[(3.0, 4.5), (13.0, 4.5)], false);
            line(&[(3.0, 8.0), (10.5, 8.0)], false);
            line(&[(3.0, 11.5), (12.0, 11.5)], false);
        }
        Icon::Pulse => line(
            &[
                (2.0, 10.0),
                (5.5, 10.0),
                (7.5, 4.5),
                (10.0, 12.5),
                (12.0, 10.0),
                (14.0, 10.0),
            ],
            false,
        ),
        Icon::Terminal => {
            line(&[(2.5, 3.5), (13.5, 3.5), (13.5, 12.5), (2.5, 12.5)], true);
            line(&[(5.0, 6.5), (7.5, 8.5), (5.0, 10.5)], false);
            line(&[(8.5, 10.5), (11.0, 10.5)], false);
        }
        Icon::Copy => {
            line(&[(6.0, 6.0), (13.0, 6.0), (13.0, 13.0), (6.0, 13.0)], true);
            line(&[(10.0, 3.5), (3.5, 3.5), (3.5, 10.0)], false);
        }
        Icon::Trash => {
            line(&[(3.0, 4.5), (13.0, 4.5)], false);
            line(&[(6.3, 4.5), (6.3, 2.3), (9.7, 2.3), (9.7, 4.5)], false);
            line(&[(4.3, 4.5), (4.9, 13.2), (11.1, 13.2), (11.7, 4.5)], false);
            line(&[(6.7, 6.8), (6.9, 11.2)], false);
            line(&[(9.3, 6.8), (9.1, 11.2)], false);
        }
        Icon::Cpu => {
            line(&[(4.5, 4.5), (11.5, 4.5), (11.5, 11.5), (4.5, 11.5)], true);
            line(&[(6.8, 6.8), (9.2, 6.8), (9.2, 9.2), (6.8, 9.2)], true);
            for c in [6.4_f32, 9.6] {
                line(&[(c, 4.5), (c, 2.6)], false);
                line(&[(c, 11.5), (c, 13.4)], false);
                line(&[(4.5, c), (2.6, c)], false);
                line(&[(11.5, c), (13.4, c)], false);
            }
        }
        Icon::Memory => {
            line(&[(2.5, 4.5), (13.5, 4.5), (13.5, 10.5), (2.5, 10.5)], true);
            line(&[(6.2, 4.5), (6.2, 10.5)], false);
            line(&[(9.8, 4.5), (9.8, 10.5)], false);
            line(&[(5.2, 10.5), (5.2, 12.6)], false);
            line(&[(10.8, 10.5), (10.8, 12.6)], false);
        }
        Icon::Clock => {
            painter.circle_stroke(g.at(8.0, 8.0), 6.0 * g.unit, stroke);
            line(&[(8.0, 8.0), (8.0, 4.3)], false);
            line(&[(8.0, 8.0), (10.9, 9.4)], false);
        }
        Icon::Download => {
            line(&[(8.0, 2.6), (8.0, 10.2)], false);
            line(&[(4.8, 7.0), (8.0, 10.4), (11.2, 7.0)], false);
            line(&[(3.2, 13.2), (12.8, 13.2)], false);
        }
        Icon::JumpDown => {
            line(&[(4.5, 3.4), (8.0, 6.9), (11.5, 3.4)], false);
            line(&[(4.5, 7.9), (8.0, 11.4), (11.5, 7.9)], false);
            line(&[(4.0, 13.7), (12.0, 13.7)], false);
        }
        Icon::Puzzle => {
            line(
                &[
                    (3.0, 3.0),
                    (6.5, 3.0),
                    (7.3, 1.8),
                    (8.7, 1.8),
                    (9.5, 3.0),
                    (13.0, 3.0),
                    (13.0, 6.5),
                    (11.8, 7.3),
                    (11.8, 8.7),
                    (13.0, 9.5),
                    (13.0, 13.0),
                    (3.0, 13.0),
                ],
                true,
            );
        }
        Icon::Folder => {
            line(
                &[
                    (2.0, 5.0),
                    (2.0, 3.5),
                    (6.5, 3.5),
                    (7.5, 5.0),
                    (14.0, 5.0),
                    (14.0, 13.0),
                    (2.0, 13.0),
                ],
                true,
            );
        }
    }
}

/// A disclosure chevron. Points right at `open_t == 0`, rotates a quarter turn
/// to point down at `open_t == 1`. Caller animates `open_t`.
pub fn chevron(painter: &egui::Painter, rect: Rect, color: Color32, open_t: f32) {
    let g = Grid::new(rect);
    let (sin, cos) = (open_t * std::f32::consts::FRAC_PI_2).sin_cos();
    let rot = |x: f32, y: f32| {
        let (dx, dy) = (x - 8.0, y - 8.0);
        g.at(8.0 + dx * cos - dy * sin, 8.0 + dx * sin + dy * cos)
    };
    let pts = vec![rot(6.5, 3.5), rot(11.0, 8.0), rot(6.5, 12.5)];
    painter.add(Shape::Path(PathShape::line(pts, g.stroke(color))));
}

/// Polyline approximation of an arc centered on grid (8, 8). `sweep` is a range
/// of degrees, 0 deg = +x, measured counter-clockwise.
fn arc(
    painter: &egui::Painter,
    g: &Grid,
    color: Color32,
    r: f32,
    sweep: std::ops::RangeInclusive<f32>,
) {
    let (from_deg, to_deg) = (*sweep.start(), *sweep.end());
    let steps = 24;
    let pts: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let a = (from_deg + (to_deg - from_deg) * i as f32 / steps as f32).to_radians();
            g.at(8.0 + r * a.cos(), 8.0 - r * a.sin())
        })
        .collect();
    painter.add(Shape::Path(PathShape::line(pts, g.stroke(color))));
}

/// A borderless icon button: tonal wash on hover (fading in, no lift), the glyph
/// shifting from muted to full ink. `tint` sets the resting glyph color for a
/// primary action; `None` uses muted ink.
pub fn icon_button(
    ui: &mut Ui,
    pal: &Palette,
    icon: Icon,
    tint: Option<Color32>,
    tooltip: &str,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(style::ICON_BTN), Sense::click());
    let t = ui.ctx().animate_bool(resp.id, resp.hovered());

    let painter = ui.painter();
    if t > 0.0 {
        painter.rect_filled(rect, style::radius(pal.corner - 2.0), pal.tint(0.11 * t));
    }
    // Legible at rest, full ink on hover.
    let rest = tint.unwrap_or_else(|| pal.text_muted.lerp_to_gamma(pal.text, 0.45));
    let peak = tint.map_or(pal.text, |a| a.lerp_to_gamma(pal.text, 0.25));
    let fg = rest.lerp_to_gamma(peak, t);
    let inset = (style::ICON_BTN - style::ICON_SM) * 0.5;
    draw(painter, icon, rect.shrink(inset), fg);

    resp.on_hover_text(tooltip)
}

/// Like [`icon_button`], but gated by `enabled`: disabled, it senses hover
/// only (never a click) and draws dimmed with no hover wash, so it reads as
/// genuinely inert rather than a live control that quietly does nothing.
pub fn icon_button_enabled(
    ui: &mut Ui,
    pal: &Palette,
    icon: Icon,
    tint: Option<Color32>,
    tooltip: &str,
    enabled: bool,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::splat(style::ICON_BTN),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let t = if enabled {
        ui.ctx().animate_bool(resp.id, resp.hovered())
    } else {
        0.0
    };

    let painter = ui.painter();
    if t > 0.0 {
        painter.rect_filled(rect, style::radius(pal.corner - 2.0), pal.tint(0.11 * t));
    }
    let color = if enabled {
        let rest = tint.unwrap_or_else(|| pal.text_muted.lerp_to_gamma(pal.text, 0.45));
        let peak = tint.map_or(pal.text, |a| a.lerp_to_gamma(pal.text, 0.25));
        rest.lerp_to_gamma(peak, t)
    } else {
        pal.text_faint
    };
    let inset = (style::ICON_BTN - style::ICON_SM) * 0.5;
    draw(painter, icon, rect.shrink(inset), color);

    resp.on_hover_text(tooltip)
}

/// Like [`icon_button`], but with a sticky state: while `active` it holds a
/// tonal wash and full-ink glyph, so a header view toggle reads as engaged
/// without a separate indicator. Still no lift — state is tone only.
pub fn toggle_icon_button(
    ui: &mut Ui,
    pal: &Palette,
    icon: Icon,
    active: bool,
    tooltip: &str,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(style::ICON_BTN), Sense::click());
    let hover_t = ui.ctx().animate_bool(resp.id, resp.hovered());
    let active_t = ui.ctx().animate_bool(resp.id.with("active"), active);

    let wash = (0.11 * hover_t).max(0.16 * active_t);
    if wash > 0.0 {
        ui.painter()
            .rect_filled(rect, style::radius(pal.corner - 2.0), pal.tint(wash));
    }
    let rest = pal.text_muted.lerp_to_gamma(pal.text, 0.45);
    let fg = rest
        .lerp_to_gamma(pal.text, hover_t)
        .lerp_to_gamma(pal.text, active_t);
    let inset = (style::ICON_BTN - style::ICON_SM) * 0.5;
    draw(ui.painter(), icon, rect.shrink(inset), fg);

    resp.on_hover_text(tooltip)
}

/// A compact text toggle (the Logs tab's `.*` regex switch): the same sticky
/// tonal state as [`toggle_icon_button`], sized to a short monospace label so a
/// glyphy caption like `.*` sits true. No lift, no border — state is tone only.
pub fn toggle_text_button(
    ui: &mut Ui,
    pal: &Palette,
    label: &str,
    active: bool,
    tooltip: &str,
) -> Response {
    let font = egui::FontId::monospace(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE);
    let w = (galley.rect.width() + 14.0).max(style::ICON_BTN);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, style::ICON_BTN), Sense::click());
    let hover_t = ui.ctx().animate_bool(resp.id, resp.hovered());
    let active_t = ui.ctx().animate_bool(resp.id.with("active"), active);

    let wash = (0.11 * hover_t).max(0.16 * active_t);
    if wash > 0.0 {
        ui.painter()
            .rect_filled(rect, style::radius(pal.corner - 2.0), pal.tint(wash));
    }
    let rest = pal.text_muted.lerp_to_gamma(pal.text, 0.45);
    let fg = rest
        .lerp_to_gamma(pal.text, hover_t)
        .lerp_to_gamma(pal.text, active_t);
    ui.painter()
        .text(rect.center(), egui::Align2::CENTER_CENTER, label, font, fg);

    resp.on_hover_text(tooltip)
}

/// One filled action button in a given `fill` tone: legible ink on it, one
/// radius, no shadow, no lift. The shared shape behind [`primary_button`] and
/// any status-tinted variant (a destructive confirm, say) — never paired with
/// a ghost button.
pub fn filled_button(ui: &mut Ui, pal: &Palette, label: &str, fill: Color32) -> Response {
    let text = egui::WidgetText::from(label).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let pad = vec2(14.0, 8.0);
    let (rect, resp) = ui.allocate_exact_size(text.size() + pad * 2.0, Sense::click());
    let t = ui.ctx().animate_bool(resp.id, resp.hovered());
    let bg = if resp.is_pointer_button_down_on() {
        fill.lerp_to_gamma(pal.text, 0.12)
    } else {
        fill.lerp_to_gamma(pal.on_accent, 0.10 * t)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        style::radius(pal.corner - 1.0),
        bg,
        Stroke::NONE,
        StrokeKind::Inside,
    );
    let text_pos = rect.center() - text.size() * 0.5;
    painter.galley(text_pos, text, pal.on_accent);
    resp
}

/// The one filled action in a view (e.g. "Retry connection"). Accent fill.
pub fn primary_button(ui: &mut Ui, pal: &Palette, label: &str) -> Response {
    filled_button(ui, pal, label, pal.accent)
}

/// A quiet text-only button: no fill at rest, a tonal wash fading in on
/// hover. Used beside a [`filled_button`] for the lower-weight side of a
/// choice (e.g. "Cancel" next to a destructive confirm) — never a bordered
/// "ghost" twin of the filled button.
pub fn text_button(ui: &mut Ui, pal: &Palette, label: &str) -> Response {
    let text = egui::WidgetText::from(label).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let pad = vec2(12.0, 8.0);
    let (rect, resp) = ui.allocate_exact_size(text.size() + pad * 2.0, Sense::click());
    let t = ui.ctx().animate_bool(resp.id, resp.hovered());
    if t > 0.0 {
        ui.painter()
            .rect_filled(rect, style::radius(pal.corner - 1.0), pal.tint(0.08 * t));
    }
    let color = pal.text_muted.lerp_to_gamma(pal.text, 0.4 + 0.5 * t);
    let text_pos = rect.center() - text.size() * 0.5;
    ui.painter().galley(text_pos, text, color);
    resp
}
