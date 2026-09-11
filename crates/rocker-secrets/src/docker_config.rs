//! Import existing registry credentials from `~/.docker/config.json`
//! (PLAN §5.2): detect `credsStore` / `credHelpers`, and pull plain `auths`
//! entries straight into the keychain via a [`SecretStore`].
//!
//! This only *reads* the Docker CLI's config; it never writes to it.

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine as _;
use serde::Deserialize;

use crate::{KeychainRef, SecretError, SecretStore};

#[derive(Debug, Default, Deserialize)]
struct RawDockerConfig {
    #[serde(default)]
    auths: HashMap<String, RawAuthEntry>,
    #[serde(rename = "credsStore", default)]
    creds_store: Option<String>,
    #[serde(rename = "credHelpers", default)]
    cred_helpers: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawAuthEntry {
    #[serde(default)]
    auth: Option<String>,
}

/// What happened to one host found while scanning the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportedEntry {
    /// A plain `auths` entry with a decodable `user:pass`, written to the
    /// keychain at `KeychainRef::registry(&host)`.
    Credential { host: String, username: String },
    /// This host is backed by a `docker-credential-*` helper (a per-host
    /// entry in `credHelpers`, or the global `credsStore` when no `auth` was
    /// present) — not a secret Rocker can import, only noted so the caller
    /// can offer to shell out to the helper directly.
    Helper { host: String, helper: String },
    /// The `auth` field did not base64-decode to a `user:pass` pair.
    Malformed { host: String },
}

impl ImportedEntry {
    pub fn host(&self) -> &str {
        match self {
            Self::Credential { host, .. }
            | Self::Helper { host, .. }
            | Self::Malformed { host } => host,
        }
    }
}

/// Result of a config-file scan: entries actually written to the keychain,
/// and entries the caller needs to handle another way.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: Vec<ImportedEntry>,
    pub skipped: Vec<ImportedEntry>,
}

/// The Docker CLI's own config file resolution: `$DOCKER_CONFIG/config.json`
/// when set, else `~/.docker/config.json`.
pub fn default_config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("DOCKER_CONFIG") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir).join("config.json"));
        }
    }
    home_dir().map(|home| home.join(".docker").join("config.json"))
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// Read and import from the Docker CLI's default config path. Returns an
/// empty report (nothing to do, not an error) if the file doesn't exist.
pub fn import_from_default_location(store: &dyn SecretStore) -> crate::Result<ImportReport> {
    let Some(path) = default_config_path() else {
        return Ok(ImportReport::default());
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => import_from_json(&text, store),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ImportReport::default()),
        Err(e) => Err(SecretError::Backend(format!(
            "reading {}: {e}",
            path.display()
        ))),
    }
}

/// Parse `config.json` content and write every decodable `auths` credential
/// into `store`. Entries backed by a helper (`credHelpers` / `credsStore`)
/// are reported but not written anywhere — Rocker holds no secret for them.
pub fn import_from_json(json: &str, store: &dyn SecretStore) -> crate::Result<ImportReport> {
    let raw: RawDockerConfig =
        serde_json::from_str(json).map_err(|e| SecretError::Backend(format!("parsing: {e}")))?;

    let mut report = ImportReport::default();

    for (host, entry) in raw.auths {
        if let Some(helper) = raw.cred_helpers.get(&host).cloned() {
            report.skipped.push(ImportedEntry::Helper { host, helper });
            continue;
        }

        match entry.auth.as_deref().filter(|s| !s.is_empty()) {
            Some(auth) => match decode_user_pass(auth) {
                Some((username, password)) => {
                    store.set(&KeychainRef::registry(&host), &password)?;
                    report
                        .imported
                        .push(ImportedEntry::Credential { host, username });
                }
                None => report.skipped.push(ImportedEntry::Malformed { host }),
            },
            None => {
                // No per-host `auth` and no per-host helper: falls back to
                // the global `credsStore`, if any set.
                if let Some(store_name) = raw.creds_store.clone() {
                    report.skipped.push(ImportedEntry::Helper {
                        host,
                        helper: store_name,
                    });
                } else {
                    report.skipped.push(ImportedEntry::Malformed { host });
                }
            }
        }
    }

    // `credHelpers` entries with no matching `auths` entry still count as a
    // configured host (e.g. a fresh ECR helper wired up but never logged in
    // through `docker login` interactively).
    for (host, helper) in raw.cred_helpers {
        if !report
            .imported
            .iter()
            .chain(report.skipped.iter())
            .any(|e| e.host() == host)
        {
            report.skipped.push(ImportedEntry::Helper { host, helper });
        }
    }

    Ok(report)
}

fn decode_user_pass(auth: &str) -> Option<(String, String)> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(auth.trim())
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let (user, pass) = text.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemorySecretStore;

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn imports_a_plain_hub_credential() {
        let json = format!(
            r#"{{"auths": {{"https://index.docker.io/v1/": {{"auth": "{}"}}}}}}"#,
            b64("octo:hunter2")
        );
        let store = MemorySecretStore::default();
        let report = import_from_json(&json, &store).unwrap();

        assert_eq!(
            report.imported,
            vec![ImportedEntry::Credential {
                host: "https://index.docker.io/v1/".to_string(),
                username: "octo".to_string(),
            }]
        );
        assert!(report.skipped.is_empty());
        assert_eq!(
            store
                .get(&KeychainRef::registry("https://index.docker.io/v1/"))
                .unwrap(),
            "hunter2"
        );
    }

    #[test]
    fn notes_a_per_host_cred_helper_without_storing_a_secret() {
        let json = r#"{
            "auths": { "123.dkr.ecr.us-east-1.amazonaws.com": {} },
            "credHelpers": { "123.dkr.ecr.us-east-1.amazonaws.com": "ecr-login" }
        }"#;
        let store = MemorySecretStore::default();
        let report = import_from_json(json, &store).unwrap();

        assert!(report.imported.is_empty());
        assert_eq!(
            report.skipped,
            vec![ImportedEntry::Helper {
                host: "123.dkr.ecr.us-east-1.amazonaws.com".to_string(),
                helper: "ecr-login".to_string(),
            }]
        );
        assert!(store
            .get(&KeychainRef::registry(
                "123.dkr.ecr.us-east-1.amazonaws.com"
            ))
            .is_err());
    }

    #[test]
    fn notes_the_global_creds_store_for_entries_with_no_auth() {
        let json = r#"{
            "auths": { "ghcr.io": {} },
            "credsStore": "desktop"
        }"#;
        let store = MemorySecretStore::default();
        let report = import_from_json(json, &store).unwrap();

        assert!(report.imported.is_empty());
        assert_eq!(
            report.skipped,
            vec![ImportedEntry::Helper {
                host: "ghcr.io".to_string(),
                helper: "desktop".to_string(),
            }]
        );
    }

    #[test]
    fn flags_malformed_auth_without_failing_the_whole_import() {
        let json = r#"{"auths": {"registry.example": {"auth": "not-valid-base64-user-pass"}}}"#;
        let store = MemorySecretStore::default();
        let report = import_from_json(json, &store).unwrap();

        assert!(report.imported.is_empty());
        assert_eq!(
            report.skipped,
            vec![ImportedEntry::Malformed {
                host: "registry.example".to_string()
            }]
        );
    }

    #[test]
    fn a_bare_cred_helper_with_no_auths_entry_is_still_reported() {
        let json = r#"{
            "credHelpers": { "123.dkr.ecr.us-east-1.amazonaws.com": "ecr-login" }
        }"#;
        let store = MemorySecretStore::default();
        let report = import_from_json(json, &store).unwrap();

        assert_eq!(
            report.skipped,
            vec![ImportedEntry::Helper {
                host: "123.dkr.ecr.us-east-1.amazonaws.com".to_string(),
                helper: "ecr-login".to_string(),
            }]
        );
    }

    #[test]
    fn missing_config_file_is_an_empty_report_not_an_error() {
        // DOCKER_CONFIG pointed at a directory with no config.json.
        let dir = std::env::temp_dir().join(format!(
            "rocker-secrets-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        // Safety: no other thread in this process reads/writes DOCKER_CONFIG.
        unsafe {
            std::env::set_var("DOCKER_CONFIG", &dir);
        }
        let store = MemorySecretStore::default();
        let report = import_from_default_location(&store).unwrap();
        assert_eq!(report, ImportReport::default());
        unsafe {
            std::env::remove_var("DOCKER_CONFIG");
        }
    }
}
