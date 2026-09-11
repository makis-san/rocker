//! The Extensions view (PLAN §5.5: two tiers, both Rust-native).
//!
//! One centered column, same shape as [`crate::groups`]: a list of installed
//! extensions, each an always-editable card — enabled / dev-mode toggles and a
//! checklist of exactly the capabilities its manifest requests, nothing more.
//! Grants are persisted straight through `ExtensionRegistry::set_settings` as
//! they're edited (unlike Groups/Settings, which hand an edit back for the
//! caller to save — there is no separate app-level config these belong in).
//! Broken extension folders are listed, not hidden, so a bad install stays
//! visible instead of silently vanishing from the list.
//!
//! This module also owns [`render_ui_node`], the pure renderer for the
//! declarative [`UiNode`] vocabulary extensions return instead of running
//! render code themselves. It draws with the same widgets as the rest of the
//! app and turns interaction into a [`UiEvent`] the host would route back to
//! the extension. It isn't wired to a live `ExtensionSupervisor` yet — that
//! needs the isolated host process's request/response protocol, which is
//! still being extended (PLAN §10, Phase 5) — so it's exercised here by tests
//! against hand-built trees rather than a live panel on screen.

use egui::{vec2, Align, Layout, RichText, Sense, Stroke};

use rocker_ext_api::{Capability, Tier, UiEvent, UiNode};
use rocker_ext_host::{
    Discovery, ExtensionRegistry, HostError, InstalledExtension, TrustedRegistry,
};
use rocker_store::{AppPaths, ExtensionRegistrySource};
use rocker_theme::Theme;

use crate::icons::{self, Icon};
use crate::style::{self, Palette};
use crate::widgets::segmented;

const COLUMN_W: f32 = 560.0;

/// Recursion ceiling for [`render_ui_node`]. Extensions are untrusted, so a
/// pathological or cyclical-looking tree can't blow the call stack — anything
/// past this depth renders as a quiet placeholder instead of recursing further.
const MAX_NODE_DEPTH: u8 = 24;

/// Owns the local extension registry and the last discovery pass. The
/// registry may fail to load (a permissions problem, a corrupt state file);
/// that's kept as a message rather than a hard error so the rest of the app
/// still runs with extensions simply unavailable.
pub struct ExtensionsScreen {
    registry: Result<ExtensionRegistry, String>,
    discovery: Discovery,
    registry_draft: RegistryDraft,
    tab: ExtensionsTab,
}

#[derive(Default)]
struct RegistryDraft {
    id: String,
    index_url: String,
    signature_url: String,
    public_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtensionsTab {
    Installed,
    Browse,
    Registries,
}

impl ExtensionsTab {
    const ALL: [Self; 3] = [Self::Installed, Self::Browse, Self::Registries];

    fn label(self) -> &'static str {
        match self {
            Self::Installed => "Installed Extensions",
            Self::Browse => "Browse",
            Self::Registries => "Extensions Registry",
        }
    }
}

impl ExtensionsScreen {
    /// Open the local registry under `paths.extensions_dir()` and run one
    /// discovery pass.
    pub fn new(paths: &AppPaths) -> Self {
        let root = paths.extensions_dir();
        match ExtensionRegistry::load(root) {
            Ok(registry) => {
                let discovery = registry.discover().unwrap_or_else(|err| {
                    tracing::warn!(%err, "extension discovery failed");
                    Discovery::default()
                });
                Self {
                    registry: Ok(registry),
                    discovery,
                    registry_draft: RegistryDraft::default(),
                    tab: ExtensionsTab::Installed,
                }
            }
            Err(err) => {
                tracing::warn!(%err, "extension registry failed to load");
                Self {
                    registry: Err(err.to_string()),
                    discovery: Discovery::default(),
                    registry_draft: RegistryDraft::default(),
                    tab: ExtensionsTab::Installed,
                }
            }
        }
    }

    /// Re-scan the local extensions folder. Real directory I/O, so this runs
    /// on demand (a press of the screen's refresh button) rather than every
    /// frame.
    fn refresh(&mut self) {
        let Ok(registry) = &self.registry else {
            return;
        };
        self.discovery = registry.discover().unwrap_or_else(|err| {
            tracing::warn!(%err, "extension discovery failed");
            Discovery::default()
        });
    }

    /// The last discovery pass, for callers (the Settings screen's theme
    /// picker) that only need to read installed extensions rather than draw
    /// this whole screen.
    pub fn discovery(&self) -> &Discovery {
        &self.discovery
    }
}

/// One installed, enabled `Tier::Theme` extension, shaped for the Settings
/// screen's theme picker — a name to show and the variants it ships, without
/// pulling in the rest of [`InstalledExtension`]'s bookkeeping.
pub struct ThemeExtensionOption {
    pub id: String,
    pub name: String,
    /// `(variant id, variant name)`, in manifest order.
    pub variants: Vec<(String, String)>,
}

/// Every installed, enabled theme extension, sorted by name — installed but
/// disabled ones are left out, the same gate the Extensions screen uses for
/// running an extension's code (PLAN §5.5), even though a theme has none.
pub fn theme_extension_options(discovery: &Discovery) -> Vec<ThemeExtensionOption> {
    let mut options: Vec<ThemeExtensionOption> = discovery
        .extensions
        .iter()
        .filter(|ext| ext.manifest.tier == Tier::Theme && ext.settings.enabled)
        .map(|ext| ThemeExtensionOption {
            id: ext.manifest.id.clone(),
            name: ext.manifest.name.clone(),
            variants: ext
                .manifest
                .theme_variants
                .iter()
                .map(|v| (v.id.clone(), v.name.clone()))
                .collect(),
        })
        .collect();
    options.sort_by(|a, b| a.name.cmp(&b.name));
    options
}

/// Resolve one variant of one installed theme extension into a concrete
/// [`Theme`], or `None` if the extension was uninstalled, disabled, or the
/// variant no longer exists — the caller falls back to a built-in theme
/// rather than erroring, the same way [`ExtensionRegistry::discover`] reports
/// a broken extension without taking down the rest of the app.
pub fn resolve_custom_theme(
    discovery: &Discovery,
    extension_id: &str,
    variant_id: &str,
) -> Option<Theme> {
    let ext = discovery.extensions.iter().find(|ext| {
        ext.manifest.id == extension_id && ext.manifest.tier == Tier::Theme && ext.settings.enabled
    })?;
    let variant = ext
        .manifest
        .theme_variants
        .iter()
        .find(|v| v.id == variant_id)
        .or_else(|| ext.manifest.theme_variants.first())?;
    let text = std::fs::read_to_string(ext.directory.join(&variant.file)).ok()?;
    Theme::from_toml(&text).ok()
}

/// What happened this frame. `None` from [`extensions_screen`] means nothing
/// did. Unlike [`crate::settings::Edit`], a change here is already
/// persisted — `error` carries a save failure the caller should surface,
/// rather than something still waiting to be written.
pub struct Edit {
    pub error: Option<String>,
    /// The app config changed and should be saved by the caller.
    pub registries_changed: bool,
}

/// Render the Extensions screen.
pub fn extensions_screen(
    ui: &mut egui::Ui,
    pal: &Palette,
    screen: &mut ExtensionsScreen,
    registries: &mut Vec<ExtensionRegistrySource>,
) -> Option<Edit> {
    let mut changed = false;
    let mut registries_changed = false;
    let mut error = None;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let full = ui.available_width();
            let col = COLUMN_W.min(full - 4.0);
            let side = ((full - col) * 0.5).max(0.0);

            ui.horizontal(|ui| {
                ui.add_space(side);
                ui.vertical(|ui| {
                    ui.set_width(col);
                    ui.add_space(6.0);

                    ui.label(
                        RichText::new("Extensions")
                            .size(19.0)
                            .strong()
                            .color(pal.text),
                    );
                    ui.add_space(10.0);
                    extensions_tabstrip(ui, pal, &mut screen.tab);
                    ui.add_space(16.0);

                    match screen.tab {
                        ExtensionsTab::Installed => {
                            installed_extensions_section(ui, pal, screen, &mut changed, &mut error)
                        }
                        ExtensionsTab::Browse => browse_extensions_section(ui, pal, registries),
                        ExtensionsTab::Registries => {
                            if registry_sources_section(
                                ui,
                                pal,
                                registries,
                                &mut screen.registry_draft,
                                &mut error,
                            ) {
                                registries_changed = true;
                            }
                        }
                    }

                    ui.add_space(30.0);
                });
            });
        });

    (changed || registries_changed || error.is_some()).then_some(Edit {
        error,
        registries_changed,
    })
}

fn extensions_tabstrip(ui: &mut egui::Ui, pal: &Palette, selected: &mut ExtensionsTab) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        for tab in ExtensionsTab::ALL {
            let active = *selected == tab;
            if icons::toggle_text_button(ui, pal, tab.label(), active, tab.label()).clicked() {
                *selected = tab;
            }
        }
    });
}

fn installed_extensions_section(
    ui: &mut egui::Ui,
    pal: &Palette,
    screen: &mut ExtensionsScreen,
    changed: &mut bool,
    error: &mut Option<String>,
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                RichText::new("Installed extensions")
                    .strong()
                    .color(pal.text),
            );
            ui.label(
                RichText::new("Installed on this computer. Permissions reflect each manifest.")
                    .small()
                    .color(pal.text_muted),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icons::icon_button(ui, pal, Icon::Refresh, None, "Rescan the extensions folder")
                .clicked()
            {
                screen.refresh();
            }
        });
    });
    ui.add_space(10.0);

    match &mut screen.registry {
        Err(load_err) => broken_banner(ui, pal, "Extensions folder unavailable", load_err.as_str()),
        Ok(registry) => {
            if !screen.discovery.failures.is_empty() {
                failures_section(ui, pal, &screen.discovery.failures);
                ui.add_space(14.0);
            }
            if screen.discovery.extensions.is_empty() && screen.discovery.failures.is_empty() {
                ui.add_space(10.0);
                ui.label(RichText::new("No extensions installed yet.").color(pal.text_faint));
            } else {
                for ext in &mut screen.discovery.extensions {
                    ui.add_space(10.0);
                    if extension_card(ui, pal, registry, ext, error) {
                        *changed = true;
                    }
                }
            }
        }
    }
}

fn browse_extensions_section(
    ui: &mut egui::Ui,
    pal: &Palette,
    registries: &[ExtensionRegistrySource],
) {
    ui.label(RichText::new("Browse extensions").strong().color(pal.text));
    ui.add_space(10.0);
    if registries.is_empty() {
        ui.label(RichText::new("Add a registry to browse extensions.").color(pal.text_faint));
        return;
    }
    for source in registries {
        registry_catalog_card(ui, pal, source, false);
        ui.add_space(6.0);
    }
}

/// Render and edit the trusted catalog list. A registry can be removed even
/// when it is the compiled-in default: default only means new installations
/// start with it, never that it is forced on an existing user.
fn registry_sources_section(
    ui: &mut egui::Ui,
    pal: &Palette,
    registries: &mut Vec<ExtensionRegistrySource>,
    draft: &mut RegistryDraft,
    error: &mut Option<String>,
) -> bool {
    let mut changed = false;
    ui.label(
        RichText::new("Extension registries")
            .size(15.0)
            .strong()
            .color(pal.text),
    );
    ui.add_space(3.0);
    ui.label(
        RichText::new("Browse signed catalogs you trust. Removing one keeps extensions already installed from it.")
            .small()
            .color(pal.text_muted),
    );
    ui.add_space(8.0);

    let mut remove = None;
    for (index, source) in registries.iter().enumerate() {
        if registry_catalog_card(ui, pal, source, true) {
            remove = Some(index);
        }
        ui.add_space(6.0);
    }
    if let Some(index) = remove {
        registries.remove(index);
        changed = true;
    }

    egui::CollapsingHeader::new(
        RichText::new("Add registry")
            .small()
            .strong()
            .color(pal.text),
    )
    .id_salt("add-extension-registry")
    .show(ui, |ui| {
        ui.add_space(6.0);
        ui.label(
            RichText::new("Add only catalogs whose signing key you trust.")
                .small()
                .color(pal.text_muted),
        );
        ui.add_space(6.0);
        registry_field(ui, pal, "ID", "e.g. community", &mut draft.id);
        registry_field(
            ui,
            pal,
            "Index URL",
            "https://…/index-v1.json",
            &mut draft.index_url,
        );
        registry_field(
            ui,
            pal,
            "Signature URL",
            "https://…/index-v1.sig",
            &mut draft.signature_url,
        );
        registry_field(
            ui,
            pal,
            "Public key",
            "64-character Ed25519 hex key",
            &mut draft.public_key,
        );
        ui.add_space(4.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icons::primary_button(ui, pal, "Add registry").clicked() {
                match registry_from_draft(draft) {
                    Ok(source) if registries.iter().any(|item| item.id == source.id) => {
                        *error = Some(format!("A registry named `{}` already exists.", source.id));
                    }
                    Ok(source) => {
                        registries.push(source);
                        *draft = RegistryDraft::default();
                        changed = true;
                    }
                    Err(message) => *error = Some(message),
                }
            }
        });
    });
    changed
}

/// A compact registry identity. GitHub-backed catalogs deliberately show the
/// repository rather than an implementation-specific raw index URL.
fn registry_catalog_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    source: &ExtensionRegistrySource,
    removable: bool,
) -> bool {
    let mut remove = false;
    egui::Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(style::radius(pal.corner))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(&source.id).strong().color(pal.text));
                    if let Some((name, url)) = github_repository(&source.index_url) {
                        let link = ui.link(RichText::new(name).small().color(pal.accent));
                        if link.clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                        }
                    } else {
                        ui.label(
                            RichText::new("Signed HTTPS registry")
                                .small()
                                .color(pal.text_muted),
                        );
                    }
                });
                if removable {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icons::text_button(ui, pal, "Remove").clicked() {
                            remove = true;
                        }
                    });
                }
            });
        });
    remove
}

/// Return the GitHub repository represented by an ordinary or raw-content URL.
fn github_repository(index_url: &str) -> Option<(String, String)> {
    let path = index_url
        .strip_prefix("https://github.com/")
        .or_else(|| index_url.strip_prefix("https://raw.githubusercontent.com/"))?;
    let mut parts = path.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((
        format!("{owner}/{repo}"),
        format!("https://github.com/{owner}/{repo}"),
    ))
}

fn registry_field(ui: &mut egui::Ui, pal: &Palette, label: &str, hint: &str, value: &mut String) {
    ui.label(RichText::new(label).small().color(pal.text_muted));
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(f32::INFINITY)
            .text_color(pal.text),
    );
    ui.add_space(4.0);
}

fn registry_from_draft(draft: &RegistryDraft) -> Result<ExtensionRegistrySource, String> {
    let public_key = decode_public_key(&draft.public_key)?;
    TrustedRegistry::new(
        draft.id.trim(),
        draft.index_url.trim(),
        draft.signature_url.trim(),
        public_key,
    )
    .map_err(|err| err.to_string())?;
    Ok(ExtensionRegistrySource {
        id: draft.id.trim().to_owned(),
        index_url: draft.index_url.trim().to_owned(),
        signature_url: draft.signature_url.trim().to_owned(),
        public_key: draft.public_key.trim().to_ascii_lowercase(),
    })
}

fn decode_public_key(value: &str) -> Result<[u8; 32], String> {
    let value = value.trim();
    if value.len() != 64 {
        return Err("The public key must be 64 hexadecimal characters.".to_string());
    }
    let mut key = [0_u8; 32];
    for (offset, byte) in key.iter_mut().enumerate() {
        let start = offset * 2;
        *byte = u8::from_str_radix(&value[start..start + 2], 16)
            .map_err(|_| "The public key must be valid hexadecimal.".to_string())?;
    }
    Ok(key)
}

/// A load failure kept off to the side: title + detail, tinted like the
/// app-level error banner (same tonal language, scoped to this screen).
fn broken_banner(ui: &mut egui::Ui, pal: &Palette, title: &str, detail: &str) {
    egui::Frame::new()
        .fill(pal.unhealthy.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0_f32, pal.unhealthy.gamma_multiply(0.42)))
        .corner_radius(style::radius(pal.corner))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                icons::draw(ui.painter(), Icon::Alert, rect, pal.unhealthy);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(title).strong().color(pal.text));
                    ui.label(RichText::new(detail).small().color(pal.text_muted));
                });
            });
        });
}

/// Broken extension folders: manifests that failed to parse or validate.
/// Listed rather than hidden, so a bad install stays visible instead of
/// silently vanishing from the list next to its healthy siblings.
fn failures_section(
    ui: &mut egui::Ui,
    pal: &Palette,
    failures: &[(std::path::PathBuf, HostError)],
) {
    ui.label(
        RichText::new("Couldn't load")
            .small()
            .strong()
            .color(pal.text_muted),
    );
    ui.add_space(6.0);
    for (path, err) in failures {
        egui::Frame::new()
            .fill(pal.unhealthy.gamma_multiply(0.10))
            .stroke(Stroke::new(1.0_f32, pal.unhealthy.gamma_multiply(0.35)))
            .corner_radius(style::radius(pal.corner))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    icons::draw(ui.painter(), Icon::Alert, rect, pal.unhealthy);
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        ui.label(RichText::new(name).small().strong().color(pal.text));
                        ui.label(RichText::new(err.to_string()).small().color(pal.text_muted));
                    });
                });
            });
        ui.add_space(6.0);
    }
}

/// One installed extension's editable card: identity + enabled toggle up
/// top, a dev-mode toggle, then a checklist of exactly the capabilities its
/// manifest requests. Every edit is persisted immediately through
/// `ExtensionRegistry::set_settings`; a save failure is written into `error`
/// rather than returned, mirroring how `Discovery::failures` is surfaced
/// rather than silently dropped. Returns `true` if any setting changed.
fn extension_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    registry: &mut ExtensionRegistry,
    ext: &mut InstalledExtension,
    error: &mut Option<String>,
) -> bool {
    let mut dirty = false;

    egui::Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .corner_radius(style::radius(pal.corner))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&ext.manifest.name).strong().color(pal.text));
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!("v{}", ext.manifest.version))
                                .small()
                                .color(pal.text_faint),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(
                            RichText::new(&ext.manifest.id)
                                .small()
                                .monospace()
                                .color(pal.text_muted),
                        );
                        ui.label(
                            RichText::new(tier_label(ext.manifest.tier))
                                .small()
                                .color(pal.text_faint),
                        );
                    });
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let salt = format!("ext-{}-enabled", ext.manifest.id);
                    if let Some(next) = toggle(ui, pal, &salt, ext.settings.enabled) {
                        ext.settings.enabled = next;
                        dirty = true;
                    }
                    ui.add_space(4.0);
                    if icons::icon_button(ui, pal, Icon::Folder, None, "Open extension folder")
                        .clicked()
                    {
                        open_in_file_manager(&ext.directory);
                    }
                });
            });

            if ext.manifest.tier == Tier::Component {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Component runtime isn't wired into the supervisor yet — this \
                         extension won't launch until it is.",
                    )
                    .small()
                    .color(pal.text_faint),
                );
            }

            if ext.manifest.tier == Tier::Theme {
                // A theme has no runtime to put in dev mode and no
                // capabilities to grant — it's just data, picked in Settings.
                ui.add_space(10.0);
                ui.label(
                    RichText::new("Variants")
                        .small()
                        .strong()
                        .color(pal.text_muted),
                );
                ui.add_space(4.0);
                let names = ext
                    .manifest
                    .theme_variants
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                ui.label(RichText::new(names).small().color(pal.text_faint));
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Pick this theme and a variant from Settings → Theme.")
                        .small()
                        .color(pal.text_faint),
                );
            } else {
                ui.add_space(8.0);
                let dev_salt = format!("ext-{}-dev", ext.manifest.id);
                if let Some(next) =
                    labeled_toggle(ui, pal, "Dev mode", &dev_salt, ext.settings.dev_mode)
                {
                    ext.settings.dev_mode = next;
                    dirty = true;
                }

                ui.add_space(10.0);
                ui.label(
                    RichText::new("Capabilities")
                        .small()
                        .strong()
                        .color(pal.text_muted),
                );
                ui.add_space(4.0);
                if ext.manifest.capabilities.is_empty() {
                    ui.label(
                        RichText::new("This extension requests no capabilities.")
                            .small()
                            .color(pal.text_faint),
                    );
                } else {
                    for cap in ext.manifest.capabilities.clone() {
                        let granted = ext.settings.granted_capabilities.contains(&cap);
                        if let Some(next) =
                            check(ui, pal, &ext.manifest.id, capability_label(cap), granted)
                        {
                            if next {
                                if !granted {
                                    ext.settings.granted_capabilities.push(cap);
                                }
                            } else {
                                ext.settings.granted_capabilities.retain(|c| *c != cap);
                            }
                            dirty = true;
                        }
                    }
                }
            }
        });

    if dirty {
        if let Err(err) = registry.set_settings(ext.manifest.id.clone(), ext.settings.clone()) {
            *error = Some(err.to_string());
        }
    }
    dirty
}

/// Ask the OS to open `path` in its file manager. Best-effort: a folder that
/// won't open is a minor inconvenience, not something that should interrupt
/// the rest of the screen, so a failure is only logged.
fn open_in_file_manager(path: &std::path::Path) {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(path).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("explorer").arg(path).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(path).spawn()
    };
    if let Err(err) = result {
        tracing::warn!(%err, path = %path.display(), "failed to open extension folder");
    }
}

fn tier_label(tier: Tier) -> &'static str {
    match tier {
        Tier::Script => "script",
        Tier::Component => "component",
        Tier::Theme => "theme",
    }
}

/// A short, human description of what granting `cap` actually lets an
/// extension do — shown beside its checkbox instead of the bare enum name.
fn capability_label(cap: Capability) -> &'static str {
    match cap {
        Capability::ContainersRead => "Read containers",
        Capability::ContainersLifecycle => "Start, stop, and restart containers",
        Capability::ContainersExec => "Run commands inside containers",
        Capability::LogsRead => "Read container logs",
        Capability::StatsRead => "Read resource stats",
        Capability::ImagesRead => "Read images",
        Capability::RegistriesRead => "Read registries",
        Capability::Network => "Make network requests",
        Capability::Storage => "Store its own local data",
        Capability::Notifications => "Show notifications",
    }
}

/// An Off/On segmented control for a boolean row, salted per-card (mirrors
/// `settings::toggle`). Returns the new value only when it actually flips.
fn toggle(ui: &mut egui::Ui, pal: &Palette, id_salt: &str, value: bool) -> Option<bool> {
    segmented(ui, pal, id_salt, &["Off", "On"], value as usize).map(|i| i == 1)
}

/// A quiet muted label to the left of a right-aligned [`toggle`] — used for
/// the card's "Dev mode" row, which (unlike "Enabled") has no trailing
/// identity text to sit beside on the same line.
fn labeled_toggle(
    ui: &mut egui::Ui,
    pal: &Palette,
    label: &str,
    id_salt: &str,
    value: bool,
) -> Option<bool> {
    let mut next = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).small().color(pal.text_muted));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(v) = toggle(ui, pal, id_salt, value) {
                next = Some(v);
            }
        });
    });
    next
}

/// A drawn checkbox row for one capability grant — same tonal language as
/// `groups::check`, duplicated locally rather than shared because the two
/// screens' rows differ (a container name there, a capability sentence
/// here). `id_salt` scopes the persistent id to one extension's card, so two
/// cards requesting the same capability never collide. Returns the new value
/// only when it flips.
fn check(ui: &mut egui::Ui, pal: &Palette, id_salt: &str, label: &str, on: bool) -> Option<bool> {
    let resp = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (b, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
            let t = ui
                .ctx()
                .animate_bool(ui.make_persistent_id((id_salt, "chk", label)), on);
            ui.painter().rect(
                b.shrink(1.0),
                style::radius((pal.corner - 3.0).max(1.0)),
                pal.surface.lerp_to_gamma(pal.accent, 0.9 * t),
                Stroke::new(1.0_f32, pal.border_strong.lerp_to_gamma(pal.accent, t)),
                egui::StrokeKind::Inside,
            );
            if t > 0.0 {
                let c = b.center();
                ui.painter().add(egui::Shape::line(
                    vec![
                        egui::pos2(c.x - 3.0, c.y),
                        egui::pos2(c.x - 0.8, c.y + 2.4),
                        egui::pos2(c.x + 3.4, c.y - 2.8),
                    ],
                    Stroke::new(1.6_f32 * t, pal.on_accent),
                ));
            }
            ui.label(RichText::new(label).color(pal.text));
        })
        .response;

    let row = ui.interact(
        resp.rect,
        ui.make_persistent_id((id_salt, "chk-hit", label)),
        Sense::click(),
    );
    if row.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    row.clicked().then_some(!on)
}

// ---- Declarative panel renderer ------------------------------------------

/// Draw one extension-declared [`UiNode`] tree with real `egui` widgets and
/// turn the interaction it received this frame (if any) into a [`UiEvent`].
/// Pure and side-effect-free beyond the `Ui` it draws into — no I/O, no
/// `ExtensionSupervisor` wiring. When a tree nests more than one event source
/// in one frame (e.g. two buttons in a `Row`), the first one encountered
/// wins; the rest still draw normally.
pub fn render_ui_node(ui: &mut egui::Ui, pal: &Palette, node: &UiNode) -> Option<UiEvent> {
    render_ui_node_at(ui, pal, node, 0)
}

fn render_ui_node_at(
    ui: &mut egui::Ui,
    pal: &Palette,
    node: &UiNode,
    depth: u8,
) -> Option<UiEvent> {
    if depth >= MAX_NODE_DEPTH {
        ui.label(RichText::new("\u{2026}").small().color(pal.text_faint));
        return None;
    }

    match node {
        UiNode::Label { text } => {
            ui.label(RichText::new(text).color(pal.text));
            None
        }
        UiNode::Button { id, text } => {
            if icons::text_button(ui, pal, text).clicked() {
                Some(UiEvent::Clicked { id: id.clone() })
            } else {
                None
            }
        }
        UiNode::TextInput { id, value } => {
            let mut text = value.clone();
            let resp = ui.add(egui::TextEdit::singleline(&mut text).desired_width(160.0));
            if resp.changed() {
                Some(UiEvent::Changed {
                    id: id.clone(),
                    value: text,
                })
            } else {
                None
            }
        }
        UiNode::Row { children } => {
            let mut event = None;
            ui.horizontal(|ui| {
                for child in children {
                    let child_event = render_ui_node_at(ui, pal, child, depth + 1);
                    event = event.take().or(child_event);
                }
            });
            event
        }
        UiNode::Column { children } => {
            let mut event = None;
            ui.vertical(|ui| {
                for child in children {
                    let child_event = render_ui_node_at(ui, pal, child, depth + 1);
                    event = event.take().or(child_event);
                }
            });
            event
        }
        UiNode::Table { headers, rows } => {
            render_table(ui, pal, headers, rows);
            None
        }
    }
}

/// A minimal table: a muted header row, then data rows in fixed-width
/// columns sized from `headers.len()`. A row with too few or too many cells
/// never panics — a short row just leaves its trailing cells blank, a long
/// one drops its extras, since extension-supplied data can't be trusted to
/// line up with its own header count.
fn render_table(ui: &mut egui::Ui, pal: &Palette, headers: &[String], rows: &[Vec<String>]) {
    if headers.is_empty() {
        return;
    }
    let col_w = (ui.available_width() / headers.len() as f32).max(48.0);

    ui.horizontal(|ui| {
        for header in headers {
            ui.add_sized(
                vec2(col_w, 18.0),
                egui::Label::new(RichText::new(header).small().strong().color(pal.text_muted)),
            );
        }
    });
    ui.add_space(4.0);

    for row in rows {
        ui.horizontal(|ui| {
            let mut cells = row.iter().map(String::as_str);
            for _ in headers {
                let cell = cells.next().unwrap_or("");
                ui.add_sized(
                    vec2(col_w, 18.0),
                    egui::Label::new(RichText::new(cell).color(pal.text)),
                );
            }
        });
        ui.add_space(2.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocker_ext_api::Manifest;
    use rocker_ext_host::ExtensionSettings;

    fn manifest(id: &str, capabilities: Vec<Capability>) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: "Example".into(),
            version: "0.1.0".into(),
            tier: Tier::Script,
            capabilities,
            entry: Some("main.rhai".into()),
            schedule_seconds: None,
            theme_variants: Vec::new(),
        }
    }

    fn screen_with(
        extensions: Vec<InstalledExtension>,
        failures: Vec<(std::path::PathBuf, HostError)>,
    ) -> ExtensionsScreen {
        let root = std::env::temp_dir().join(format!(
            "rocker-ext-ui-test-{}-{}",
            std::process::id(),
            extensions.len()
        ));
        let registry =
            ExtensionRegistry::load(root.clone()).expect("registry loads over a fresh root");
        ExtensionsScreen {
            registry: Ok(registry),
            discovery: Discovery {
                extensions,
                failures,
            },
            registry_draft: RegistryDraft::default(),
            tab: ExtensionsTab::Installed,
        }
    }

    /// Lay the whole screen out headlessly at a few widths: catches
    /// layout-math panics and confirms a no-input frame reports no edit
    /// (pointer-driven checks happen in the running app, same convention as
    /// `settings::tests` and `groups::tests`).
    #[test]
    fn screen_lays_out_without_panic() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());

        let mut screen = screen_with(
            vec![
                InstalledExtension {
                    manifest: manifest(
                        "example.notifier",
                        vec![Capability::ContainersRead, Capability::Notifications],
                    ),
                    directory: std::path::PathBuf::from("/nonexistent/example.notifier"),
                    settings: ExtensionSettings {
                        enabled: true,
                        dev_mode: false,
                        granted_capabilities: vec![Capability::ContainersRead],
                    },
                },
                InstalledExtension {
                    manifest: manifest("example.empty", vec![]),
                    directory: std::path::PathBuf::from("/nonexistent/example.empty"),
                    settings: ExtensionSettings::default(),
                },
            ],
            vec![(
                std::path::PathBuf::from("/nonexistent/broken-one"),
                HostError::Runtime("bad manifest".into()),
            )],
        );

        for width in [420.0_f32, 700.0, 1200.0] {
            let mut registries = vec![ExtensionRegistrySource::official()];
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(width, 640.0),
                )),
                ..Default::default()
            };
            let mut edit = None;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    edit = extensions_screen(ui, &pal, &mut screen, &mut registries);
                });
            });
            assert!(edit.is_none(), "no pointer input, so nothing should change");
        }
    }

    /// Same no-panic guarantee with no extensions and no failures — the
    /// empty state, and a registry that failed to load in the first place.
    #[test]
    fn empty_and_broken_registry_lay_out_without_panic() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());

        let mut empty = screen_with(vec![], vec![]);
        let mut broken = ExtensionsScreen {
            registry: Err("permission denied".into()),
            discovery: Discovery::default(),
            registry_draft: RegistryDraft::default(),
            tab: ExtensionsTab::Installed,
        };

        for screen in [&mut empty, &mut broken] {
            let mut registries = vec![ExtensionRegistrySource::official()];
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(600.0, 400.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    extensions_screen(ui, &pal, screen, &mut registries);
                });
            });
        }
    }

    #[test]
    fn capability_label_covers_every_variant() {
        for cap in [
            Capability::ContainersRead,
            Capability::ContainersLifecycle,
            Capability::ContainersExec,
            Capability::LogsRead,
            Capability::StatsRead,
            Capability::ImagesRead,
            Capability::RegistriesRead,
            Capability::Network,
            Capability::Storage,
            Capability::Notifications,
        ] {
            assert!(!capability_label(cap).is_empty());
        }
    }

    #[test]
    fn registry_draft_accepts_a_signed_https_source() {
        let draft = RegistryDraft {
            id: "community".into(),
            index_url: "https://registry.example/index-v1.json".into(),
            signature_url: "https://registry.example/index-v1.sig".into(),
            public_key: "21bc5889a2e5293ee6a22da5678f0497e90b2c67a2c55fd79f1ca0434af21e0a".into(),
        };

        let source = registry_from_draft(&draft).expect("valid registry draft");

        assert_eq!(source.id, "community");
    }

    #[test]
    fn registry_draft_rejects_a_non_hex_public_key() {
        let error = decode_public_key("z".repeat(64).as_str()).expect_err("invalid key");

        assert_eq!(error, "The public key must be valid hexadecimal.");
    }

    #[test]
    fn github_repository_turns_a_raw_index_url_into_a_repository_link() {
        let repository = github_repository(
            "https://raw.githubusercontent.com/makis-san/rocker-registry/main/index-v1.json",
        );

        assert_eq!(
            repository,
            Some((
                "makis-san/rocker-registry".into(),
                "https://github.com/makis-san/rocker-registry".into(),
            ))
        );
    }

    /// Every `UiNode` variant, including nested `Row`/`Column`/`Table` and a
    /// ragged table (fewer and more cells than headers), draws without
    /// panicking and — with no pointer input — resolves to no event.
    #[test]
    fn render_ui_node_lays_out_every_variant_without_panic() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());

        let tree = UiNode::Column {
            children: vec![
                UiNode::Label {
                    text: "Status".into(),
                },
                UiNode::Row {
                    children: vec![
                        UiNode::Button {
                            id: "start".into(),
                            text: "Start".into(),
                        },
                        UiNode::Button {
                            id: "stop".into(),
                            text: "Stop".into(),
                        },
                        UiNode::TextInput {
                            id: "note".into(),
                            value: "hi".into(),
                        },
                    ],
                },
                UiNode::Table {
                    headers: vec!["Name".into(), "State".into()],
                    rows: vec![
                        vec!["web".into(), "running".into()],
                        vec!["db".into()],
                        vec!["cache".into(), "stopped".into(), "extra".into()],
                    ],
                },
            ],
        };

        for width in [240.0_f32, 480.0, 900.0] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(width, 400.0),
                )),
                ..Default::default()
            };
            let mut event = None;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    event = render_ui_node(ui, &pal, &tree);
                });
            });
            assert!(event.is_none(), "no pointer input, so no event should fire");
        }
    }

    #[test]
    fn render_ui_node_handles_an_empty_table_without_panic() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());
        let node = UiNode::Table {
            headers: vec![],
            rows: vec![],
        };
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(400.0, 200.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    render_ui_node(ui, &pal, &node);
                });
            },
        );
    }

    /// A tree nested far past `MAX_NODE_DEPTH` renders as a placeholder past
    /// the cap instead of recursing indefinitely.
    #[test]
    fn render_ui_node_caps_pathological_nesting_without_panic() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());

        let mut node = UiNode::Label {
            text: "bottom".into(),
        };
        for _ in 0..(MAX_NODE_DEPTH as usize + 40) {
            node = UiNode::Column {
                children: vec![node],
            };
        }

        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(400.0, 600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    render_ui_node(ui, &pal, &node);
                });
            },
        );
    }

    /// The bundled reference Catppuccin theme extension end to end: its
    /// manifest discovers as an enabled `Tier::Theme` extension, shows up in
    /// [`theme_extension_options`], and its Mocha variant parses into a real
    /// [`Theme`] through [`resolve_custom_theme`] — the same path Settings
    /// and the app's theme resolution use, exercised here against the actual
    /// file on disk rather than a hand-built fixture, so a schema drift
    /// between `rocker_theme::Theme` and this shipped example would fail a
    /// test instead of only surfacing as a broken theme at runtime.
    #[test]
    fn reference_catppuccin_theme_resolves_through_the_real_pipeline() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../extensions/examples/catppuccin");
        let manifest =
            rocker_ext_host::extension_manifest(&directory).expect("theme manifest is valid");
        assert_eq!(manifest.tier, Tier::Theme);

        let discovery = Discovery {
            extensions: vec![InstalledExtension {
                manifest,
                directory,
                settings: ExtensionSettings {
                    enabled: true,
                    ..ExtensionSettings::default()
                },
            }],
            failures: Vec::new(),
        };

        let options = theme_extension_options(&discovery);
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].id, "catppuccin.theme");
        assert_eq!(
            options[0].variants,
            vec![("mocha".to_string(), "Mocha".to_string())]
        );

        let theme = resolve_custom_theme(&discovery, "catppuccin.theme", "mocha")
            .expect("the Mocha variant parses into a real Theme");
        assert_eq!(theme.id, "catppuccin-mocha");
        assert_eq!(theme.mode, rocker_theme::Mode::Dark);
        assert_eq!(theme.tokens.accent.0, "#cba6f7");
        assert_eq!(theme.tokens.terminal_palette.len(), 16);

        // A disabled extension (the install default) must not surface as a
        // pickable custom theme, mirroring how a disabled extension's code
        // never runs.
        let mut disabled = discovery;
        disabled.extensions[0].settings.enabled = false;
        assert!(theme_extension_options(&disabled).is_empty());
        assert!(resolve_custom_theme(&disabled, "catppuccin.theme", "mocha").is_none());
    }
}
