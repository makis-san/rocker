//! The `egui` front-end. `rocker-ui` owns the design system (`style`), the
//! hand-drawn icon set (`icons`), the composed widgets, and the [`eframe::App`]
//! implementation. The `rocker` binary owns process wiring (runtime, tracing,
//! CLI).

mod app;
mod detail;
mod format;
mod icons;
mod settings;
mod style;
mod terminal;
mod widgets;

pub use app::RockerApp;
pub use style::{install, Palette};
