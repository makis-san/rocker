//! Credential storage. Secrets live only in the OS keychain; config holds a
//! reference, never the value (PLAN §5.2, §7).
//!
//! [`KeyringSecretStore`] is the production backend, wrapping the `keyring`
//! crate (macOS Keychain / Windows Credential Manager / Linux secret-service
//! over `zbus`). [`MemorySecretStore`] is a non-persistent fake for tests and
//! headless runs. [`docker_config`] imports existing credentials from
//! `~/.docker/config.json`. [`probe`] runs a real `/v2/` auth check against a
//! registry. [`credential_helper`] shells out to a `docker-credential-*`
//! helper binary for a `Helper`-typed registry, the same request `docker
//! login`/`docker pull` make.
//!
//! Cloud-native registries (AWS ECR, GCR, ...) are not modeled with any
//! provider-specific code here — a user sets them up the standard way (a
//! `credHelpers` entry in `~/.docker/config.json` pointing at, say,
//! `docker-credential-ecr-login`), `docker_config` imports the host as
//! `Helper`-typed with no secret stored, and `credential_helper` resolves a
//! fresh credential from it exactly like the Docker CLI would. No AWS SDK or
//! other cloud-provider dependency belongs in this crate (PLAN §5.2).

pub mod credential_helper;
pub mod docker_config;
pub mod probe;

use std::collections::HashMap;
use std::sync::Mutex;

use thiserror::Error;

/// Reverse-DNS namespace `keyring` entries are stored under, matching the
/// Flatpak/desktop app id so credentials don't collide with other apps that
/// happen to use the same host string as a keyring key.
const SERVICE: &str = "io.github.makis_san.Rocker";

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("no secret stored for {0}")]
    NotFound(String),
    #[error("keychain: {0}")]
    Backend(String),
}

pub type Result<T> = std::result::Result<T, SecretError>;

/// An opaque reference persisted in config in place of the secret itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeychainRef(pub String);

impl KeychainRef {
    /// Reference for a registry credential, namespaced by host.
    pub fn registry(host: &str) -> Self {
        Self(format!("registry:{host}"))
    }
}

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &KeychainRef) -> Result<String>;
    fn set(&self, key: &KeychainRef, secret: &str) -> Result<()>;
    fn delete(&self, key: &KeychainRef) -> Result<()>;
}

/// Non-persistent store for tests and headless runs.
#[derive(Default)]
pub struct MemorySecretStore {
    map: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key: &KeychainRef) -> Result<String> {
        self.map
            .lock()
            .unwrap()
            .get(&key.0)
            .cloned()
            .ok_or_else(|| SecretError::NotFound(key.0.clone()))
    }

    fn set(&self, key: &KeychainRef, secret: &str) -> Result<()> {
        self.map
            .lock()
            .unwrap()
            .insert(key.0.clone(), secret.to_string());
        Ok(())
    }

    fn delete(&self, key: &KeychainRef) -> Result<()> {
        self.map.lock().unwrap().remove(&key.0);
        Ok(())
    }
}

/// The OS keychain, via the `keyring` crate. Each [`KeychainRef`] becomes a
/// `keyring::Entry` under the fixed [`SERVICE`] namespace, keyed by the
/// reference's own string (already host-namespaced, e.g. `registry:ghcr.io`).
#[derive(Default)]
pub struct KeyringSecretStore {
    _private: (),
}

impl KeyringSecretStore {
    pub fn new() -> Self {
        Self { _private: () }
    }

    fn entry(&self, key: &KeychainRef) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, &key.0).map_err(|e| SecretError::Backend(e.to_string()))
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, key: &KeychainRef) -> Result<String> {
        self.entry(key)?.get_password().map_err(|e| match e {
            keyring::Error::NoEntry => SecretError::NotFound(key.0.clone()),
            other => SecretError::Backend(other.to_string()),
        })
    }

    fn set(&self, key: &KeychainRef, secret: &str) -> Result<()> {
        self.entry(key)?
            .set_password(secret)
            .map_err(|e| SecretError::Backend(e.to_string()))
    }

    fn delete(&self, key: &KeychainRef) -> Result<()> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Backend(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips() {
        let store = MemorySecretStore::default();
        let key = KeychainRef::registry("ghcr.io");
        assert!(matches!(store.get(&key), Err(SecretError::NotFound(_))));

        store.set(&key, "s3cr3t").unwrap();
        assert_eq!(store.get(&key).unwrap(), "s3cr3t");

        store.delete(&key).unwrap();
        assert!(matches!(store.get(&key), Err(SecretError::NotFound(_))));
    }

    /// Deleting an absent key is a no-op, not an error — both backends agree
    /// on this so callers don't need to check existence first.
    #[test]
    fn deleting_an_absent_key_is_not_an_error() {
        let store = MemorySecretStore::default();
        store
            .delete(&KeychainRef::registry("nope.example"))
            .unwrap();
    }

    /// Real OS-keychain round trips need a live backend (macOS Keychain,
    /// Windows Credential Manager, or a Linux secret-service daemon), which
    /// CI runners don't provide. Run this manually with
    /// `cargo test -p rocker-secrets -- --ignored` on a machine that has one.
    #[test]
    #[ignore = "requires a live OS keychain / secret-service backend"]
    fn keyring_store_round_trips() {
        let store = KeyringSecretStore::new();
        let key = KeychainRef::registry("rocker-secrets-test.example");
        store.set(&key, "s3cr3t").unwrap();
        assert_eq!(store.get(&key).unwrap(), "s3cr3t");
        store.delete(&key).unwrap();
        assert!(matches!(store.get(&key), Err(SecretError::NotFound(_))));
    }
}
