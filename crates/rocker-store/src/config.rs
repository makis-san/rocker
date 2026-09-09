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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".to_string(),
            stats_retention_hours: 24,
            max_stats_streams: 12,
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
