//! System-tray icon and its live Docker summary.
//!
//! The tray shows the totals at a glance — combined CPU, memory in use, disk
//! Docker is holding, and how many containers are up vs. stopped — and is the
//! way back to the window after "minimize to tray". Everything it shows is
//! pushed in from the egui frame through [`Tray::render`]; the tray never
//! touches the engine itself.
//!
//! ## Threading
//!
//! On Linux the tray is `libappindicator`: GTK objects that have to be created
//! and driven on one thread. We make that thread the egui UI thread —
//! `gtk::init` in [`Tray::new`], then [`Tray::render`] pumps the GTK loop once
//! per frame. A heartbeat repaint (see `app.rs`) keeps frames coming while the
//! window is hidden, so tray clicks still land.

/// A click the tray is asking the app to act on.
// Without the `tray` feature the stub never builds these, but `app.rs` still
// matches on them.
#[cfg_attr(not(feature = "tray"), allow(dead_code))]
pub enum TrayAction {
    /// Bring the window back.
    Show,
    /// Quit for real (not "minimize to tray").
    Quit,
}

/// The numbers the tray menu renders. Compared frame-to-frame so the menu is
/// only rewritten when something actually moved.
#[derive(Clone, PartialEq)]
pub struct TraySummary {
    pub connected: bool,
    /// Combined CPU across running containers, as a percentage of one core.
    pub cpu_pct: f32,
    /// Combined memory in use across running containers, bytes.
    pub mem_used: u64,
    /// Docker's total on-disk usage, or `None` if the daemon didn't report it.
    pub disk_bytes: Option<u64>,
    pub running: usize,
    pub stopped: usize,
}

#[cfg(feature = "tray")]
mod imp {
    use super::{TrayAction, TraySummary};

    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    /// Menu ids for the two actionable rows. The stat rows are disabled, so
    /// they never fire an event and need no id.
    const ID_SHOW: &str = "rocker:show";
    const ID_QUIT: &str = "rocker:quit";

    /// Source art, way past what a tray slot wants — decode once, trim, shrink.
    const ICON_PNG: &[u8] = include_bytes!("../../../assets/tray.png");
    /// Emitted size. Larger than the ~22px a tray actually paints, so the host
    /// does its own clean downscale rather than us handing it a muddy 64px.
    const ICON_PX: u32 = 128;

    pub struct Tray {
        // Field order matters for drop: the icon must go before the menu it
        // borrows widgets from.
        icon: TrayIcon,
        _menu: Menu,
        rows: Rows,
        last: Option<TraySummary>,
    }

    struct Rows {
        cpu: MenuItem,
        mem: MenuItem,
        disk: MenuItem,
        running: MenuItem,
        stopped: MenuItem,
    }

    impl Tray {
        pub fn new() -> Option<Self> {
            #[cfg(target_os = "linux")]
            if let Err(e) = gtk::init() {
                tracing::warn!(error = %e, "GTK init failed; running without a system tray");
                return None;
            }

            let show = MenuItem::with_id(ID_SHOW, "Show Rocker", true, None);
            let quit = MenuItem::with_id(ID_QUIT, "Quit Rocker", true, None);
            let rows = Rows {
                cpu: MenuItem::new("CPU      —", false, None),
                mem: MenuItem::new("Memory   —", false, None),
                disk: MenuItem::new("Disk     —", false, None),
                running: MenuItem::new("Running  —", false, None),
                stopped: MenuItem::new("Stopped  —", false, None),
            };

            let menu = Menu::new();
            menu.append_items(&[
                &show,
                &PredefinedMenuItem::separator(),
                &rows.cpu,
                &rows.mem,
                &rows.disk,
                &PredefinedMenuItem::separator(),
                &rows.running,
                &rows.stopped,
                &PredefinedMenuItem::separator(),
                &quit,
            ])
            .inspect_err(|e| tracing::warn!(error = %e, "couldn't build tray menu"))
            .ok()?;

            let mut builder = TrayIconBuilder::new()
                .with_id("rocker-tray")
                .with_tooltip("Rocker")
                .with_menu(Box::new(menu.clone()));
            // `tray-icon` doesn't send the icon's pixels over D-Bus — it writes
            // a PNG to disk and hands the StatusNotifierItem host that *path*.
            // Its default (`/tmp`) is private to a Flatpak sandbox, so the host
            // (e.g. GNOME's AppIndicator extension) reads nothing and the icon
            // comes up blank. `XDG_CACHE_HOME` is `~/.var/app/<id>/cache` under
            // Flatpak, the same absolute path inside the sandbox and out.
            #[cfg(target_os = "linux")]
            if let Some(dir) = flatpak_tray_icon_dir() {
                builder = builder.with_temp_dir_path(dir);
            }
            if let Some(icon) = load_icon() {
                builder = builder.with_icon(icon);
            }
            // Elsewhere a left-click means "show the window"; the menu is on
            // the right-click. Linux (libappindicator) ignores this and always
            // opens the menu, which is why the stats live in the menu.
            #[cfg(not(target_os = "linux"))]
            {
                builder = builder.with_menu_on_left_click(false);
            }

            let icon = builder
                .build()
                .inspect_err(|e| tracing::warn!(error = %e, "system tray unavailable"))
                .ok()?;

            Some(Self {
                icon,
                _menu: menu,
                rows,
                last: None,
            })
        }

        /// Called once per egui frame: pump the platform loop and refresh the
        /// menu labels if the numbers moved.
        pub fn render(&mut self, summary: &TraySummary) {
            #[cfg(target_os = "linux")]
            {
                // Let libappindicator / dbusmenu callbacks run. Bounded so a
                // busy bus can't stall the frame.
                let mut budget = 64;
                while budget > 0 && gtk::events_pending() {
                    gtk::main_iteration_do(false);
                    budget -= 1;
                }
            }

            if self.last.as_ref() == Some(summary) {
                return;
            }

            if summary.connected {
                self.rows
                    .cpu
                    .set_text(format!("CPU      {:.0}%", summary.cpu_pct.max(0.0)));
                self.rows.mem.set_text(format!(
                    "Memory   {}",
                    crate::format::bytes(summary.mem_used)
                ));
                self.rows.disk.set_text(match summary.disk_bytes {
                    Some(b) => format!("Disk     {}", crate::format::bytes(b)),
                    None => "Disk     —".to_owned(),
                });
                self.rows
                    .running
                    .set_text(format!("Running  {}", summary.running));
                self.rows
                    .stopped
                    .set_text(format!("Stopped  {}", summary.stopped));
            } else {
                self.rows.cpu.set_text("Docker   not connected");
                self.rows.mem.set_text("Memory   —");
                self.rows.disk.set_text("Disk     —");
                self.rows.running.set_text("Running  —");
                self.rows.stopped.set_text("Stopped  —");
            }

            let _ = self.icon.set_tooltip(Some(if summary.connected {
                format!(
                    "Rocker — {} running, {} stopped",
                    summary.running, summary.stopped
                )
            } else {
                "Rocker — not connected".to_owned()
            }));

            self.last = Some(summary.clone());
        }

        /// Drain the global tray/menu channels. Quit wins over Show if both
        /// are queued.
        pub fn poll(&self) -> Option<TrayAction> {
            let mut show = false;
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                match event.id.as_ref() {
                    ID_QUIT => return Some(TrayAction::Quit),
                    ID_SHOW => show = true,
                    _ => {}
                }
            }
            while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    show = true;
                }
            }
            show.then_some(TrayAction::Show)
        }
    }

    /// Under Flatpak, a directory the tray-icon PNG can live in where the
    /// StatusNotifierItem host can still read it back by the same path.
    /// `None` when not sandboxed (the crate's `/tmp` default is fine there).
    #[cfg(target_os = "linux")]
    fn flatpak_tray_icon_dir() -> Option<std::path::PathBuf> {
        if !std::path::Path::new("/.flatpak-info").exists() {
            return None;
        }
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache"))
            })?;
        Some(base.join("rocker/tray"))
    }

    fn load_icon() -> Option<Icon> {
        let src = image::load_from_memory(ICON_PNG)
            .inspect_err(|e| tracing::warn!(error = %e, "tray icon decode failed"))
            .ok()?
            .to_rgba8();

        // Crop the transparent margin so the mark fills the slot. A source with
        // empty padding scales down to a small shape adrift in a clear box,
        // which at tray size just reads as faded.
        let (w, h) = src.dimensions();
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
        for (x, y, px) in src.enumerate_pixels() {
            if px.0[3] > 8 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        let trimmed = if x1 >= x0 && y1 >= y0 {
            image::imageops::crop_imm(&src, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image()
        } else {
            src
        };

        // Fit into a square keeping aspect ratio (a non-square source stretched
        // straight to NxN would distort), then centre it on transparency.
        let fitted = image::DynamicImage::ImageRgba8(trimmed)
            .resize(ICON_PX, ICON_PX, image::imageops::FilterType::Lanczos3)
            .to_rgba8();
        let (fw, fh) = fitted.dimensions();
        let mut canvas = image::RgbaImage::new(ICON_PX, ICON_PX);
        image::imageops::overlay(
            &mut canvas,
            &fitted,
            ((ICON_PX - fw) / 2) as i64,
            ((ICON_PX - fh) / 2) as i64,
        );

        Icon::from_rgba(canvas.into_raw(), ICON_PX, ICON_PX)
            .inspect_err(|e| tracing::warn!(error = %e, "tray icon build failed"))
            .ok()
    }
}

#[cfg(not(feature = "tray"))]
mod imp {
    use super::{TrayAction, TraySummary};

    /// Stand-in when the `tray` feature is off: constructs to nothing, so the
    /// app runs as a plain window.
    pub struct Tray;

    impl Tray {
        pub fn new() -> Option<Self> {
            None
        }
        pub fn render(&mut self, _summary: &TraySummary) {}
        pub fn poll(&self) -> Option<TrayAction> {
            None
        }
    }
}

pub use imp::Tray;
