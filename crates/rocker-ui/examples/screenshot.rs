//! Renders the real `RockerApp` against the local Docker daemon, captures one
//! frame via eframe's screenshot command, writes it as a BMP, and exits.
//!
//! ```sh
//! cargo run -p rocker-ui --example screenshot -- /tmp/rocker.bmp
//!
//! # The settings screen instead of the container list:
//! DOCKMAN_VIEW=settings cargo run -p rocker-ui --example screenshot -- /tmp/settings.bmp
//!
//! # The registries screen, seeded with a verified and an unverified entry:
//! DOCKMAN_VIEW=registries cargo run -p rocker-ui --example screenshot -- /tmp/registries.bmp
//!
//! # A container screen. DOCKMAN_CONTAINER filters by name substring (first
//! # match wins, empty = any); DOCKMAN_TAB picks the tab (overview / logs /
//! # stats / terminal, default overview).
//! DOCKMAN_VIEW=container DOCKMAN_TAB=stats cargo run -p rocker-ui --example screenshot -- /tmp/stats.bmp
//! ```
//!
//! BMP (not PNG) so the example needs no image-encoding dependency; convert with
//! `magick /tmp/rocker.bmp /tmp/rocker.png` if you want PNG.

use std::io::Write;
use std::time::{Duration, Instant};

use rocker_ui::RockerApp;

/// What to drive the app to before capturing.
enum Target {
    List,
    Settings,
    Registries,
    Container { needle: String, tab: String },
}

struct Capture {
    inner: RockerApp,
    out: String,
    target: Target,
    start: Instant,
    opened: bool,
    started_terminal: bool,
    requested: bool,
}

/// How long to let each stage settle before moving to the next. Generous
/// because the engine, and Docker itself, need real wall-clock time (an
/// `exec` round trip, a `stats` sample arriving) — frame count alone doesn't
/// track that under a software GL renderer.
const CONNECT: Duration = Duration::from_millis(900);
const OPEN: Duration = Duration::from_millis(500);
/// Stats/logs/terminal need at least one live sample; give it real headroom.
const WARM: Duration = Duration::from_secs(9);

impl eframe::App for Capture {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.inner.update(ctx, frame);
        let elapsed = self.start.elapsed();

        if !self.opened && elapsed >= CONNECT {
            match &self.target {
                Target::Settings => {
                    self.inner.open_settings();
                    self.opened = true;
                }
                Target::Registries => {
                    self.inner.debug_seed_registry("ghcr.io", "octo", true);
                    self.inner
                        .debug_seed_registry("registry.gitlab.com", "octo", false);
                    self.inner.open_registries();
                    self.opened = true;
                }
                // The container list may not have landed on the very first
                // try right at CONNECT; keep retrying each frame until it has.
                Target::Container { needle, .. } => self.opened = self.inner.open_container(needle),
                Target::List => self.opened = true,
            }
        }

        if let Target::Container { tab, .. } = &self.target {
            if self.opened && elapsed >= CONNECT + OPEN && !self.started_terminal {
                self.inner.debug_set_tab(tab);
                if tab == "terminal" {
                    self.inner.debug_start_terminal();
                }
                self.started_terminal = true;
            }
        }

        let warm_needed = match &self.target {
            Target::Container { tab, .. } => matches!(tab.as_str(), "stats" | "logs" | "terminal"),
            _ => false,
        };
        let ready_at = CONNECT + OPEN + if warm_needed { WARM } else { Duration::ZERO };

        if elapsed >= ready_at && !self.requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            self.requested = true;
        }

        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            write_bmp(&self.out, &image).expect("write bmp");
            eprintln!("wrote {} ({}x{})", self.out, image.size[0], image.size[1]);
            std::process::exit(0);
        }

        ctx.request_repaint();
    }
}

fn write_bmp(path: &str, image: &egui::ColorImage) -> std::io::Result<()> {
    let (w, h) = (image.size[0] as u32, image.size[1] as u32);
    let row = (w * 4) as usize;
    let pixel_bytes = row * h as usize;
    let file_size = 54 + pixel_bytes as u32;

    let mut buf = Vec::with_capacity(file_size as usize);
    buf.extend_from_slice(b"BM");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset
    buf.extend_from_slice(&40u32.to_le_bytes()); // DIB header size
    buf.extend_from_slice(&(w as i32).to_le_bytes());
    buf.extend_from_slice(&(-(h as i32)).to_le_bytes()); // negative = top-down
    buf.extend_from_slice(&1u16.to_le_bytes()); // planes
    buf.extend_from_slice(&32u16.to_le_bytes()); // bpp
    buf.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    buf.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
    buf.extend_from_slice(&2835i32.to_le_bytes()); // ~72 DPI
    buf.extend_from_slice(&2835i32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());

    for px in &image.pixels {
        let [r, g, b, a] = px.to_srgba_unmultiplied();
        buf.extend_from_slice(&[b, g, r, a]);
    }

    let mut f = std::fs::File::create(path)?;
    f.write_all(&buf)
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/rocker.bmp".to_string());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("rocker-engine")
        .build()
        .expect("tokio runtime");
    let handle = runtime.handle().clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rocker")
            .with_inner_size([1040.0, 720.0]),
        ..Default::default()
    };

    eprintln!(
        "DEBUG env DOCKMAN_VIEW={:?} DOCKMAN_CONTAINER={:?} DOCKMAN_TAB={:?} pid={}",
        std::env::var("DOCKMAN_VIEW"),
        std::env::var("DOCKMAN_CONTAINER"),
        std::env::var("DOCKMAN_TAB"),
        std::process::id(),
    );
    let target = match std::env::var("DOCKMAN_VIEW").as_deref() {
        Ok("settings") => Target::Settings,
        Ok("registries") => Target::Registries,
        Ok("container") => Target::Container {
            needle: std::env::var("DOCKMAN_CONTAINER").unwrap_or_default(),
            tab: std::env::var("DOCKMAN_TAB").unwrap_or_else(|_| "overview".into()),
        },
        _ => Target::List,
    };

    eframe::run_native(
        "rocker-screenshot",
        options,
        Box::new(move |cc| {
            Ok(Box::new(Capture {
                inner: RockerApp::new(cc, handle.clone()),
                out,
                target,
                start: Instant::now(),
                opened: false,
                started_terminal: false,
                requested: false,
            }))
        }),
    )
    .expect("run");
}
