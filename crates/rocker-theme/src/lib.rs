//! Design tokens and theme loading (PLAN §5.4).
//!
//! A theme is pure data: metadata plus token overrides plus a `mode`. Themes
//! never execute code, so no sandbox is needed. The token schema is versioned
//! (`rocker_core::TOKEN_SCHEMA_VERSION`) because themes *and* extensions freeze
//! against it.
//!
//! The `egui::Visuals` mapping and the `notify`-based hot reload live in
//! `rocker-ui` / the app, so this crate stays UI-toolkit-free.

use serde::{Deserialize, Serialize};

pub use rocker_core::TOKEN_SCHEMA_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Light,
    Dark,
    /// Follows the OS setting.
    Either,
}

/// `#rrggbb` (or `#rrggbbaa`). Kept as a string in the schema so themes stay
/// diff-friendly; parsed at apply time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hex(pub String);

impl Hex {
    /// Parse to `(r, g, b, a)`, defaulting alpha to 255.
    pub fn rgba(&self) -> Option<(u8, u8, u8, u8)> {
        let s = self.0.strip_prefix('#')?;
        let byte = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok();
        match s.len() {
            6 => Some((byte(0)?, byte(2)?, byte(4)?, 255)),
            8 => Some((byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => None,
        }
    }
}

/// The status colors Rocker draws container state with (PLAN §5.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusColors {
    pub running: Hex,
    pub paused: Hex,
    pub exited: Hex,
    pub unhealthy: Hex,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub surface: Hex,
    pub surface_raised: Hex,
    pub text: Hex,
    pub text_muted: Hex,
    pub border: Hex,
    pub accent: Hex,
    pub status: StatusColors,
    /// Terminal 16-color palette, indices 0..=15.
    pub terminal_palette: Vec<Hex>,
    pub radius: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub author: Option<String>,
    pub mode: Mode,
    pub tokens: Tokens,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            schema_version: TOKEN_SCHEMA_VERSION,
            id: "dark".into(),
            name: "Dark".into(),
            author: None,
            mode: Mode::Dark,
            tokens: Tokens {
                // Warm neutral charcoal, deliberately not the stock cool
                // blue-slate dark (PLAN §9 design bar). Real themes: Phase 4.
                surface: Hex("#191816".into()),
                surface_raised: Hex("#211f1c".into()),
                text: Hex("#e9e6e0".into()),
                text_muted: Hex("#9c968b".into()),
                border: Hex("#33302b".into()),
                accent: Hex("#7fa8b8".into()),
                status: StatusColors {
                    running: Hex("#7fb069".into()),
                    paused: Hex("#d0a15e".into()),
                    exited: Hex("#928c82".into()),
                    unhealthy: Hex("#d16b5a".into()),
                },
                terminal_palette: default_terminal_palette(),
                radius: 6.0,
            },
        }
    }

    pub fn light() -> Self {
        Self {
            schema_version: TOKEN_SCHEMA_VERSION,
            id: "light".into(),
            name: "Light".into(),
            author: None,
            mode: Mode::Light,
            tokens: Tokens {
                // A chosen warm off-white, not the UI-kit gray-100 default.
                surface: Hex("#faf9f5".into()),
                surface_raised: Hex("#f1efe8".into()),
                text: Hex("#26241f".into()),
                text_muted: Hex("#6c665b".into()),
                border: Hex("#e0ddd3".into()),
                accent: Hex("#3f7d8c".into()),
                status: StatusColors {
                    running: Hex("#4f7a3d".into()),
                    paused: Hex("#8a6320".into()),
                    exited: Hex("#6c665b".into()),
                    unhealthy: Hex("#a8412f".into()),
                },
                terminal_palette: default_terminal_palette(),
                radius: 6.0,
            },
        }
    }

    pub fn built_ins() -> Vec<Theme> {
        vec![Self::light(), Self::dark()]
    }

    /// Parse a theme from TOML, rejecting a mismatched schema version.
    pub fn from_toml(text: &str) -> Result<Self, ThemeError> {
        let theme: Theme = toml_from_str(text)?;
        if theme.schema_version != TOKEN_SCHEMA_VERSION {
            return Err(ThemeError::SchemaMismatch {
                found: theme.schema_version,
                expected: TOKEN_SCHEMA_VERSION,
            });
        }
        Ok(theme)
    }
}

fn default_terminal_palette() -> Vec<Hex> {
    [
        "#1e1e1e", "#f85149", "#3fb950", "#d29922", "#4c8dff", "#bc8cff", "#39c5cf", "#b1bac4",
        "#6e7681", "#ff7b72", "#56d364", "#e3b341", "#79c0ff", "#d2a8ff", "#56d4dd", "#f0f6fc",
    ]
    .into_iter()
    .map(|s| Hex(s.to_string()))
    .collect()
}

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ThemeError {
    #[error("theme schema version {found}, this build expects {expected}")]
    SchemaMismatch { found: u32, expected: u32 },
    #[error("parse: {0}")]
    Parse(String),
}

// Kept behind a shim so the `toml` dependency is added by `cargo add` at the
// same version as the rest of the workspace.
fn toml_from_str<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, ThemeError> {
    toml::from_str(text).map_err(|e| ThemeError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parses() {
        assert_eq!(Hex("#ff8800".into()).rgba(), Some((255, 136, 0, 255)));
        assert_eq!(Hex("#00000080".into()).rgba(), Some((0, 0, 0, 128)));
        assert_eq!(Hex("nope".into()).rgba(), None);
    }

    #[test]
    fn built_ins_roundtrip_through_toml() {
        for t in Theme::built_ins() {
            let s = toml::to_string(&t).unwrap();
            let back = Theme::from_toml(&s).unwrap();
            assert_eq!(back, t);
        }
    }
}
