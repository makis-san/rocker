//! Process entry point: set up logging and the tokio runtime, then hand control
//! to the `egui` UI. All wiring lives here so the library crates stay portable
//! (PLAN §3.3).

// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::Context as _;

/// App icon shown in the window title bar, taskbar/dock, and Alt-Tab switcher.
const ICON_PNG_BYTES: &[u8] = include_bytes!("../../../assets/icon-1024.png");

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,rocker=debug".into()),
        )
        .init();

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

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rocker")
            .with_inner_size([980.0, 680.0])
            .with_min_inner_size([560.0, 360.0])
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "rocker",
        native_options,
        Box::new(move |cc| Ok(Box::new(rocker_ui::RockerApp::new(cc, handle.clone())))),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;

    Ok(())
}
