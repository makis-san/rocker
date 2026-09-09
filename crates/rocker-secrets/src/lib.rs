//! Credential storage. Secrets live only in the OS keychain; config holds a
//! reference, never the value (PLAN §5.2, §7).
//!
//! This scaffold defines the [`SecretStore`] seam and an in-memory fake. The
//! `keyring`-backed implementation and the `docker-credential-*` helper bridge
//! land in Phase 3.

use std::collections::HashMap;
use std::sync::Mutex;

use thiserror::Error;

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
