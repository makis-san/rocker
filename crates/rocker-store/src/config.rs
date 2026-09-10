//! The hand-editable TOML config file.

use serde::{Deserialize, Serialize};

use rocker_core::{Connection, Group};

use crate::paths::AppPaths;
use crate::{Result, StoreError};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub settings: Settings,
    pub connections: Vec<Connection>,
    pub groups: Vec<Group>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            connections: vec![Connection::local_default()],
            groups: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Theme id: a built-in (`light` / `dark` / `system`) or a file stem.
    pub theme: String,
    /// Hours of usage history to retain (PLAN §5.3).
    pub stats_retention_hours: u32,
    /// Max concurrent stats streams before LRU eviction (PLAN §5.3).
    pub max_stats_streams: usize,
    /// Hide to the system tray instead of quitting when the window is closed
    /// or minimized. The tray icon itself is shown whenever the platform
    /// supports it; this only governs the close/minimize behaviour.
    pub minimize_to_tray: bool,
    /// Launch with the window already hidden to the tray — used when Rocker
    /// starts itself at login.
    pub start_minimized: bool,
    /// Register Rocker to start automatically when you log in.
    pub open_at_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".to_string(),
            stats_retention_hours: 24,
            max_stats_streams: 12,
            minimize_to_tray: true,
            start_minimized: false,
            open_at_login: false,
        }
    }
}

impl Config {
    /// Load from `paths.config_file()`, returning defaults if it does not exist.
    pub fn load(paths: &AppPaths) -> Result<Self> {
        let file = paths.config_file();
        match std::fs::read_to_string(&file) {
            Ok(text) => toml::from_str(&text).map_err(|e| StoreError::Parse(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    /// Write back to `paths.config_file()`, creating the directory if needed.
    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        std::fs::create_dir_all(&paths.config_dir)?;
        let text = toml::to_string_pretty(self).map_err(|e| StoreError::Parse(e.to_string()))?;
        std::fs::write(paths.config_file(), text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config written before the tray/autostart keys existed still loads,
    /// with the new keys taking their defaults rather than `false`.
    #[test]
    fn pre_tray_config_loads_with_sensible_defaults() {
        let text = r#"
            [settings]
            theme = "dark"
            stats_retention_hours = 48
            max_stats_streams = 8
        "#;
        let cfg: Config = toml::from_str(text).expect("legacy config parses");
        assert_eq!(cfg.settings.theme, "dark");
        assert_eq!(cfg.settings.stats_retention_hours, 48);
        assert!(cfg.settings.minimize_to_tray);
        assert!(!cfg.settings.start_minimized);
        assert!(!cfg.settings.open_at_login);
    }
}
