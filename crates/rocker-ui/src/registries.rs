//! The Registries view (PLAN §5.2). One centered column: an "Add registry"
//! form, an "Import from Docker config" action, then a card per configured
//! registry with its auth status and a "Test connection" button.
//!
//! Credentials never touch `Config`/TOML — only a [`rocker_core::Registry`]'s
//! `keychain_ref` does, resolved through a [`SecretStore`] this screen owns.
//! A test-connection probe is real network I/O, so it always runs on a
//! plain background thread and reports back over a channel drained each
//! frame, never inline in the UI closure (PLAN §3.2: the UI never blocks).
//!
//! `~/.docker/config.json` is also imported automatically on every startup
//! (`RockerApp::new`, via [`apply_import`]) — the button here is only a
//! manual re-scan for mid-session changes, not the only way in.
//!
//! Cloud-native registries (AWS ECR, GCR, ...) don't get their own add form
//! here either: a user sets one up the standard way outside Rocker (a
//! `credHelpers` entry in `~/.docker/config.json` pointing at, say,
//! `docker-credential-ecr-login`), the importer above notes it as a
//! `Helper`-typed entry with no secret, and its card's "Test connection"
//! resolves a fresh credential from that helper (PLAN §5.2).

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use egui::{Align, Layout, RichText, Stroke};
use rocker_core::{AuthType, Registry};
use rocker_secrets::probe::{self, ConnectionOutcome};
use rocker_secrets::{KeychainRef, SecretStore};

use crate::format;
use crate::icons::{self, Icon};
use crate::style::{self, Palette};

const COLUMN_W: f32 = 560.0;

/// One registry's last (or in-flight) test-connection result, kept apart from
/// `Registry` itself since only a success updates `verified_at_ms`.
enum TestState {
    Testing,
    Outcome(ConnectionOutcome),
}

/// Owns the secret store, the "add registry" form fields, in-flight test
/// state, and the channel background probes report back on. Lives alongside
/// `RockerApp`'s other screens (`ExtensionsScreen`, `DetailScreen`, ...).
pub struct RegistriesScreen {
    secrets: Arc<dyn SecretStore>,
    new_host: String,
    new_username: String,
    new_password: String,
    tests: HashMap<String, TestState>,
    test_tx: Sender<(String, ConnectionOutcome)>,
    test_rx: Receiver<(String, ConnectionOutcome)>,
    /// Set right after "Import from Docker config", cleared on the next edit.
    import_summary: Option<String>,
}

impl RegistriesScreen {
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        let (test_tx, test_rx) = std::sync::mpsc::channel();
        Self {
            secrets,
            new_host: String::new(),
            new_username: String::new(),
            new_password: String::new(),
            tests: HashMap::new(),
            test_tx,
            test_rx,
            import_summary: None,
        }
    }

    /// Kick off a background probe with a stored credential; the outcome
    /// lands in `tests` on a later `registries_screen` call via [`Self::drain`].
    fn spawn_test(&mut self, host: String, username: String, password: String) {
        self.spawn(host.clone(), move || {
            probe::test_connection(&host, &username, &password)
        });
    }

    /// Kick off a background probe for a `Helper`-typed registry: the
    /// credential comes from its `docker-credential-*` helper, resolved
    /// fresh in the background thread, never held by this screen.
    fn spawn_helper_test(&mut self, host: String, helper: String) {
        self.spawn(host.clone(), move || {
            probe::test_helper_connection(&host, &helper)
        });
    }

    /// Shared plumbing for both probe kinds: mark the row "Testing…", then
    /// run `probe` on a background thread (real network and/or subprocess
    /// I/O, so it never runs inline in the UI closure) and report back over
    /// the channel [`Self::drain`] reads.
    fn spawn(&mut self, host: String, probe: impl FnOnce() -> ConnectionOutcome + Send + 'static) {
        self.tests.insert(host.clone(), TestState::Testing);
        let tx = self.test_tx.clone();
        std::thread::Builder::new()
            .name("rocker-registry-probe".into())
            .spawn(move || {
                let _ = tx.send((host, probe()));
            })
            .ok(); // A failed spawn just leaves the row at "Testing…" forever
                   // rather than crashing the app; vanishingly unlikely in practice.
    }

    /// Non-blocking drain of finished probes. Returns `true` if any
    /// `registries` entry's `verified_at_ms` should be considered changed
    /// (a success), so the caller knows to persist config.
    fn drain(&mut self, registries: &mut [Registry]) -> bool {
        let mut changed = false;
        while let Ok((host, outcome)) = self.test_rx.try_recv() {
            if matches!(
                outcome,
                ConnectionOutcome::Authenticated | ConnectionOutcome::AnonymousOk
            ) {
                if let Some(r) = registries.iter_mut().find(|r| r.host == host) {
                    r.verified_at_ms = Some(now_ms());
                    changed = true;
                }
            }
            self.tests.insert(host, TestState::Outcome(outcome));
        }
        changed
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Render the Registries screen. Returns `true` if `registries` changed this
/// frame (added, removed, or a test just verified one) and the caller should
/// persist config.
pub fn registries_screen(
    ui: &mut egui::Ui,
    pal: &Palette,
    state: &mut RegistriesScreen,
    registries: &mut Vec<Registry>,
) -> bool {
    let mut changed = state.drain(registries);
    let mut delete: Option<usize> = None;

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
                        RichText::new("Registries")
                            .size(19.0)
                            .strong()
                            .color(pal.text),
                    );
                    ui.add_space(3.0);
                    ui.label(
                        RichText::new(
                            "Docker Hub, GHCR, GitLab, or any generic v2 host. Credentials \
                             live in your OS keychain, never in the config file. A host set up \
                             with its own credential helper — AWS ECR via \
                             docker-credential-ecr-login, for instance — is tested by asking \
                             that helper for a fresh credential, never stored here.",
                        )
                        .small()
                        .color(pal.text_muted),
                    );

                    ui.add_space(16.0);
                    if import_row(ui, pal, state, registries) {
                        changed = true;
                    }
                    if let Some(summary) = &state.import_summary {
                        ui.add_space(6.0);
                        ui.label(RichText::new(summary).small().color(pal.text_muted));
                    }

                    ui.add_space(14.0);
                    if add_form(ui, pal, state, registries) {
                        changed = true;
                    }

                    for (i, r) in registries.iter().enumerate() {
                        ui.add_space(10.0);
                        registry_card(ui, pal, state, r, &mut delete, i);
                    }

                    if registries.is_empty() {
                        ui.add_space(28.0);
                        ui.label(RichText::new("No registries yet.").color(pal.text_faint));
                    }
                    ui.add_space(30.0);
                });
            });
        });

    if let Some(i) = delete {
        if i < registries.len() {
            if let Some(keychain_ref) = &registries[i].keychain_ref {
                let _ = state.secrets.delete(&KeychainRef(keychain_ref.clone()));
            }
            state.tests.remove(&registries[i].host);
            registries.remove(i);
            changed = true;
        }
    }
    changed
}

/// Apply a [`docker_config::ImportReport`]'s decoded credentials into
/// `registries` (update by host if it already exists, else add), and return
/// how many were applied. Shared by the manual "Import from Docker config"
/// button and the automatic startup import (`RockerApp::new`) so both stay
/// in sync — this is the one place that turns an import into config state.
pub fn apply_import(
    report: &rocker_secrets::docker_config::ImportReport,
    registries: &mut Vec<Registry>,
) -> usize {
    let mut applied = 0;
    for entry in &report.imported {
        if let rocker_secrets::docker_config::ImportedEntry::Credential { host, username } = entry {
            if let Some(existing) = registries.iter_mut().find(|r| &r.host == host) {
                existing.username = username.clone();
                existing.auth_type = AuthType::Basic;
                existing.keychain_ref = Some(KeychainRef::registry(host).0.clone());
            } else {
                registries.push(Registry::basic(host.clone(), username.clone()));
            }
            applied += 1;
        }
    }
    applied
}

/// Human-readable summary of an import, e.g. for a notice banner or the
/// screen's own summary line.
pub fn import_summary_text(
    report: &rocker_secrets::docker_config::ImportReport,
    applied: usize,
) -> String {
    let helper_hosts = report.skipped.len();
    if applied == 0 && helper_hosts == 0 {
        "No entries found in ~/.docker/config.json.".to_string()
    } else {
        format!(
            "Imported {applied} credential{}; {helper_hosts} host{} use a \
             credential helper Rocker can't import a secret for.",
            if applied == 1 { "" } else { "s" },
            if helper_hosts == 1 { "" } else { "s" },
        )
    }
}

/// "Import from Docker config" — a local, synchronous file read (no network),
/// safe to run straight in the UI closure. Also runs automatically on every
/// startup (`RockerApp::new`); this button is a manual re-scan for whenever
/// `~/.docker/config.json` changes mid-session (a fresh `docker login`).
fn import_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    state: &mut RegistriesScreen,
    registries: &mut Vec<Registry>,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        if icons::icon_button(
            ui,
            pal,
            Icon::Download,
            None,
            "Import from ~/.docker/config.json",
        )
        .clicked()
        {
            match rocker_secrets::docker_config::import_from_default_location(&*state.secrets) {
                Ok(report) => {
                    let added = apply_import(&report, registries);
                    state.import_summary = Some(import_summary_text(&report, added));
                    changed = added > 0;
                }
                Err(e) => state.import_summary = Some(format!("Import failed: {e}")),
            }
        }
        ui.label(
            RichText::new("Import from Docker config")
                .small()
                .color(pal.text_muted),
        );
    });
    changed
}

/// The "add a registry" row: host / username / password fields plus an
/// "Add & test" button.
fn add_form(
    ui: &mut egui::Ui,
    pal: &Palette,
    state: &mut RegistriesScreen,
    registries: &mut Vec<Registry>,
) -> bool {
    let mut changed = false;
    egui::Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .corner_radius(style::radius(pal.corner))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Add a registry").strong().color(pal.text));
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.add(
                    egui::TextEdit::singleline(&mut state.new_host)
                        .hint_text("host, e.g. ghcr.io")
                        .desired_width(180.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut state.new_username)
                        .hint_text("username")
                        .desired_width(120.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut state.new_password)
                        .hint_text("password / token")
                        .password(true)
                        .desired_width(140.0),
                );
                let can_add = !state.new_host.trim().is_empty();
                if icons::icon_button_enabled(
                    ui,
                    pal,
                    Icon::Plus,
                    Some(pal.accent),
                    "Add & test",
                    can_add,
                )
                .clicked()
                {
                    let host = state.new_host.trim().to_string();
                    let username = state.new_username.trim().to_string();
                    let password = std::mem::take(&mut state.new_password);

                    let _ = state.secrets.set(&KeychainRef::registry(&host), &password);
                    if let Some(existing) = registries.iter_mut().find(|r| r.host == host) {
                        existing.username = username.clone();
                        existing.auth_type = AuthType::Basic;
                        existing.keychain_ref = Some(KeychainRef::registry(&host).0.clone());
                    } else {
                        registries.push(Registry::basic(host.clone(), username.clone()));
                    }
                    state.spawn_test(host, username, password);

                    state.new_host.clear();
                    state.new_username.clear();
                    changed = true;
                }
            });
        });
    changed
}

/// One registry's card: host, auth badge, verified-at, and a Test/Delete
/// action pair. Pushes `i` into `delete` if its trash is pressed, and starts
/// a background probe (landing later via [`RegistriesScreen::drain`]) if
/// "Test" is pressed — neither is reflected in a return value here.
fn registry_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    state: &mut RegistriesScreen,
    r: &Registry,
    delete: &mut Option<usize>,
    i: usize,
) {
    egui::Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .corner_radius(style::radius(pal.corner))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (mark, _) =
                    ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                icons::draw(ui.painter(), Icon::Key, mark, pal.text_muted);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(&r.host).strong().color(pal.text));
                    let sub = match r.auth_type {
                        AuthType::Basic if !r.username.is_empty() => r.username.clone(),
                        AuthType::Basic => "no username set".to_string(),
                        AuthType::Helper => format!(
                            "via {} credential helper",
                            r.helper.as_deref().unwrap_or("docker-credential-*")
                        ),
                    };
                    ui.label(RichText::new(sub).small().color(pal.text_muted));
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icons::icon_button(ui, pal, Icon::Trash, None, "Remove registry").clicked() {
                        *delete = Some(i);
                    }
                    ui.add_space(4.0);
                    let testing = matches!(state.tests.get(&r.host), Some(TestState::Testing));
                    if icons::icon_button_enabled(
                        ui,
                        pal,
                        Icon::Refresh,
                        None,
                        "Test connection",
                        !testing,
                    )
                    .clicked()
                    {
                        match r.auth_type {
                            AuthType::Basic => {
                                let password = r
                                    .keychain_ref
                                    .as_ref()
                                    .and_then(|k| state.secrets.get(&KeychainRef(k.clone())).ok())
                                    .unwrap_or_default();
                                state.spawn_test(r.host.clone(), r.username.clone(), password);
                            }
                            AuthType::Helper => {
                                let helper = r.helper.clone().unwrap_or_default();
                                state.spawn_helper_test(r.host.clone(), helper);
                            }
                        }
                    }
                });
            });

            ui.add_space(6.0);
            status_line(ui, pal, state, r);
        });
}

/// The status line under a card: a coloured outcome, or a last-verified time,
/// or nothing yet — never both a stale timestamp and a fresh failure.
fn status_line(ui: &mut egui::Ui, pal: &Palette, state: &RegistriesScreen, r: &Registry) {
    let (text, color) = match state.tests.get(&r.host) {
        Some(TestState::Testing) => ("Testing…".to_string(), pal.text_muted),
        Some(TestState::Outcome(outcome)) => outcome_text(pal, outcome),
        None => match r.verified_at_ms {
            Some(ts) => (format!("Verified {}", format::ago(ts)), pal.text_faint),
            None => ("Not yet tested".to_string(), pal.text_faint),
        },
    };
    ui.label(RichText::new(text).small().color(color));
}

fn outcome_text(pal: &Palette, outcome: &ConnectionOutcome) -> (String, egui::Color32) {
    match outcome {
        ConnectionOutcome::AnonymousOk => ("Reachable (no auth needed)".to_string(), pal.running),
        ConnectionOutcome::Authenticated => ("Verified".to_string(), pal.running),
        ConnectionOutcome::AuthFailed => ("Authentication failed".to_string(), pal.unhealthy),
        ConnectionOutcome::Unsupported { status } => (
            format!("Unexpected response (HTTP {status})"),
            pal.unhealthy,
        ),
        ConnectionOutcome::NetworkError(e) => (format!("Network error: {e}"), pal.unhealthy),
        ConnectionOutcome::HelperError(e) => (e.clone(), pal.unhealthy),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocker_secrets::docker_config::{ImportReport, ImportedEntry};
    use rocker_secrets::MemorySecretStore;

    fn screen() -> RegistriesScreen {
        RegistriesScreen::new(Arc::new(MemorySecretStore::default()))
    }

    #[test]
    fn apply_import_adds_a_new_host_and_updates_an_existing_one() {
        let mut registries = vec![Registry::basic("ghcr.io", "stale-user")];
        let report = ImportReport {
            imported: vec![
                ImportedEntry::Credential {
                    host: "ghcr.io".to_string(),
                    username: "octo".to_string(),
                },
                ImportedEntry::Credential {
                    host: "registry.gitlab.com".to_string(),
                    username: "octo".to_string(),
                },
            ],
            skipped: vec![],
        };

        let applied = apply_import(&report, &mut registries);

        assert_eq!(applied, 2);
        assert_eq!(registries.len(), 2);
        assert_eq!(registries[0].username, "octo");
        assert_eq!(registries[1].host, "registry.gitlab.com");
    }

    /// Re-running the same import (as happens on every startup) neither
    /// duplicates the entry nor errors — merge by host, not append.
    #[test]
    fn apply_import_is_idempotent_for_the_same_host() {
        let mut registries = Vec::new();
        let report = ImportReport {
            imported: vec![ImportedEntry::Credential {
                host: "ghcr.io".to_string(),
                username: "octo".to_string(),
            }],
            skipped: vec![],
        };

        apply_import(&report, &mut registries);
        apply_import(&report, &mut registries);

        assert_eq!(registries.len(), 1);
    }

    #[test]
    fn import_summary_text_reports_nothing_found() {
        let report = ImportReport::default();
        assert_eq!(
            import_summary_text(&report, 0),
            "No entries found in ~/.docker/config.json."
        );
    }

    /// Lay the whole screen out headlessly at a few widths, both empty and
    /// with entries in every state: catches layout-math panics and confirms
    /// a no-input frame reports no edit.
    #[test]
    fn screen_lays_out_without_panic() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());

        for width in [420.0_f32, 700.0, 1200.0] {
            let mut state = screen();
            state
                .tests
                .insert("testing.example".to_string(), TestState::Testing);
            state.tests.insert(
                "failed.example".to_string(),
                TestState::Outcome(ConnectionOutcome::AuthFailed),
            );
            let mut registries = vec![
                Registry::basic("ghcr.io", "octo"),
                Registry::basic("testing.example", "octo"),
                Registry::basic("failed.example", "octo"),
                Registry::via_helper("123.dkr.ecr.us-east-1.amazonaws.com", "ecr-login"),
            ];
            registries[0].verified_at_ms = Some(now_ms());

            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(width, 640.0),
                )),
                ..Default::default()
            };
            let mut changed = true;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    changed = registries_screen(ui, &pal, &mut state, &mut registries);
                });
            });
            assert!(!changed, "no pointer input, so nothing should change");
            assert_eq!(registries.len(), 4);
        }
    }

    #[test]
    fn empty_registry_list_lays_out_without_panic() {
        let ctx = egui::Context::default();
        let pal = crate::style::install(&ctx, &rocker_theme::Theme::dark());
        let mut state = screen();
        let mut registries = Vec::new();

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(700.0, 640.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                registries_screen(ui, &pal, &mut state, &mut registries);
            });
        });
    }

    /// A finished background probe lands in `tests` and, on success, stamps
    /// `verified_at_ms` — proven without a real thread by feeding the channel
    /// directly.
    #[test]
    fn drain_applies_a_successful_outcome_to_the_matching_registry() {
        let mut state = screen();
        let mut registries = vec![Registry::basic("ghcr.io", "octo")];
        state
            .test_tx
            .send(("ghcr.io".to_string(), ConnectionOutcome::Authenticated))
            .unwrap();

        let changed = state.drain(&mut registries);
        assert!(changed);
        assert!(registries[0].verified_at_ms.is_some());
        assert!(matches!(
            state.tests.get("ghcr.io"),
            Some(TestState::Outcome(ConnectionOutcome::Authenticated))
        ));
    }

    #[test]
    fn drain_records_a_failure_without_stamping_verified_at() {
        let mut state = screen();
        let mut registries = vec![Registry::basic("ghcr.io", "octo")];
        state
            .test_tx
            .send(("ghcr.io".to_string(), ConnectionOutcome::AuthFailed))
            .unwrap();

        let changed = state.drain(&mut registries);
        assert!(!changed);
        assert!(registries[0].verified_at_ms.is_none());
    }

    /// Unlike the `drain_*` tests above (which feed the channel directly),
    /// this exercises the real background thread `spawn_helper_test` starts,
    /// end to end through `probe::test_helper_connection`, for a helper that
    /// isn't installed — proving the Helper test path is wired up, not just
    /// `drain`'s channel handling.
    #[test]
    fn spawn_helper_test_reports_a_missing_helper_through_the_real_thread() {
        let mut state = screen();
        state.spawn_helper_test(
            "123.dkr.ecr.us-east-1.amazonaws.com".to_string(),
            "this-does-not-exist-anywhere".to_string(),
        );

        let (host, outcome) = state
            .test_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the background thread reports back");

        assert_eq!(host, "123.dkr.ecr.us-east-1.amazonaws.com");
        assert!(matches!(outcome, ConnectionOutcome::HelperError(_)));
    }
}
