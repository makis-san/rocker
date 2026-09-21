//! Settings navigation and controls.

use rocker_core::{Connection, ConnectionId, ConnectionKind, KubernetesPod};
use rocker_ext_api::Tier;
use rocker_ext_host::Discovery;
use rocker_store::{KubernetesKubeconfig, Settings};

use crate::extensions::ThemeExtensionOption;
use crate::style::{self, Palette};
use crate::widgets::{dropdown, segmented, stepper};

/// Read-only facts shown in the About section.
pub struct About<'a> {
    pub app_version: &'a str,
    pub engine: Option<&'a str>,
    pub engine_status: &'a str,
    pub config_path: &'a str,
}

/// State consumed by the Settings screen for one frame.
pub struct SettingsScreenData<'a> {
    pub settings: &'a mut Settings,
    pub connections: &'a mut Vec<Connection>,
    pub kubernetes_kubeconfigs: &'a mut Vec<KubernetesKubeconfig>,
    pub kubernetes_engines: &'a [KubernetesEngine],
    pub kubernetes_pods: &'a [KubernetesPod],
    pub kubernetes_loading: bool,
    pub kubernetes_error: Option<&'a str>,
    pub custom_themes: &'a [ThemeExtensionOption],
    pub discovery: &'a Discovery,
    pub about: About<'a>,
}
/// An engine connection selected from the Docker pane.
pub enum DockerAction {
    Connect(Connection),
}
/// A read-only Kubernetes query selected from the settings pane.
pub enum KubernetesAction {
    RefreshPods { kubeconfig: String, context: String },
}
/// Changes made while drawing the settings screen.
pub struct Edit {
    pub theme_changed: bool,
    pub autostart_changed: bool,
    pub docker_action: Option<DockerAction>,
    pub kubernetes_action: Option<KubernetesAction>,
}

const CONTENT_W: f32 = 552.0;
const CONTROL_W: f32 = 232.0;
const ROW_H: f32 = 44.0;
const THEME_OPTS: [&str; 4] = ["System", "Light", "Dark", "Custom"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Appearance,
    History,
    System,
    Docker,
    Kubernetes,
    Plugins,
    About,
}
impl Page {
    const CORE: [Self; 6] = [
        Self::Appearance,
        Self::History,
        Self::System,
        Self::Docker,
        Self::Kubernetes,
        Self::About,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::History => "Usage history",
            Self::System => "System",
            Self::Docker => "Docker engines",
            Self::Kubernetes => "Kubernetes engines",
            Self::Plugins => "Plugins",
            Self::About => "About",
        }
    }
}
#[derive(Clone, Default)]
struct EngineDraft {
    name: String,
    endpoint: String,
}

#[derive(Clone, Default)]
struct KubeconfigDraft {
    name: String,
    path: String,
}

struct KubernetesPageData<'a> {
    enabled: &'a mut bool,
    kubeconfigs: &'a mut Vec<KubernetesKubeconfig>,
    engines: &'a [KubernetesEngine],
    pods: &'a [KubernetesPod],
    loading: bool,
    error: Option<&'a str>,
    changed: &'a mut bool,
    action: &'a mut Option<KubernetesAction>,
}

/// A Kubernetes context found in a kubeconfig on the current device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KubernetesEngine {
    pub name: String,
    pub cluster: String,
    pub user: String,
    pub current: bool,
    pub source: String,
    kubeconfig: String,
}

/// Select the current context (or first available context) for a Pod refresh.
pub fn kubernetes_pod_query(engines: &[KubernetesEngine]) -> Option<(String, String)> {
    engines
        .iter()
        .find(|engine| engine.current)
        .or_else(|| engines.first())
        .map(|engine| (engine.kubeconfig.clone(), engine.name.clone()))
}

/// Queries for every distinct context discovered on this device.
pub fn kubernetes_pod_queries(engines: &[KubernetesEngine]) -> Vec<(String, String)> {
    let mut contexts = std::collections::HashSet::new();
    engines
        .iter()
        .filter(|engine| contexts.insert(engine.name.as_str()))
        .map(|engine| (engine.kubeconfig.clone(), engine.name.clone()))
        .collect()
}

/// Find the local kubeconfig file for one discovered Kubernetes context.
pub fn kubernetes_kubeconfig_for_context(
    engines: &[KubernetesEngine],
    context: &str,
) -> Option<String> {
    engines
        .iter()
        .find(|engine| engine.name == context)
        .map(|engine| engine.kubeconfig.clone())
}
fn theme_index(id: &str) -> usize {
    match id {
        "light" => 1,
        "dark" => 2,
        "custom" => 3,
        _ => 0,
    }
}
fn theme_id(index: usize) -> &'static str {
    match index {
        1 => "light",
        2 => "dark",
        3 => "custom",
        _ => "system",
    }
}

/// Draw the persistent settings sidebar and selected settings page.
pub fn settings_screen(
    ui: &mut egui::Ui,
    pal: &Palette,
    data: SettingsScreenData<'_>,
) -> Option<Edit> {
    let SettingsScreenData {
        settings,
        connections,
        kubernetes_kubeconfigs,
        kubernetes_engines,
        kubernetes_pods,
        kubernetes_loading,
        kubernetes_error,
        custom_themes,
        discovery,
        about,
    } = data;
    let id = ui.make_persistent_id("settings-page");
    let mut page = ui.data_mut(|data| data.get_temp::<Page>(id).unwrap_or(Page::Appearance));
    let plugin_menu_id = ui.make_persistent_id("settings-plugins-expanded");
    let mut plugins_expanded =
        ui.data_mut(|data| data.get_temp::<bool>(plugin_menu_id).unwrap_or(false));
    let selected_plugin_id = ui.make_persistent_id("settings-selected-plugin");
    let mut selected_plugin = ui.data_mut(|data| data.get_temp::<String>(selected_plugin_id));
    let has_plugin_settings = plugin_settings_extensions(discovery).next().is_some();
    if !has_plugin_settings && page == Page::Plugins {
        page = Page::Appearance;
        selected_plugin = None;
    }
    let mut theme_changed = false;
    let mut autostart_changed = false;
    let mut changed = false;
    let mut docker_action = None;
    let mut kubernetes_action = None;
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(164.0, ui.available_height()),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.add_space(7.0);
                ui.label(
                    egui::RichText::new("SETTINGS")
                        .small()
                        .strong()
                        .color(pal.text_muted),
                );
                ui.add_space(8.0);
                for candidate in Page::CORE {
                    let selected = page == candidate;
                    if ui
                        .selectable_label(
                            selected,
                            egui::RichText::new(candidate.label()).color(if selected {
                                pal.text
                            } else {
                                pal.text_muted
                            }),
                        )
                        .clicked()
                    {
                        page = candidate;
                    }
                }
                if has_plugin_settings {
                    let plugins_selected = page == Page::Plugins;
                    if ui
                        .selectable_label(
                            plugins_selected,
                            egui::RichText::new("Plugins").color(if plugins_selected {
                                pal.text
                            } else {
                                pal.text_muted
                            }),
                        )
                        .clicked()
                    {
                        plugins_expanded = !plugins_expanded;
                    }
                    if plugins_expanded {
                        ui.indent("plugin-settings-submenu", |ui| {
                            for extension in plugin_settings_extensions(discovery) {
                                let selected = selected_plugin.as_deref()
                                    == Some(extension.manifest.id.as_str());
                                if ui
                                    .selectable_label(
                                        selected,
                                        egui::RichText::new(&extension.manifest.name)
                                            .small()
                                            .color(if selected {
                                                pal.text
                                            } else {
                                                pal.text_muted
                                            }),
                                    )
                                    .clicked()
                                {
                                    page = Page::Plugins;
                                    selected_plugin = Some(extension.manifest.id.clone());
                                }
                            }
                        });
                    }
                }
            },
        );
        ui.separator();
        ui.add_space(style::MD);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Scroll areas inherit the parent horizontal layout. Make the
                // pane explicit so headings and setting rows always stack.
                ui.vertical(|ui| {
                    ui.set_width(CONTENT_W.min(ui.available_width()));
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(page.label())
                            .size(19.0)
                            .strong()
                            .color(pal.text),
                    );
                    match page {
                        Page::Appearance => {
                            row(ui, pal, "Theme", "Match your OS, or pick one.", |ui| {
                                if let Some(next) = segmented(
                                    ui,
                                    pal,
                                    "theme",
                                    &THEME_OPTS,
                                    theme_index(&settings.theme),
                                ) {
                                    settings.theme = theme_id(next).to_string();
                                    theme_changed = true;
                                }
                            });
                            if settings.theme == "custom" {
                                theme_changed |=
                                    custom_theme_rows(ui, pal, settings, custom_themes);
                            }
                        }
                        Page::History => {
                            row(
                                ui,
                                pal,
                                "Retention",
                                "How long usage history is kept.",
                                |ui| {
                                    if let Some(next) = stepper(
                                        ui,
                                        pal,
                                        "retention",
                                        settings.stats_retention_hours as i64,
                                        12..=336,
                                        12,
                                        "h",
                                    ) {
                                        settings.stats_retention_hours = next as u32;
                                        changed = true;
                                    }
                                },
                            );
                            row(
                                ui,
                                pal,
                                "Live graphs",
                                "Cap on containers streaming stats at once.",
                                |ui| {
                                    if let Some(next) = stepper(
                                        ui,
                                        pal,
                                        "streams",
                                        settings.max_stats_streams as i64,
                                        4..=64,
                                        4,
                                        "",
                                    ) {
                                        settings.max_stats_streams = next as usize;
                                        changed = true;
                                    }
                                },
                            );
                        }
                        Page::System => {
                            row(
                                ui,
                                pal,
                                "Minimize to tray",
                                "Closing or minimizing hides Rocker to the tray.",
                                |ui| {
                                    if let Some(next) =
                                        toggle(ui, pal, "min-to-tray", settings.minimize_to_tray)
                                    {
                                        settings.minimize_to_tray = next;
                                        changed = true;
                                    }
                                },
                            );
                            row(
                                ui,
                                pal,
                                "Start hidden",
                                "Launch straight to the tray, no window.",
                                |ui| {
                                    if let Some(next) =
                                        toggle(ui, pal, "start-hidden", settings.start_minimized)
                                    {
                                        settings.start_minimized = next;
                                        changed = true;
                                    }
                                },
                            );
                            row(
                                ui,
                                pal,
                                "Open at login",
                                "Start Rocker automatically when you sign in.",
                                |ui| {
                                    if let Some(next) =
                                        toggle(ui, pal, "open-at-login", settings.open_at_login)
                                    {
                                        settings.open_at_login = next;
                                        autostart_changed = true;
                                    }
                                },
                            );
                        }
                        Page::Docker => docker_page(
                            ui,
                            pal,
                            connections,
                            about.engine_status,
                            &mut changed,
                            &mut docker_action,
                        ),
                        Page::Kubernetes => kubernetes_page(
                            ui,
                            pal,
                            KubernetesPageData {
                                enabled: &mut settings.kubernetes_enabled,
                                kubeconfigs: kubernetes_kubeconfigs,
                                engines: kubernetes_engines,
                                pods: kubernetes_pods,
                                loading: kubernetes_loading,
                                error: kubernetes_error,
                                changed: &mut changed,
                                action: &mut kubernetes_action,
                            },
                        ),
                        Page::Plugins => {
                            plugins_page(ui, pal, discovery, selected_plugin.as_deref())
                        }
                        Page::About => {
                            row(ui, pal, "Version", "The build you're running.", |ui| {
                                value(ui, pal, about.app_version)
                            });
                            row(
                                ui,
                                pal,
                                "Docker Engine",
                                "Version reported by the daemon.",
                                |ui| value(ui, pal, about.engine.unwrap_or("not connected")),
                            );
                            section(ui, pal, "Config file");
                            ui.label(
                                egui::RichText::new(about.config_path)
                                    .monospace()
                                    .size(11.5)
                                    .color(pal.text_muted),
                            );
                        }
                    };
                    ui.add_space(28.0);
                });
            });
    });
    ui.data_mut(|data| data.insert_temp(id, page));
    ui.data_mut(|data| data.insert_temp(plugin_menu_id, plugins_expanded));
    if let Some(selected_plugin) = selected_plugin {
        ui.data_mut(|data| data.insert_temp(selected_plugin_id, selected_plugin));
    }
    (theme_changed
        || autostart_changed
        || changed
        || docker_action.is_some()
        || kubernetes_action.is_some())
    .then_some(Edit {
        theme_changed,
        autostart_changed,
        docker_action,
        kubernetes_action,
    })
}

fn docker_page(
    ui: &mut egui::Ui,
    pal: &Palette,
    connections: &mut Vec<Connection>,
    engine_status: &str,
    changed: &mut bool,
    action: &mut Option<DockerAction>,
) {
    section(ui, pal, "Configured engines");
    let mut selected = None;
    for (index, connection) in connections.iter().enumerate() {
        let endpoint = endpoint(&connection.kind);
        let active = connection.default;
        row(ui, pal, &connection.name, &endpoint, |ui| {
            if active {
                ui.horizontal(|ui| {
                    value(ui, pal, engine_status);
                    if ui.button("Reconnect").clicked() {
                        selected = Some(index);
                    }
                });
            } else if ui.button("Switch").clicked() {
                selected = Some(index);
            }
        });
    }
    if let Some(index) = selected {
        for connection in connections.iter_mut() {
            connection.default = false;
        }
        let connection = &mut connections[index];
        connection.default = true;
        *action = Some(DockerAction::Connect(connection.clone()));
        *changed = true;
    }
    section(ui, pal, "Add engine");
    let draft_id = ui.make_persistent_id("new-engine-draft");
    let mut draft = ui.data_mut(|data| data.get_temp::<EngineDraft>(draft_id).unwrap_or_default());
    ui.label(egui::RichText::new("Add a local socket or SSH host. TLS TCP connections remain supported from existing config files.").small().color(pal.text_muted));
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut draft.name);
    });
    ui.horizontal(|ui| {
        ui.label("Endpoint");
        ui.text_edit_singleline(&mut draft.endpoint);
    });
    if ui.button("Add engine").clicked()
        && !draft.name.trim().is_empty()
        && !draft.endpoint.trim().is_empty()
    {
        let number = connections.len() + 1;
        connections.push(Connection {
            id: ConnectionId::new(format!("engine-{number}")),
            name: draft.name.trim().to_owned(),
            kind: draft_kind(draft.endpoint.trim()),
            default: false,
        });
        *changed = true;
        draft = EngineDraft::default();
    }
    ui.data_mut(|data| data.insert_temp(draft_id, draft));
    ui.add_space(12.0);
    ui.label(egui::RichText::new("Docker's API cannot safely start or stop the daemon that serves it. Use the host's service manager for daemon controls; this pane reports connection status and switches engines.").small().color(pal.text_faint));
}

fn kubernetes_page(ui: &mut egui::Ui, pal: &Palette, data: KubernetesPageData<'_>) {
    let KubernetesPageData {
        enabled,
        kubeconfigs,
        engines,
        pods,
        loading,
        error,
        changed,
        action,
    } = data;
    row(
        ui,
        pal,
        "Enable Kubernetes",
        "Query every discovered context for pods and usage. This can reach production clusters and may trigger exec-credential prompts.",
        |ui| {
            if let Some(next) = toggle(ui, pal, "kubernetes-enabled", *enabled) {
                *enabled = next;
                *changed = true;
                if next {
                    if let Some((kubeconfig, context)) = kubernetes_pod_query(engines) {
                        *action = Some(KubernetesAction::RefreshPods { kubeconfig, context });
                    }
                }
            }
        },
    );
    if !*enabled {
        return;
    }
    section(ui, pal, "Engines on this device");
    if engines.is_empty() {
        ui.label(
            egui::RichText::new(
                "No Kubernetes contexts were found. Rocker checks ~/.kube/config, KUBECONFIG, and the files below.",
            )
            .small()
            .color(pal.text_muted),
        );
    }
    for engine in engines {
        let details = if engine.user.is_empty() {
            format!("{} · {}", engine.cluster, engine.source)
        } else {
            format!("{} · {} · {}", engine.cluster, engine.user, engine.source)
        };
        row(ui, pal, &engine.name, &details, |ui| {
            if engine.current {
                value(ui, pal, "Current context");
            }
        });
    }

    section(ui, pal, "Pods");
    if loading {
        value(ui, pal, "Loading Pods…");
    } else if let Some(error) = error {
        ui.label(egui::RichText::new(error).small().color(pal.unhealthy));
    } else if pods.is_empty() {
        ui.label(
            egui::RichText::new("No Pods in this context.")
                .small()
                .color(pal.text_muted),
        );
    } else {
        for pod in pods {
            row(ui, pal, &pod.name, &pod.namespace, |ui| {
                value(ui, pal, &format!("{} · {}", pod.ready, pod.phase));
            });
        }
    }
    if let Some((kubeconfig, context)) = kubernetes_pod_query(engines) {
        if ui.button("Refresh Pods").clicked() {
            *action = Some(KubernetesAction::RefreshPods {
                kubeconfig,
                context,
            });
        }
    }

    section(ui, pal, "Additional kubeconfigs");
    let mut remove = None;
    for (index, kubeconfig) in kubeconfigs.iter().enumerate() {
        row(ui, pal, &kubeconfig.name, &kubeconfig.path, |ui| {
            if ui.button("Remove").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        kubeconfigs.remove(index);
        *changed = true;
    }

    let draft_id = ui.make_persistent_id("new-kubeconfig-draft");
    let mut draft = ui.data_mut(|data| {
        data.get_temp::<KubeconfigDraft>(draft_id)
            .unwrap_or_default()
    });
    ui.label(
        egui::RichText::new(
            "Add another kubeconfig file from this device. Its credentials stay local.",
        )
        .small()
        .color(pal.text_muted),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut draft.name);
    });
    ui.horizontal(|ui| {
        ui.label("File path");
        ui.text_edit_singleline(&mut draft.path);
    });
    if ui.button("Add kubeconfig").clicked() && !draft.path.trim().is_empty() {
        let path = draft.path.trim().to_owned();
        let name = if draft.name.trim().is_empty() {
            std::path::Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Kubeconfig")
                .to_owned()
        } else {
            draft.name.trim().to_owned()
        };
        if !kubeconfigs.iter().any(|source| source.path == path) {
            kubeconfigs.push(KubernetesKubeconfig { name, path });
            *changed = true;
        }
        draft = KubeconfigDraft::default();
    }
    ui.data_mut(|data| data.insert_temp(draft_id, draft));
}

/// Discover contexts from kubeconfig files available on this device.
///
/// The parser intentionally reads only context metadata: it does not load a
/// certificate, token, or exec credential from the file.
pub fn discover_kubernetes_engines(
    additional_kubeconfigs: &[KubernetesKubeconfig],
) -> Vec<KubernetesEngine> {
    let mut sources = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        sources.push((
            "Default kubeconfig".to_owned(),
            std::path::PathBuf::from(home).join(".kube/config"),
        ));
    }
    if let Some(paths) = std::env::var_os("KUBECONFIG") {
        sources.extend(std::env::split_paths(&paths).map(|path| ("KUBECONFIG".to_owned(), path)));
    }
    sources.extend(
        additional_kubeconfigs
            .iter()
            .map(|source| (source.name.clone(), std::path::PathBuf::from(&source.path))),
    );

    let mut seen_paths = std::collections::HashSet::new();
    let mut engines = Vec::new();
    for (source, path) in sources {
        let path = path.to_string_lossy().into_owned();
        if !seen_paths.insert(path.clone()) {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        engines.extend(parse_kubeconfig_contexts(&contents, &source, &path));
    }
    engines.sort_by(|left, right| {
        right
            .current
            .cmp(&left.current)
            .then_with(|| left.name.cmp(&right.name))
    });
    engines
}

fn parse_kubeconfig_contexts(
    contents: &str,
    source: &str,
    kubeconfig: &str,
) -> Vec<KubernetesEngine> {
    #[derive(Default)]
    struct ContextDraft {
        name: String,
        cluster: String,
        user: String,
    }

    let current = contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("current-context:").map(yaml_value));
    let mut contexts = Vec::new();
    let mut in_contexts = false;
    let mut draft: Option<ContextDraft> = None;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed == "contexts:" {
            in_contexts = true;
            continue;
        }
        if in_contexts
            && !line.starts_with(' ')
            && !line.starts_with('\t')
            && !trimmed.starts_with('-')
        {
            break;
        }
        if !in_contexts {
            continue;
        }
        if trimmed.starts_with("- context:") || trimmed.starts_with("- name:") {
            if let Some(draft) = draft.take().filter(|draft| !draft.name.is_empty()) {
                contexts.push(draft);
            }
            draft = Some(ContextDraft {
                name: trimmed
                    .strip_prefix("- name:")
                    .map(yaml_value)
                    .unwrap_or_default(),
                ..ContextDraft::default()
            });
        } else if let Some(draft) = &mut draft {
            if let Some(value) = trimmed.strip_prefix("cluster:") {
                draft.cluster = yaml_value(value);
            } else if let Some(value) = trimmed.strip_prefix("user:") {
                draft.user = yaml_value(value);
            } else if let Some(value) = trimmed.strip_prefix("name:") {
                draft.name = yaml_value(value);
            }
        }
    }
    if let Some(draft) = draft.filter(|draft| !draft.name.is_empty()) {
        contexts.push(draft);
    }
    contexts
        .into_iter()
        .map(|context| KubernetesEngine {
            current: current.as_deref() == Some(context.name.as_str()),
            name: context.name,
            cluster: context.cluster,
            user: context.user,
            source: source.to_owned(),
            kubeconfig: kubeconfig.to_owned(),
        })
        .collect()
}

fn yaml_value(value: &str) -> String {
    value.trim().trim_matches(['\'', '"']).to_owned()
}
fn plugin_settings_extensions(
    discovery: &Discovery,
) -> impl Iterator<Item = &rocker_ext_host::InstalledExtension> {
    discovery.extensions.iter().filter(|extension| {
        extension.settings.enabled
            && matches!(extension.manifest.tier, Tier::Script | Tier::Component)
    })
}

fn plugins_page(
    ui: &mut egui::Ui,
    pal: &Palette,
    discovery: &Discovery,
    selected_plugin: Option<&str>,
) {
    let Some(extension) = plugin_settings_extensions(discovery)
        .find(|extension| Some(extension.manifest.id.as_str()) == selected_plugin)
    else {
        ui.label(egui::RichText::new("Choose a plugin from the sidebar.").color(pal.text_muted));
        return;
    };
    section(ui, pal, &extension.manifest.name);
    ui.label(
        egui::RichText::new("Plugin settings are provided by the plugin's settings panel.")
            .color(pal.text_muted),
    );
}
fn endpoint(kind: &ConnectionKind) -> String {
    match kind {
        ConnectionKind::Socket { path } | ConnectionKind::NamedPipe { path } => path.clone(),
        ConnectionKind::Tcp { host, port, .. } => format!("tcp://{host}:{port}"),
        ConnectionKind::Ssh { uri } => uri.clone(),
    }
}
fn draft_kind(endpoint: &str) -> ConnectionKind {
    if endpoint.starts_with("ssh://") {
        ConnectionKind::Ssh {
            uri: endpoint.to_owned(),
        }
    } else {
        ConnectionKind::Socket {
            path: endpoint.trim_start_matches("unix://").to_owned(),
        }
    }
}
fn custom_theme_rows(
    ui: &mut egui::Ui,
    pal: &Palette,
    settings: &mut Settings,
    custom_themes: &[ThemeExtensionOption],
) -> bool {
    if custom_themes.is_empty() {
        row(
            ui,
            pal,
            "Custom theme",
            "No enabled theme extensions installed.",
            |_| {},
        );
        return false;
    }
    let mut changed = false;
    let mut ext_idx = custom_themes
        .iter()
        .position(|t| Some(t.id.as_str()) == settings.theme_extension.as_deref())
        .unwrap_or(0);
    if settings.theme_extension.as_deref() != Some(custom_themes[ext_idx].id.as_str()) {
        settings.theme_extension = Some(custom_themes[ext_idx].id.clone());
        settings.theme_variant = None;
        changed = true;
    }
    let names: Vec<&str> = custom_themes.iter().map(|t| t.name.as_str()).collect();
    row(
        ui,
        pal,
        "Custom theme",
        "An installed theme extension.",
        |ui| {
            if let Some(next) = dropdown(ui, pal, "theme-ext", &names, ext_idx) {
                ext_idx = next;
                settings.theme_extension = Some(custom_themes[ext_idx].id.clone());
                settings.theme_variant = None;
                changed = true;
            }
        },
    );
    let ext = &custom_themes[ext_idx];
    let mut variant_idx = ext
        .variants
        .iter()
        .position(|(id, _)| Some(id.as_str()) == settings.theme_variant.as_deref())
        .unwrap_or(0);
    if ext.variants.get(variant_idx).map(|(id, _)| id.as_str()) != settings.theme_variant.as_deref()
    {
        settings.theme_variant = ext.variants.get(variant_idx).map(|(id, _)| id.clone());
        changed = true;
    }
    if ext.variants.len() > 1 {
        let names: Vec<&str> = ext.variants.iter().map(|(_, name)| name.as_str()).collect();
        row(
            ui,
            pal,
            "Variant",
            "This theme ships more than one look.",
            |ui| {
                if let Some(next) = dropdown(ui, pal, "theme-variant", &names, variant_idx) {
                    variant_idx = next;
                    settings.theme_variant = Some(ext.variants[variant_idx].0.clone());
                    changed = true;
                }
            },
        );
    }
    changed
}
fn toggle(ui: &mut egui::Ui, pal: &Palette, id: &str, value: bool) -> Option<bool> {
    segmented(ui, pal, id, &["Off", "On"], value as usize).map(|i| i == 1)
}
fn section(ui: &mut egui::Ui, pal: &Palette, label: &str) {
    ui.add_space(20.0);
    ui.label(
        egui::RichText::new(label)
            .small()
            .strong()
            .color(pal.text_muted),
    );
    ui.add_space(6.0);
}
fn row(
    ui: &mut egui::Ui,
    pal: &Palette,
    title: &str,
    desc: &str,
    control: impl FnOnce(&mut egui::Ui),
) {
    let text_w = (ui.available_width() - CONTROL_W - style::MD).max(160.0);
    ui.horizontal(|ui| {
        ui.set_min_height(ROW_H);
        ui.allocate_ui_with_layout(
            egui::vec2(text_w, ROW_H),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(egui::RichText::new(title).strong().color(pal.text));
                    ui.label(egui::RichText::new(desc).small().color(pal.text_muted));
                });
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(CONTROL_W, ROW_H),
            egui::Layout::right_to_left(egui::Align::Center),
            control,
        );
    });
}
fn value(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .monospace()
            .size(12.0)
            .color(pal.text_muted),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_sidebar_lays_out_without_an_edit() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());
        let mut settings = Settings::default();
        let mut connections = vec![Connection::local_default()];
        let mut edit = None;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 640.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    edit = settings_screen(
                        ui,
                        &pal,
                        SettingsScreenData {
                            settings: &mut settings,
                            connections: &mut connections,
                            kubernetes_kubeconfigs: &mut Vec::new(),
                            kubernetes_engines: &[],
                            kubernetes_pods: &[],
                            kubernetes_loading: false,
                            kubernetes_error: None,
                            custom_themes: &[],
                            discovery: &Discovery::default(),
                            about: About {
                                app_version: "0.0.0",
                                engine: None,
                                engine_status: "Not connected",
                                config_path: "/tmp/rocker/config.toml",
                            },
                        },
                    );
                });
            },
        );
        assert!(edit.is_none());
    }

    #[test]
    fn endpoint_parser_preserves_ssh_and_socket_targets() {
        assert!(matches!(
            draft_kind("ssh://docker@example.test"),
            ConnectionKind::Ssh { .. }
        ));
        assert!(matches!(
            draft_kind("unix:///run/docker.sock"),
            ConnectionKind::Socket { .. }
        ));
    }

    #[test]
    fn kubeconfig_parser_reads_context_metadata_without_credentials() {
        let contexts = parse_kubeconfig_contexts(
            r#"
current-context: local-dev
contexts:
  - context:
      cluster: docker-desktop
      user: docker-desktop
    name: local-dev
  - context:
      cluster: staging
      user: deployer
    name: staging
users:
  - name: deployer
    user:
      token: secret-not-read
"#,
            "Default kubeconfig",
            "/tmp/config",
        );

        assert_eq!(contexts.len(), 2);
        assert_eq!(contexts[0].name, "local-dev");
        assert!(contexts[0].current);
    }

    #[test]
    fn kubeconfig_parser_reads_rancher_desktop_context_ordering() {
        let contexts = parse_kubeconfig_contexts(
            r#"
contexts:
  - name: rancher-desktop
    context:
      cluster: rancher-desktop
      user: rancher-desktop
current-context: rancher-desktop
"#,
            "Default kubeconfig",
            "/tmp/config",
        );

        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].name, "rancher-desktop");
    }
}
