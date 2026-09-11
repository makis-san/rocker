//! Registry credential metadata (PLAN §5.2, §6). Pure data: the secret
//! itself never lives here, only a reference into the OS keychain that
//! `rocker-secrets::SecretStore` resolves.
//!
//! Native cloud-provider registries (AWS ECR, GCR) are deliberately not
//! modeled here — those ship as extensions against the `registries`
//! capability, not as core providers (PLAN §5.2, §10 Phase 5).

use serde::{Deserialize, Serialize};

/// How a registry entry's credential is obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    /// Username + password/token, tested with a real `/v2/` auth probe.
    /// The secret lives in the keychain at `keychain_ref`.
    Basic,
    /// Backed by a `docker-credential-*` helper reported by
    /// `~/.docker/config.json` (`credHelpers` for this host, or the global
    /// `credsStore`). Rocker holds no secret for these; the helper is
    /// shelled out to when a credential is needed.
    Helper,
}

/// One configured registry (Docker Hub, GHCR, GitLab, or a generic v2 host).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    /// Registry hostname, e.g. `ghcr.io`, `registry.gitlab.com`, or
    /// `https://index.docker.io/v1/` for Docker Hub.
    pub host: String,
    pub username: String,
    pub auth_type: AuthType,
    /// Reference into the OS keychain (a `rocker_secrets::KeychainRef`'s
    /// inner string). Only set for `AuthType::Basic`.
    #[serde(default)]
    pub keychain_ref: Option<String>,
    /// The `docker-credential-*` helper name for `AuthType::Helper` entries
    /// (e.g. `osxkeychain`, `desktop`, `ecr-login`).
    #[serde(default)]
    pub helper: Option<String>,
    /// Unix milliseconds of the last successful auth probe, if any.
    #[serde(default)]
    pub verified_at_ms: Option<u64>,
}

impl Registry {
    pub fn basic(host: impl Into<String>, username: impl Into<String>) -> Self {
        let host = host.into();
        let keychain_ref = Some(format!("registry:{host}"));
        Self {
            host,
            username: username.into(),
            auth_type: AuthType::Basic,
            keychain_ref,
            helper: None,
            verified_at_ms: None,
        }
    }

    pub fn via_helper(host: impl Into<String>, helper: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            username: String::new(),
            auth_type: AuthType::Helper,
            keychain_ref: None,
            helper: Some(helper.into()),
            verified_at_ms: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_registry_gets_a_namespaced_keychain_ref() {
        let reg = Registry::basic("ghcr.io", "octo");
        assert_eq!(reg.auth_type, AuthType::Basic);
        assert_eq!(reg.keychain_ref, Some("registry:ghcr.io".to_string()));
        assert!(reg.helper.is_none());
    }

    #[test]
    fn helper_registry_has_no_keychain_ref() {
        let reg = Registry::via_helper("123.dkr.ecr.us-east-1.amazonaws.com", "ecr-login");
        assert_eq!(reg.auth_type, AuthType::Helper);
        assert_eq!(reg.helper, Some("ecr-login".to_string()));
        assert!(reg.keychain_ref.is_none());
    }

    #[test]
    fn round_trips_through_toml() {
        let reg = Registry::basic("ghcr.io", "octo");
        let text = toml::to_string(&reg).expect("serialize");
        let back: Registry = toml::from_str(&text).expect("deserialize");
        assert_eq!(reg, back);
    }
}
