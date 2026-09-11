//! The extension contract (PLAN §5.5).
//!
//! Extensions do not run render code against an immediate-mode host. They return
//! a declarative [`UiNode`] tree that the host draws with `egui` and routes
//! events back over. This crate holds the manifest, the capability set, and that
//! node vocabulary — shared by `rocker-ext-host` and (via generated bindings)
//! the WIT world in `wit/world.wit`.

use std::path::{Component, Path};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Deny-by-default capabilities, granted at install and revocable (PLAN §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    ContainersRead,
    ContainersLifecycle,
    ContainersExec,
    LogsRead,
    StatsRead,
    ImagesRead,
    RegistriesRead,
    Network,
    Storage,
    Notifications,
}

/// The container lifecycle operations an extension may request from Rocker.
///
/// This is an intent only: the application checks
/// [`Capability::ContainersLifecycle`] and executes the Docker API call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Kill,
}

impl ContainerAction {
    /// Parse the stable lowercase spellings used by the scripting API.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            "pause" => Some(Self::Pause),
            "unpause" => Some(Self::Unpause),
            "kill" => Some(Self::Kill),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// `rhai` scripting (Phase 5, ships first).
    Script,
    /// `wasmtime` component (Phase 5, second).
    Component,
    /// Pure data: design tokens only, no code and no capabilities. Ships one
    /// or more named variants (PLAN §5.4 theme extensions).
    Theme,
}

/// One named look a [`Tier::Theme`] extension ships. `file` is a
/// `rocker_theme::Theme`-schema TOML document, relative to the extension
/// folder — parsed by the UI layer, which owns the theme token schema, not by
/// this crate or the host supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeVariant {
    /// Stable id, e.g. `"mocha"`. Used as a configuration key.
    pub id: String,
    /// Shown in the variant picker, e.g. `"Mocha"`.
    pub name: String,
    /// Path to the variant's theme TOML, relative to the extension folder.
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub tier: Tier,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Extension entry point, relative to the extension folder. Required for
    /// the executable tiers (`Script`, `Component`); absent for `Theme`,
    /// which has no code to run.
    #[serde(default)]
    pub entry: Option<String>,
    /// Optional interval for invoking the script's `on_schedule` hook.
    #[serde(default)]
    pub schedule_seconds: Option<u64>,
    /// The looks this extension ships. Populated only for `Tier::Theme`,
    /// which must declare at least one.
    #[serde(default)]
    pub theme_variants: Vec<ThemeVariant>,
}

/// A manifest that would let an extension escape its installation directory or
/// cannot be addressed reliably by Rocker.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestError {
    /// Extension identifiers are used as stable configuration keys and local
    /// directory names, so their vocabulary is deliberately narrow.
    #[error("extension id `{id}` must contain only lowercase ASCII letters, digits, `-`, or `.`")]
    InvalidId { id: String },
    /// The entry point must remain inside the installed extension directory.
    #[error("extension entry `{entry}` must be a relative path without `..` components")]
    InvalidEntry { entry: String },
    /// Repeated capabilities would make install prompts and configuration
    /// comparisons ambiguous.
    #[error("extension `{id}` declares capability {cap:?} more than once")]
    DuplicateCapability { id: String, cap: Capability },
    /// Scheduled scripts must wait at least one second between evaluations.
    #[error("extension `{id}` must use a schedule of at least one second")]
    InvalidSchedule { id: String },
    /// `Script` and `Component` extensions have code to run and must name it.
    #[error("extension `{id}` has tier {tier:?} and must declare an entry")]
    MissingEntry { id: String, tier: Tier },
    /// A `Theme` extension has no code, so an entry would be meaningless.
    #[error("theme extension `{id}` must not declare an entry")]
    UnexpectedEntry { id: String },
    /// Themes are pure data; a capability request from one would be a sign of
    /// a mismatched manifest rather than something to prompt the user for.
    #[error("theme extension `{id}` must not request capabilities")]
    ThemeCapabilitiesNotAllowed { id: String },
    /// Only a `Theme` extension carries variants — an executable extension
    /// listing them would just be dead manifest data.
    #[error("extension `{id}` has tier {tier:?} and must not declare theme variants")]
    UnexpectedThemeVariants { id: String, tier: Tier },
    /// A theme has nothing to apply without at least one named look.
    #[error("theme extension `{id}` must declare at least one variant")]
    NoThemeVariants { id: String },
    /// Variant ids share the same narrow vocabulary as extension ids, for the
    /// same reason: they are used as stable configuration keys.
    #[error(
        "theme extension `{id}` variant id `{variant}` must contain only lowercase ASCII \
         letters, digits, `-`, or `.`"
    )]
    InvalidThemeVariantId { id: String, variant: String },
    /// Two variants sharing an id would make a stored selection ambiguous.
    #[error("theme extension `{id}` declares variant id `{variant}` more than once")]
    DuplicateThemeVariant { id: String, variant: String },
    /// A variant's theme file is subject to the same directory-escape rule as
    /// an executable extension's entry point.
    #[error("theme extension `{id}` variant `{variant}` file `{file}` must be a relative path without `..` components")]
    InvalidThemeVariantFile {
        id: String,
        variant: String,
        file: String,
    },
}

/// Shared by an executable extension's `entry` and a theme variant's `file`:
/// both must be a non-empty relative path that stays inside the extension's
/// own installation directory.
fn is_safe_relative_path(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty()
        && !p.is_absolute()
        && !p.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}

impl Manifest {
    /// Validate fields which define the local extension boundary.
    ///
    /// This does not grant the requested capabilities. Installers must obtain
    /// those grants explicitly before activating the extension.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.id.is_empty()
            || !self.id.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
            })
        {
            return Err(ManifestError::InvalidId {
                id: self.id.clone(),
            });
        }

        match self.tier {
            Tier::Theme => {
                if self.entry.is_some() {
                    return Err(ManifestError::UnexpectedEntry {
                        id: self.id.clone(),
                    });
                }
                if !self.capabilities.is_empty() {
                    return Err(ManifestError::ThemeCapabilitiesNotAllowed {
                        id: self.id.clone(),
                    });
                }
                if self.theme_variants.is_empty() {
                    return Err(ManifestError::NoThemeVariants {
                        id: self.id.clone(),
                    });
                }
                let mut variant_ids = std::collections::HashSet::new();
                for variant in &self.theme_variants {
                    if variant.id.is_empty()
                        || !variant.id.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || matches!(byte, b'-' | b'.')
                        })
                    {
                        return Err(ManifestError::InvalidThemeVariantId {
                            id: self.id.clone(),
                            variant: variant.id.clone(),
                        });
                    }
                    if !variant_ids.insert(variant.id.clone()) {
                        return Err(ManifestError::DuplicateThemeVariant {
                            id: self.id.clone(),
                            variant: variant.id.clone(),
                        });
                    }
                    if !is_safe_relative_path(&variant.file) {
                        return Err(ManifestError::InvalidThemeVariantFile {
                            id: self.id.clone(),
                            variant: variant.id.clone(),
                            file: variant.file.clone(),
                        });
                    }
                }
            }
            Tier::Script | Tier::Component => {
                let entry = self
                    .entry
                    .as_deref()
                    .ok_or_else(|| ManifestError::MissingEntry {
                        id: self.id.clone(),
                        tier: self.tier,
                    })?;
                if !is_safe_relative_path(entry) {
                    return Err(ManifestError::InvalidEntry {
                        entry: entry.to_owned(),
                    });
                }
                if !self.theme_variants.is_empty() {
                    return Err(ManifestError::UnexpectedThemeVariants {
                        id: self.id.clone(),
                        tier: self.tier,
                    });
                }
            }
        }

        let mut capabilities = std::collections::HashSet::new();
        for capability in &self.capabilities {
            if !capabilities.insert(*capability) {
                return Err(ManifestError::DuplicateCapability {
                    id: self.id.clone(),
                    cap: *capability,
                });
            }
        }

        if self.schedule_seconds == Some(0) {
            return Err(ManifestError::InvalidSchedule {
                id: self.id.clone(),
            });
        }

        Ok(())
    }

    /// Return the optional recurring script interval.
    pub fn schedule_interval(&self) -> Option<Duration> {
        self.schedule_seconds.map(Duration::from_secs)
    }
}

/// The constrained UI vocabulary for v1 (PLAN §5.5). Expanded from real
/// extension needs, not up front.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "node")]
pub enum UiNode {
    Label {
        text: String,
    },
    Button {
        id: String,
        text: String,
    },
    TextInput {
        id: String,
        value: String,
    },
    Row {
        children: Vec<UiNode>,
    },
    Column {
        children: Vec<UiNode>,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

/// An event the host routes back to the extension after a render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum UiEvent {
    Clicked { id: String },
    Changed { id: String, value: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest {
            id: "example.extension".into(),
            name: "Example".into(),
            version: "0.1.0".into(),
            tier: Tier::Script,
            capabilities: vec![Capability::ContainersRead],
            entry: Some("main.rhai".into()),
            schedule_seconds: None,
            theme_variants: Vec::new(),
        }
    }

    fn theme_manifest() -> Manifest {
        Manifest {
            id: "example.theme".into(),
            name: "Example Theme".into(),
            version: "0.1.0".into(),
            tier: Tier::Theme,
            capabilities: Vec::new(),
            entry: None,
            schedule_seconds: None,
            theme_variants: vec![ThemeVariant {
                id: "mocha".into(),
                name: "Mocha".into(),
                file: "mocha.toml".into(),
            }],
        }
    }

    #[test]
    fn validate_accepts_a_safe_manifest() {
        assert_eq!(manifest().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_parent_entry_paths() {
        let mut manifest = manifest();
        manifest.entry = Some("../outside.rhai".into());

        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::InvalidEntry { .. })
        ));
    }

    #[test]
    fn validate_rejects_duplicate_capabilities() {
        let mut manifest = manifest();
        manifest.capabilities.push(Capability::ContainersRead);

        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::DuplicateCapability { .. })
        ));
    }

    #[test]
    fn validate_rejects_a_script_with_no_entry() {
        let mut manifest = manifest();
        manifest.entry = None;

        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::MissingEntry { .. })
        ));
    }

    #[test]
    fn validate_accepts_a_safe_theme() {
        assert_eq!(theme_manifest().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_a_theme_with_an_entry() {
        let mut manifest = theme_manifest();
        manifest.entry = Some("main.rhai".into());

        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::UnexpectedEntry { .. })
        ));
    }

    #[test]
    fn validate_rejects_a_theme_requesting_capabilities() {
        let mut manifest = theme_manifest();
        manifest.capabilities.push(Capability::ContainersRead);

        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::ThemeCapabilitiesNotAllowed { .. })
        ));
    }

    #[test]
    fn validate_rejects_a_theme_with_no_variants() {
        let mut manifest = theme_manifest();
        manifest.theme_variants.clear();

        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::NoThemeVariants { .. })
        ));
    }

    #[test]
    fn validate_rejects_duplicate_variant_ids() {
        let mut manifest = theme_manifest();
        let mocha = manifest.theme_variants[0].clone();
        manifest.theme_variants.push(mocha);

        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::DuplicateThemeVariant { .. })
        ));
    }

    #[test]
    fn validate_rejects_a_variant_file_escaping_the_extension_dir() {
        let mut manifest = theme_manifest();
        manifest.theme_variants[0].file = "../outside.toml".into();

        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::InvalidThemeVariantFile { .. })
        ));
    }

    #[test]
    fn validate_rejects_a_script_declaring_theme_variants() {
        let mut manifest = manifest();
        manifest.theme_variants.push(ThemeVariant {
            id: "mocha".into(),
            name: "Mocha".into(),
            file: "mocha.toml".into(),
        });

        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::UnexpectedThemeVariants { .. })
        ));
    }
}
