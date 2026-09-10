//! Rendering and input for the in-app terminal.
//!
//! [`rocker_term::Screen`] holds the VT grid; this module paints it as one
//! monospace [`egui::text::LayoutJob`] per row, and caches the resulting galley
//! keyed by a content hash so an unchanged row costs nothing to re-lay (PLAN
//! §2, §9). It also translates `egui` key/text events into the byte stream a
//! PTY expects. The exec transport lives in `rocker-engine`; nothing here talks
//! to Docker directly.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{vec2, Color32, FontId, Galley, Rect, Sense, Stroke, StrokeKind, Vec2};
use rocker_term::{keys, Cell, Color, Screen};

use crate::style::{self, Palette};

/// Per-row galley cache for one terminal surface. A row whose content hash is
/// unchanged since last frame reuses its laid-out galley instead of rebuilding
/// the style runs and re-shaping the text.
#[derive(Default)]
pub struct RowCache {
    rows: Vec<Option<(u64, Arc<Galley>)>>,
}

impl RowCache {
    /// Drop everything — used when the session restarts.
    pub fn clear(&mut self) {
        self.rows.clear();
    }
}

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

/// Paint the grid inside `rect`, reusing `cache`'s galley for any row whose
/// content (and the palette) is unchanged since last frame.
pub fn paint(
    ui: &mut egui::Ui,
    pal: &Palette,
    screen: &Screen,
    rect: Rect,
    focused: bool,
    cache: &mut RowCache,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, style::radius(pal.corner), pal.term_bg);

    let cs = cell_size(ui);
    let (_, rows) = screen.size();
    let origin = rect.left_top() + vec2(6.0, 5.0);
    let pal_fp = palette_fingerprint(pal);

    if cache.rows.len() != rows as usize {
        cache.rows.clear();
        cache.rows.resize(rows as usize, None);
    }

    for (y, row) in screen.rows().enumerate() {
        if y >= rows as usize {
            break;
        }
        let h = row_hash(pal_fp, row);
        let galley = match &cache.rows[y] {
            Some((cached, g)) if *cached == h => g.clone(),
            _ => {
                let g = ui.fonts_mut(|f| f.layout_job(row_job(pal, row)));
                cache.rows[y] = Some((h, g.clone()));
                g
            }
        };
        painter.galley(
            egui::pos2(origin.x, origin.y + y as f32 * cs.y),
            galley,
            pal.term_fg,
        );
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
                Stroke::new(1.0_f32, pal.accent.gamma_multiply(0.7)),
                StrokeKind::Inside,
            );
        }
    }
}

/// The effective (foreground, optional background) a cell paints in, after
/// inverse and dim are folded in.
fn cell_colors(pal: &Palette, c: &Cell) -> (Color32, Option<Color32>) {
    let mut fg = resolve(pal, c.fg, true);
    let mut bg = match c.bg {
        Color::Default => None,
        other => Some(resolve(pal, other, false)),
    };
    if c.inverse {
        let f = resolve(pal, c.fg, true);
        bg = Some(f);
        fg = resolve(pal, c.bg, false);
    }
    if c.dim {
        fg = fg.lerp_to_gamma(pal.term_bg, 0.4);
    }
    (fg, bg)
}

/// Build one row as a single `LayoutJob`, coalescing cells that share a visual
/// style into one styled run. Spaces are kept so background fills and the
/// monospace advance stay correct.
fn row_job(pal: &Palette, row: &[Cell]) -> LayoutJob {
    let font = FontId::monospace(FONT_PT);
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;

    let mut x = 0usize;
    while x < row.len() {
        let (fg, bg) = cell_colors(pal, &row[x]);
        let underline = row[x].underline;
        let italics = row[x].italic;
        let mut text = String::new();
        while x < row.len() {
            let c = row[x];
            let (cfg, cbg) = cell_colors(pal, &c);
            if cfg != fg || cbg != bg || c.underline != underline || c.italic != italics {
                break;
            }
            text.push(if c.ch == '\0' || c.ch == ' ' {
                ' '
            } else {
                c.ch
            });
            x += 1;
        }
        job.append(
            &text,
            0.0,
            TextFormat {
                font_id: font.clone(),
                color: fg,
                background: bg.unwrap_or(Color32::TRANSPARENT),
                italics,
                underline: if underline {
                    Stroke::new(1.0, fg)
                } else {
                    Stroke::NONE
                },
                ..Default::default()
            },
        );
    }
    job
}

/// Hash a row's cells together with a palette fingerprint, so a theme change
/// invalidates every cached galley even though the cells didn't change.
fn row_hash(pal_fp: u64, row: &[Cell]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    pal_fp.hash(&mut h);
    row.hash(&mut h);
    h.finish()
}

fn palette_fingerprint(pal: &Palette) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for c in [pal.term_bg, pal.term_fg, pal.accent] {
        c.to_array().hash(&mut h);
    }
    for c in pal.term_ansi {
        c.to_array().hash(&mut h);
    }
    h.finish()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(ch: char) -> Cell {
        Cell {
            ch,
            ..Cell::default()
        }
    }

    #[test]
    fn row_hash_tracks_content_and_palette() {
        let row_a = [cell('h'), cell('i'), cell(' ')];
        let row_b = [cell('h'), cell('o'), cell(' ')];

        assert_eq!(row_hash(1, &row_a), row_hash(1, &row_a), "stable");
        assert_ne!(row_hash(1, &row_a), row_hash(1, &row_b), "content change");
        assert_ne!(
            row_hash(1, &row_a),
            row_hash(2, &row_a),
            "palette change invalidates"
        );

        let mut bold = row_a;
        bold[0].bold = true;
        assert_ne!(row_hash(1, &row_a), row_hash(1, &bold), "attr change");
    }

    #[test]
    fn paint_reuses_cached_rows() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());
        let mut screen = Screen::new(20, 6);
        screen.feed(b"hello world\r\nsecond line\r\n");
        let mut cache = RowCache::default();

        let render = |cache: &mut RowCache| {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let rect =
                        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 200.0));
                    paint(ui, &pal, &screen, rect, true, cache);
                });
            });
        };

        render(&mut cache);
        let first: Vec<u64> = cache.rows.iter().flatten().map(|(h, _)| *h).collect();
        assert_eq!(cache.rows.len(), 6);

        render(&mut cache);
        let second: Vec<u64> = cache.rows.iter().flatten().map(|(h, _)| *h).collect();
        assert_eq!(
            first, second,
            "unchanged rows keep their hash across frames"
        );
    }
}
