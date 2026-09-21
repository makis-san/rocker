//! Process entry point: set up logging and the tokio runtime, then hand control
//! to the `egui` UI. All wiring lives here so the library crates stay portable
//! (PLAN §3.3).

// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::Context as _;
use rocker_store::{AppPaths, Config};

#[cfg(debug_assertions)]
use std::path::{Path, PathBuf};

#[cfg(debug_assertions)]
use rocker_ext_host::{
    official_registry, ExtensionRegistry, HostError, RegistryClient, RegistryTransport,
};

/// App icon shown in the window title bar, taskbar/dock, and Alt-Tab switcher.
const ICON_PNG_BYTES: &[u8] = include_bytes!("../../../assets/icon-1024.png");

/// Wayland does not let winit hide or restore an existing window. A tray needs
/// both operations, so prefer X11/XWayland when the session makes it available.
#[cfg(target_os = "linux")]
fn should_use_x11_for_tray(tray_requested: bool, x11_display_available: bool) -> bool {
    tray_requested && x11_display_available
}

/// Checked-in registry used to make local `cargo run` sessions representative
/// of a packaged install without fetching extensions from the network.
#[cfg(debug_assertions)]
const LOCAL_REGISTRY_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../registry");

/// Filesystem transport for the checked-in development registry. The index
/// still goes through the regular signature and package verification path.
#[cfg(debug_assertions)]
struct LocalRegistryTransport {
    root: PathBuf,
}

#[cfg(debug_assertions)]
impl RegistryTransport for LocalRegistryTransport {
    fn fetch(&self, url: &str, max_bytes: usize) -> rocker_ext_host::Result<Vec<u8>> {
        let path = if url.ends_with("/index-v1.json") {
            self.root.join("index-v1.json")
        } else if url.ends_with("/index-v1.sig") {
            self.root.join("index-v1.sig")
        } else {
            let package = Path::new(url)
                .file_name()
                .filter(|name| name.to_string_lossy().ends_with(".rockerext"))
                .ok_or_else(|| {
                    HostError::Registry(format!("unexpected local registry URL {url}"))
                })?;
            self.root.join("packages").join(package)
        };
        let bytes = std::fs::read(&path)
            .map_err(|error| HostError::Registry(format!("read {}: {error}", path.display())))?;
        if bytes.len() > max_bytes {
            return Err(HostError::Registry(format!(
                "{} exceeds the {max_bytes} byte limit",
                path.display()
            )));
        }
        Ok(bytes)
    }
}

/// Install every newest release from the checked-in registry during local
/// development. Existing extensions retain their settings, including an
/// explicit decision to disable one; freshly installed extensions are enabled
/// so themes and other local packages are immediately available to exercise.
#[cfg(debug_assertions)]
fn load_local_registry_extensions(paths: &AppPaths) -> anyhow::Result<()> {
    let client = RegistryClient::new(
        official_registry().context("create trusted local registry")?,
        LocalRegistryTransport {
            root: PathBuf::from(LOCAL_REGISTRY_ROOT),
        },
    );
    let index = client
        .fetch_index()
        .context("verify checked-in extension registry")?;
    let mut local =
        ExtensionRegistry::load(paths.extensions_dir()).context("open local extension registry")?;

    for release in index.latest_releases() {
        let package = client.download_package(release).with_context(|| {
            format!("verify local extension {}@{}", release.id, release.version)
        })?;
        match local.install_registry_package(client.registry(), release, &package) {
            Ok(_) => {
                local
                    .set_settings(
                        release.id.clone(),
                        rocker_ext_host::ExtensionSettings {
                            enabled: true,
                            ..Default::default()
                        },
                    )
                    .with_context(|| format!("enable local extension {}", release.id))?;
            }
            Err(HostError::Downgrade { .. }) => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("install local extension {}@{}", release.id, release.version)
                });
            }
        }
    }

    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,rocker=debug".into()),
        )
        .init();

    // `rocker install` / `uninstall` / `self-update` / `doctor` run here and
    // exit before any window or runtime is created; anything else opens the GUI.
    match rocker_setup::cli::run()? {
        rocker_setup::cli::Outcome::Handled(code) => std::process::exit(code),
        rocker_setup::cli::Outcome::LaunchGui => {}
    }

    let paths = AppPaths::resolve();
    #[cfg(debug_assertions)]
    if let Err(error) = load_local_registry_extensions(&paths) {
        tracing::warn!(%error, "local extension registry bootstrap failed");
    }

    // The async engine runs on a multi-thread runtime on background threads; the
    // UI thread never blocks on it.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("rocker-engine")
        .build()
        .context("build tokio runtime")?;
    let handle = runtime.handle().clone();

    let icon =
        eframe::icon_data::from_png_bytes(ICON_PNG_BYTES).expect("bundled app icon is a valid PNG");

    // Honor "start hidden" before the window is ever mapped, so it doesn't
    // flash on screen on its way to the tray.
    let window_settings = Config::load(&paths)
        .map(|config| config.settings)
        .unwrap_or_default();
    let start_hidden = window_settings.start_minimized;
    #[cfg(all(target_os = "linux", feature = "tray"))]
    let tray_requested = window_settings.minimize_to_tray || start_hidden;
    #[cfg(all(target_os = "linux", not(feature = "tray")))]
    let tray_requested = false;

    let mut native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rocker")
            .with_inner_size([980.0, 680.0])
            .with_min_inner_size([560.0, 360.0])
            .with_icon(icon)
            .with_visible(!start_hidden),
        ..Default::default()
    };

    #[cfg(target_os = "linux")]
    if should_use_x11_for_tray(tray_requested, std::env::var_os("DISPLAY").is_some()) {
        use winit::platform::x11::EventLoopBuilderExtX11 as _;

        // `Visible(false)` is a no-op on Wayland in winit 0.30. On X11 it
        // genuinely hides the window, and `Visible(true)` restores it from the
        // tray menu. Only force X11 when the session exposes XWayland.
        native_options.event_loop_builder = Some(Box::new(|builder| {
            builder.with_x11();
        }));
    }

    eframe::run_native(
        "rocker",
        native_options,
        Box::new(move |cc| Ok(Box::new(rocker_ui::RockerApp::new(cc, handle.clone())))),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;

    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::should_use_x11_for_tray;

    #[test]
    fn tray_requested_with_x11_display_uses_restorable_backend() {
        assert!(should_use_x11_for_tray(true, true));
    }

    #[test]
    fn tray_without_x11_display_keeps_the_available_backend() {
        assert!(!should_use_x11_for_tray(true, false));
    }
}
