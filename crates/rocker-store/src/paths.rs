//! Platform config/data directory resolution.
//!
//! Uses `$XDG_CONFIG_HOME` / `$XDG_DATA_HOME` on Linux with the documented
//! fallbacks, and the OS-appropriate equivalents elsewhere. Kept dependency-free
//! for the scaffold; swap for `directories` if the rules get fussier.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl AppPaths {
    pub fn resolve() -> Self {
        Self {
            config_dir: base(ConfigOrData::Config).join("rocker"),
            data_dir: base(ConfigOrData::Data).join("rocker"),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir.join("themes")
    }

    pub fn history_db(&self) -> PathBuf {
        self.data_dir.join("history.redb")
    }

    /// Where the Logs tab's "Export" writes dumps.
    pub fn exports_dir(&self) -> PathBuf {
        self.data_dir.join("exports")
    }
}

enum ConfigOrData {
    Config,
    Data,
}

fn base(which: ConfigOrData) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);

    #[cfg(target_os = "macos")]
    {
        let sub = match which {
            ConfigOrData::Config => "Library/Application Support",
            ConfigOrData::Data => "Library/Application Support",
        };
        return home
            .map(|h| h.join(sub))
            .unwrap_or_else(|| PathBuf::from("."));
    }

    #[cfg(windows)]
    {
        let _ = &home;
        let var = match which {
            ConfigOrData::Config => "APPDATA",
            ConfigOrData::Data => "LOCALAPPDATA",
        };
        return std::env::var_os(var)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let (var, fallback) = match which {
            ConfigOrData::Config => ("XDG_CONFIG_HOME", ".config"),
            ConfigOrData::Data => ("XDG_DATA_HOME", ".local/share"),
        };
        std::env::var_os(var)
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(fallback)))
            .unwrap_or_else(|| PathBuf::from("."))
    }
}
