//! The hand-editable TOML config file.

use serde::{Deserialize, Serialize};

use rocker_core::{Connection, Group, KubernetesWorkloadRef};

use crate::paths::AppPaths;
use crate::{Result, StoreError};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub settings: Settings,
    pub connections: Vec<Connection>,
    /// Extra kubeconfig files to include alongside the current device's
    /// default kubeconfig and any paths named by `KUBECONFIG`.
    pub kubernetes_kubeconfigs: Vec<KubernetesKubeconfig>,
    /// Managed workloads Rocker scaled to zero, with their replica count kept
    /// so a later Start restores the workload rather than guessing a value.
    pub kubernetes_paused_workloads: Vec<PausedKubernetesWorkload>,
    pub groups: Vec<Group>,
    /// Signed extension catalogs the user has chosen to browse.
    pub extension_registries: Vec<ExtensionRegistrySource>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            connections: vec![Connection::local_default()],
            kubernetes_kubeconfigs: Vec::new(),
            kubernetes_paused_workloads: Vec::new(),
            groups: Vec::new(),
            extension_registries: vec![ExtensionRegistrySource::official()],
        }
    }
}

/// A workload paused through Rocker’s Kubernetes controls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PausedKubernetesWorkload {
    /// The workload that was scaled down.
    #[serde(flatten)]
    pub workload: KubernetesWorkloadRef,
    /// Replica count immediately before Rocker paused the workload.
    pub replicas: u32,
}

/// An additional local kubeconfig file selected in Kubernetes settings.
///
/// Rocker reads contexts from this file on the current device. Credentials
/// remain in the kubeconfig and never cross an extension boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KubernetesKubeconfig {
    /// Human-friendly source name shown in Settings.
    pub name: String,
    /// Absolute or home-relative path to a kubeconfig YAML file.
    pub path: String,
}

/// A user-trusted extension registry.
///
/// Registries use a detached Ed25519 signature. The public key is stored here
/// rather than accepted from a downloaded index, so adding a registry is an
/// explicit trust decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionRegistrySource {
    /// Stable identifier used for installed-package provenance.
    pub id: String,
    /// HTTPS location of the signed registry index.
    pub index_url: String,
    /// HTTPS location of the detached index signature.
    pub signature_url: String,
    /// Hex-encoded, 32-byte Ed25519 public key.
    pub public_key: String,
}

impl ExtensionRegistrySource {
    /// Rocker's curated registry, enabled for new configurations.
    pub fn official() -> Self {
        Self {
            id: rocker_core::registry::OFFICIAL_REGISTRY_ID.to_string(),
            index_url: rocker_core::registry::OFFICIAL_REGISTRY_INDEX_URL.to_string(),
            signature_url: rocker_core::registry::OFFICIAL_REGISTRY_SIGNATURE_URL.to_string(),
            public_key: rocker_core::registry::OFFICIAL_REGISTRY_PUBLIC_KEY
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Theme id: a built-in (`light` / `dark` / `system`), or `custom` to use
    /// a variant of an installed `Tier::Theme` extension named by
    /// `theme_extension` / `theme_variant`.
    pub theme: String,
    /// The installed theme extension id to use when `theme == "custom"`.
    #[serde(default)]
    pub theme_extension: Option<String>,
    /// The variant id (within `theme_extension`) to use when
    /// `theme == "custom"`.
    #[serde(default)]
    pub theme_variant: Option<String>,
    /// Hours of usage history to retain (PLAN §5.3).
    pub stats_retention_hours: u32,
    /// Max concurrent stats streams before LRU eviction (PLAN §5.3).
    pub max_stats_streams: usize,
    /// Hide to the system tray instead of quitting when the window is closed
    /// or minimized. When this and `start_minimized` are both off there is no
    /// tray icon at all and closing the window quits Rocker.
    pub minimize_to_tray: bool,
    /// Launch with the window already hidden to the tray — used when Rocker
    /// starts itself at login.
    pub start_minimized: bool,
    /// Register Rocker to start automatically when you log in.
    pub open_at_login: bool,
    /// Query discovered Kubernetes contexts for pods and usage. Off by
    /// default: enabling it queries every context in every kubeconfig on
    /// this device, which can reach production clusters and trigger
    /// exec-credential plugins (cloud auth, MFA) on every launch.
    #[serde(default)]
    pub kubernetes_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".to_string(),
            theme_extension: None,
            theme_variant: None,
            stats_retention_hours: 24,
            max_stats_streams: 12,
            minimize_to_tray: true,
            start_minimized: false,
            open_at_login: false,
            kubernetes_enabled: false,
        }
    }
}

impl Config {
    /// Load from `paths.config_file()`, returning defaults if it does not exist.
    pub fn load(paths: &AppPaths) -> Result<Self> {
        let file = paths.config_file();
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                let mut config: Self =
                    toml::from_str(&text).map_err(|e| StoreError::Parse(e.to_string()))?;
                config.migrate_official_registry_key();
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    // Repair only the exact default shipped before the registry key rotation.
    // Custom keys, URLs, and explicitly removed registries remain user choices.
    fn migrate_official_registry_key(&mut self) {
        let official = ExtensionRegistrySource::official();
        for source in &mut self.extension_registries {
            if source.id == official.id
                && source.index_url == official.index_url
                && source.signature_url == official.signature_url
                && source.public_key
                    == "21bc5889a2e5293ee6a22da5678f0497e90b2c67a2c55fd79f1ca0434af21e0a"
            {
                source.public_key.clone_from(&official.public_key);
            }
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

    #[test]
    fn registry_migration_preserves_user_trust_choices() {
        let mut legacy = ExtensionRegistrySource::official();
        legacy.public_key =
            "21bc5889a2e5293ee6a22da5678f0497e90b2c67a2c55fd79f1ca0434af21e0a".into();
        let mut custom_id = legacy.clone();
        custom_id.id = "custom".into();
        let mut custom_index = legacy.clone();
        custom_index.index_url = "https://example.com/index.json".into();
        let mut custom_signature = legacy.clone();
        custom_signature.signature_url = "https://example.com/index.sig".into();
        let mut custom_key = legacy;
        custom_key.public_key = "07".repeat(32);
        for registries in [
            vec![],
            vec![custom_id, custom_index, custom_signature, custom_key],
            vec![ExtensionRegistrySource::official()],
        ] {
            let mut config = Config {
                extension_registries: registries.clone(),
                ..Config::default()
            };
            config.migrate_official_registry_key();
            assert_eq!(config.extension_registries, registries);
        }
    }

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
        assert_eq!(
            cfg.extension_registries,
            vec![ExtensionRegistrySource::official()]
        );
        assert!(cfg.kubernetes_paused_workloads.is_empty());
    }
}
