//! Rendering and input for the in-app terminal.
//!
//! [`rocker_term::Screen`] holds the VT grid; this module paints it with one
//! monospace galley per row (batched into style runs) and translates `egui`
//! key/text events into the byte stream a PTY expects. The exec transport lives
//! in `rocker-engine`; nothing here talks to Docker directly.

use rocker_term::{keys, Cell, Color, Screen};
use egui::{vec2, Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::style::{self, Palette};

/// Point size for the terminal's monospace grid.
pub const FONT_PT: f32 = 13.0;

/// Cell dimensions for the terminal font at [`FONT_PT`].
pub fn cell_size(ui: &egui::Ui) -> Vec2 {
    let font = FontId::monospace(FONT_PT);
    let galley = ui
        .painter()
        .layout_no_wrap("MMMMMMMMMM".to_owned(), font, Color32::WHITE);
    vec2(
        (galley.rect.width() / 10.0).max(4.0),
        (galley.rect.height() + 2.0).max(8.0),
    )
}

/// Resolve a VT [`Color`] against the palette's terminal colours.
fn resolve(pal: &Palette, c: Color, is_fg: bool) -> Color32 {
    match c {
        Color::Default => {
            if is_fg {
                pal.term_fg
            } else {
                pal.term_bg
            }
        }
        Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        Color::Indexed(i) => indexed(pal, i),
    }
}

fn indexed(pal: &Palette, i: u8) -> Color32 {
    match i {
        0..=15 => pal.term_ansi[i as usize],
        16..=231 => {
            let n = i - 16;
            let step = |v: u8| {
                if v == 0 {
                    0u8
                } else {
                    (v as u16 * 40 + 55) as u8
                }
            };
            Color32::from_rgb(step(n / 36), step((n / 6) % 6), step(n % 6))
        }
        _ => {
            let v = (i - 232) as u16 * 10 + 8;
            Color32::from_gray(v as u8)
        }
    }
}

/// Paint the grid inside `rect`. Returns the response covering the grid so the
/// caller can manage focus.
pub fn paint(ui: &mut egui::Ui, pal: &Palette, screen: &Screen, rect: Rect, focused: bool) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, style::radius(pal.corner), pal.term_bg);

    let cs = cell_size(ui);
    let font = FontId::monospace(FONT_PT);
    let (cols, rows) = screen.size();
    let origin = rect.left_top() + vec2(6.0, 5.0);

    for (y, row) in screen.rows().enumerate() {
        if y as u16 >= rows {
            break;
        }
        let y_px = origin.y + y as f32 * cs.y;

        // Background runs first, so glyphs sit on top of their cell fill.
        let mut x0 = 0usize;
        while x0 < row.len() {
            let bg = run_bg(pal, &row[x0]);
            let mut x1 = x0 + 1;
            while x1 < row.len() && run_bg(pal, &row[x1]) == bg {
                x1 += 1;
            }
            if let Some(fill) = bg {
                let r = Rect::from_min_size(
                    egui::pos2(origin.x + x0 as f32 * cs.x, y_px),
                    vec2((x1 - x0) as f32 * cs.x, cs.y),
                );
                painter.rect_filled(r, 0.0, fill);
            }
            x0 = x1;
        }

        // Then glyph runs sharing fg + attributes.
        let mut x = 0usize;
        while x < row.len() {
            let cell = row[x];
            if cell.ch == ' ' && !cell.underline {
                x += 1;
                continue;
            }
            let key = style_key(&cell);
            let mut text = String::new();
            let start = x;
            while x < row.len() {
                let c = row[x];
                if style_key(&c) != key {
                    break;
                }
                text.push(if c.ch == '\0' { ' ' } else { c.ch });
                x += 1;
            }
            let mut fg = resolve(pal, cell.fg, true);
            if cell.inverse {
                fg = resolve(pal, cell.bg, false);
            }
            if cell.dim {
                fg = fg.lerp_to_gamma(pal.term_bg, 0.4);
            }
            let pos = egui::pos2(origin.x + start as f32 * cs.x, y_px);
            painter.text(pos, Align2::LEFT_TOP, &text, font.clone(), fg);
            if cell.underline {
                let uy = y_px + cs.y - 1.5;
                painter.hline(
                    pos.x..=pos.x + (x - start) as f32 * cs.x,
                    uy,
                    Stroke::new(1.0, fg),
                );
            }
        }
        let _ = cols;
    }

    // Cursor.
    if screen.cursor_visible() {
        let (cx, cy) = screen.cursor();
        let cr = Rect::from_min_size(
            egui::pos2(origin.x + cx as f32 * cs.x, origin.y + cy as f32 * cs.y),
            cs,
        );
        if focused {
            painter.rect_filled(cr, 1.0, pal.accent.gamma_multiply(0.55));
        } else {
            painter.rect(
                cr,
                1.0,
                Color32::TRANSPARENT,
                Stroke::new(1.0, pal.accent.gamma_multiply(0.7)),
                StrokeKind::Inside,
            );
        }
    }
}

fn run_bg(pal: &Palette, c: &Cell) -> Option<Color32> {
    let bg = if c.inverse {
        resolve(pal, c.fg, true)
    } else {
        match c.bg {
            Color::Default => return None,
            other => resolve(pal, other, false),
        }
    };
    Some(bg)
}

/// A hashable-ish key so consecutive cells with the same visual style batch into
/// one `painter.text` call.
fn style_key(c: &Cell) -> (Color, bool, bool, bool) {
    (c.fg, c.bold, c.italic, c.inverse)
}

/// An interactive surface for the grid: click to focus, and while focused it
/// locks Tab / arrows / Escape so they reach the shell instead of moving egui
/// focus. Returns `(response, focused)`.
pub fn surface(ui: &mut egui::Ui, rect: Rect, id: egui::Id) -> (egui::Response, bool) {
    let resp = ui.interact(rect, id, Sense::click());
    if resp.clicked() {
        resp.request_focus();
    }
    let focused = resp.has_focus();
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
    }
    (resp, focused)
}

/// Translate this frame's input into PTY bytes, consuming the events it handles
/// so `egui` does not also act on them. Only call while the grid has focus.
pub fn take_input(ui: &egui::Ui) -> Vec<u8> {
    let mut out = Vec::new();
    ui.input(|i| {
        for ev in &i.events {
            match ev {
                egui::Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                egui::Event::Paste(t) => out.extend_from_slice(t.as_bytes()),
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if modifiers.ctrl || modifiers.mac_cmd {
                        if let Some(b) = ctrl_combo(*key) {
                            out.push(b);
                        }
                    } else if let Some(bytes) = named_key(*key) {
                        out.extend_from_slice(&bytes);
                    }
                }
                _ => {}
            }
        }
    });
    out
}

fn named_key(key: egui::Key) -> Option<Vec<u8>> {
    use egui::Key as K;
    let k = match key {
        K::Enter => keys::Key::Enter,
        K::Backspace => keys::Key::Backspace,
        K::Tab => keys::Key::Tab,
        K::Escape => keys::Key::Escape,
        K::ArrowUp => keys::Key::Up,
        K::ArrowDown => keys::Key::Down,
        K::ArrowRight => keys::Key::Right,
        K::ArrowLeft => keys::Key::Left,
        K::Home => keys::Key::Home,
        K::End => keys::Key::End,
        K::PageUp => keys::Key::PageUp,
        K::PageDown => keys::Key::PageDown,
        K::Delete => keys::Key::Delete,
        K::Insert => keys::Key::Insert,
        K::F1 => keys::Key::F(1),
        K::F2 => keys::Key::F(2),
        K::F3 => keys::Key::F(3),
        K::F4 => keys::Key::F(4),
        K::F5 => keys::Key::F(5),
        K::F6 => keys::Key::F(6),
        K::F7 => keys::Key::F(7),
        K::F8 => keys::Key::F(8),
        K::F9 => keys::Key::F(9),
        K::F10 => keys::Key::F(10),
        K::F11 => keys::Key::F(11),
        K::F12 => keys::Key::F(12),
        _ => return None,
    };
    Some(keys::encode(k))
}

fn ctrl_combo(key: egui::Key) -> Option<u8> {
    use egui::Key as K;
    let ch = match key {
        K::A => 'a',
        K::B => 'b',
        K::C => 'c',
        K::D => 'd',
        K::E => 'e',
        K::F => 'f',
        K::G => 'g',
        K::H => 'h',
        K::I => 'i',
        K::J => 'j',
        K::K => 'k',
        K::L => 'l',
        K::M => 'm',
        K::N => 'n',
        K::O => 'o',
        K::P => 'p',
        K::Q => 'q',
        K::R => 'r',
        K::S => 's',
        K::T => 't',
        K::U => 'u',
        K::V => 'v',
        K::W => 'w',
        K::X => 'x',
        K::Y => 'y',
        K::Z => 'z',
        K::OpenBracket => '[',
        K::CloseBracket => ']',
        K::Backslash => '\\',
        K::Space => ' ',
        _ => return None,
    };
    keys::ctrl_byte(ch)
}
