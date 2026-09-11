//! Signed extension-registry metadata and package handling.
//!
//! A registry is deliberately only a transport and discovery mechanism. The
//! client authenticates the exact index bytes and each package with the
//! registry's pinned Ed25519 key before `ExtensionRegistry` extracts anything.

use std::{
    collections::BTreeSet,
    fs,
    io::Cursor,
    path::{Component, Path, PathBuf},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::{HostError, Result};

const INDEX_FORMAT: u32 = 1;
const MAX_INDEX_BYTES: usize = 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 1024;
const MAX_PACKAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_ARCHIVE_FILES: usize = 256;
const MAX_UNPACKED_BYTES: u64 = 64 * 1024 * 1024;

/// Stable identifier for Rocker's built-in, curated extension registry.
pub const OFFICIAL_REGISTRY_ID: &str = "official";
/// Detached signed index served by the official GitHub registry repository.
pub const OFFICIAL_REGISTRY_INDEX_URL: &str =
    "https://raw.githubusercontent.com/makis-san/rocker-registry/main/index-v1.json";
/// Detached signature corresponding to [`OFFICIAL_REGISTRY_INDEX_URL`].
pub const OFFICIAL_REGISTRY_SIGNATURE_URL: &str =
    "https://raw.githubusercontent.com/makis-san/rocker-registry/main/index-v1.sig";
/// Pinned Ed25519 public key for the official registry index and packages.
pub const OFFICIAL_REGISTRY_PUBLIC_KEY: [u8; 32] = [
    0x21, 0xbc, 0x58, 0x89, 0xa2, 0xe5, 0x29, 0x3e, 0xe6, 0xa2, 0x2d, 0xa5, 0x67, 0x8f, 0x04, 0x97,
    0xe9, 0x0b, 0x2c, 0x67, 0xa2, 0xc5, 0x5f, 0xd7, 0x9f, 0x1c, 0xa0, 0x43, 0x4a, 0xf2, 0x1e, 0x0a,
];

/// Build Rocker's built-in curated registry with its compiled-in trust root.
pub fn official_registry() -> Result<TrustedRegistry> {
    TrustedRegistry::new(
        OFFICIAL_REGISTRY_ID,
        OFFICIAL_REGISTRY_INDEX_URL,
        OFFICIAL_REGISTRY_SIGNATURE_URL,
        OFFICIAL_REGISTRY_PUBLIC_KEY,
    )
}

/// A pinned registry identity and the locations of its detached signed index.
#[derive(Debug, Clone)]
pub struct TrustedRegistry {
    id: String,
    index_url: String,
    signature_url: String,
    verifying_key: VerifyingKey,
}

impl TrustedRegistry {
    /// Create a trusted registry from a stable ID, HTTPS locations, and a
    /// 32-byte Ed25519 public key.
    ///
    /// The key is pinned in application configuration or compiled into the
    /// app. It is never accepted from the registry index itself.
    pub fn new(
        id: impl Into<String>,
        index_url: impl Into<String>,
        signature_url: impl Into<String>,
        public_key: [u8; 32],
    ) -> Result<Self> {
        let id = id.into();
        let index_url = index_url.into();
        let signature_url = signature_url.into();
        validate_registry_id(&id)?;
        require_https_url(&index_url)?;
        require_https_url(&signature_url)?;
        let verifying_key = VerifyingKey::from_bytes(&public_key)
            .map_err(|error| HostError::Registry(error.to_string()))?;
        Ok(Self {
            id,
            index_url,
            signature_url,
            verifying_key,
        })
    }

    /// Stable local identifier used for provenance records.
    pub fn id(&self) -> &str {
        &self.id
    }

    fn index_url(&self) -> &str {
        &self.index_url
    }

    fn signature_url(&self) -> &str {
        &self.signature_url
    }

    pub(crate) fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }
}

/// A pluggable byte transport for registry indexes and package downloads.
///
/// Networking remains outside the UI thread: callers can use this trait from
/// the engine's background work queue and tests can provide deterministic
/// in-memory responses.
pub trait RegistryTransport {
    /// Fetch at most `max_bytes` from one HTTPS resource, enforcing the
    /// transport's own timeout and redirect policy. It must return an error if
    /// the response exceeds the supplied limit instead of buffering it all.
    fn fetch(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>>;
}

/// A verified version-one registry index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryIndex {
    /// Registry schema version. Rocker rejects unknown versions rather than
    /// guessing their verification or compatibility semantics.
    pub format: u32,
    /// Every available immutable extension release.
    pub releases: Vec<RegistryRelease>,
}

impl RegistryIndex {
    /// Verify a detached base64 Ed25519 signature over the raw index bytes and
    /// then parse and validate the signed JSON document.
    pub fn verify(
        bytes: &[u8],
        signature_base64: &str,
        verifying_key: &VerifyingKey,
    ) -> Result<Self> {
        verify_signature(bytes, signature_base64, verifying_key)?;
        let index: Self = serde_json::from_slice(bytes)
            .map_err(|error| HostError::Registry(format!("index JSON: {error}")))?;
        index.validate()?;
        Ok(index)
    }

    /// Find all releases for one extension ID, newest-first as specified by
    /// the signed catalog author.
    pub fn releases_for<'a>(
        &'a self,
        id: &'a str,
    ) -> impl Iterator<Item = &'a RegistryRelease> + 'a {
        self.releases.iter().filter(move |release| release.id == id)
    }

    fn validate(&self) -> Result<()> {
        if self.format != INDEX_FORMAT {
            return Err(HostError::Registry(format!(
                "unsupported index format {}; expected {INDEX_FORMAT}",
                self.format
            )));
        }
        let mut release_ids = BTreeSet::new();
        for release in &self.releases {
            release.validate()?;
            if !release_ids.insert((&release.id, &release.version)) {
                return Err(HostError::Registry(format!(
                    "duplicate release {}@{}",
                    release.id, release.version
                )));
            }
        }
        Ok(())
    }
}

/// One immutable package advertised by a signed registry index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryRelease {
    /// Must match the package manifest's extension ID exactly.
    pub id: String,
    /// Must match the package manifest's version exactly.
    pub version: String,
    /// HTTPS address of the `.rockerext` ZIP package.
    pub package_url: String,
    /// Lowercase hexadecimal SHA-256 of the archive bytes.
    pub sha256: String,
    /// Base64 Ed25519 signature over the archive bytes.
    pub signature: String,
    /// Optional minimum compatible Rocker version for the caller's install UI
    /// and compatibility policy.
    #[serde(default)]
    pub min_rocker_version: Option<String>,
}

impl RegistryRelease {
    /// Verify archive size, digest, and detached package signature before it
    /// can be extracted or inspected as an extension.
    pub fn verify_package(&self, package: &[u8], verifying_key: &VerifyingKey) -> Result<()> {
        self.validate()?;
        if package.len() > MAX_PACKAGE_BYTES {
            return Err(HostError::ArchiveTooLarge {
                limit: MAX_PACKAGE_BYTES as u64,
            });
        }
        let digest = sha256_hex(package);
        if digest != self.sha256 {
            return Err(HostError::PackageDigestMismatch);
        }
        verify_signature(package, &self.signature, verifying_key)
    }

    fn validate(&self) -> Result<()> {
        validate_registry_id(&self.id)?;
        if self.version.is_empty() || self.version.len() > 128 {
            return Err(HostError::Registry(format!(
                "release {} has an invalid version",
                self.id
            )));
        }
        require_https_url(&self.package_url)?;
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(HostError::Registry(format!(
                "release {} has an invalid SHA-256 digest",
                self.id
            )));
        }
        Ok(())
    }
}

/// Retrieves and verifies a catalog through an application-owned transport.
#[derive(Debug)]
pub struct RegistryClient<T> {
    registry: TrustedRegistry,
    transport: T,
}

impl<T> RegistryClient<T>
where
    T: RegistryTransport,
{
    /// Construct a client for one pinned registry and background-safe
    /// transport implementation.
    pub fn new(registry: TrustedRegistry, transport: T) -> Self {
        Self {
            registry,
            transport,
        }
    }

    /// Fetch and authenticate the detached index before exposing its releases.
    pub fn fetch_index(&self) -> Result<RegistryIndex> {
        let index = self
            .transport
            .fetch(self.registry.index_url(), MAX_INDEX_BYTES)?;
        let signature = self
            .transport
            .fetch(self.registry.signature_url(), MAX_SIGNATURE_BYTES)?;
        let signature = std::str::from_utf8(&signature)
            .map_err(|error| HostError::Registry(format!("index signature is not UTF-8: {error}")))?
            .trim();
        RegistryIndex::verify(&index, signature, self.registry.verifying_key())
    }

    /// Download and authenticate the immutable archive advertised by `release`.
    pub fn download_package(&self, release: &RegistryRelease) -> Result<Vec<u8>> {
        let package = self
            .transport
            .fetch(&release.package_url, MAX_PACKAGE_BYTES)?;
        release.verify_package(&package, self.registry.verifying_key())?;
        Ok(package)
    }

    /// Return the registry identity this client is pinned to.
    pub fn registry(&self) -> &TrustedRegistry {
        &self.registry
    }
}

pub(crate) fn extract_package(package: &[u8], destination: &Path) -> Result<()> {
    let mut archive = ZipArchive::new(Cursor::new(package))
        .map_err(|error| HostError::Archive(error.to_string()))?;
    if archive.len() > MAX_ARCHIVE_FILES {
        return Err(HostError::Archive(format!(
            "contains more than {MAX_ARCHIVE_FILES} files"
        )));
    }

    let mut paths = BTreeSet::new();
    let mut unpacked = 0_u64;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|error| HostError::Archive(error.to_string()))?;
        let path = safe_archive_path(file.name())?;
        if !paths.insert(path.clone()) {
            return Err(HostError::UnsafeArchiveEntry(file.name().to_owned()));
        }
        if is_symbolic_link(file.unix_mode()) {
            return Err(HostError::UnsafeArchiveEntry(file.name().to_owned()));
        }
        if file.is_dir() {
            fs::create_dir_all(destination.join(path))?;
            continue;
        }

        unpacked = unpacked
            .checked_add(file.size())
            .ok_or(HostError::ArchiveTooLarge {
                limit: MAX_UNPACKED_BYTES,
            })?;
        if unpacked > MAX_UNPACKED_BYTES {
            return Err(HostError::ArchiveTooLarge {
                limit: MAX_UNPACKED_BYTES,
            });
        }

        let output_path = destination.join(path);
        let parent = output_path
            .parent()
            .ok_or_else(|| HostError::UnsafeArchiveEntry(output_path.display().to_string()))?;
        fs::create_dir_all(parent)?;
        let mut output = fs::File::create(output_path)?;
        std::io::copy(&mut file, &mut output)?;
    }
    Ok(())
}

fn verify_signature(
    bytes: &[u8],
    signature_base64: &str,
    verifying_key: &VerifyingKey,
) -> Result<()> {
    let signature_bytes = BASE64
        .decode(signature_base64.trim())
        .map_err(|_| HostError::InvalidSignature)?;
    let signature: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| HostError::InvalidSignature)?;
    let signature = Signature::from_bytes(&signature);
    verifying_key
        .verify(bytes, &signature)
        .map_err(|_| HostError::InvalidSignature)
}

fn validate_registry_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
    {
        return Err(HostError::Registry(format!(
            "registry extension ID `{id}` is invalid"
        )));
    }
    Ok(())
}

fn require_https_url(url: &str) -> Result<()> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(HostError::Registry(format!("URL `{url}` is invalid")));
    };
    if scheme != "https"
        || rest.is_empty()
        || rest.starts_with('/')
        || rest.contains(char::is_whitespace)
    {
        return Err(HostError::Registry(format!("URL `{url}` must use HTTPS")));
    }
    Ok(())
}

fn safe_archive_path(name: &str) -> Result<PathBuf> {
    let path = Path::new(name);
    if name.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(HostError::UnsafeArchiveEntry(name.to_owned()));
    }
    Ok(path.to_path_buf())
}

fn is_symbolic_link(mode: Option<u32>) -> bool {
    mode.is_some_and(|mode| mode & 0o170000 == 0o120000)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &byte in digest.as_slice() {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    hex
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, io::Write};

    use ed25519_dalek::{Signer, SigningKey};
    use tempfile::tempdir;
    use zip::{write::SimpleFileOptions, ZipWriter};

    use super::*;

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    #[test]
    fn official_registry_uses_the_pinned_catalog_identity() {
        let registry = official_registry().expect("official registry is valid");

        assert_eq!(registry.id(), OFFICIAL_REGISTRY_ID);
    }

    fn signature(bytes: &[u8]) -> String {
        BASE64.encode(signing_key().sign(bytes).to_bytes())
    }

    fn release(package: &[u8]) -> RegistryRelease {
        RegistryRelease {
            id: "example.summary".into(),
            version: "1.2.3".into(),
            package_url: "https://registry.example/extensions/example.summary-1.2.3.rockerext"
                .into(),
            sha256: sha256_hex(package),
            signature: signature(package),
            min_rocker_version: None,
        }
    }

    fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            for (path, contents) in entries {
                writer
                    .start_file(path, SimpleFileOptions::default())
                    .expect("test archive file starts");
                writer
                    .write_all(contents.as_bytes())
                    .expect("test archive file writes");
            }
            writer.finish().expect("test archive finishes");
        }
        cursor.into_inner()
    }

    #[test]
    fn index_verify_accepts_signed_catalog() {
        let document = br#"{"format":1,"releases":[]}"#;
        let index = RegistryIndex::verify(
            document,
            &signature(document),
            &signing_key().verifying_key(),
        )
        .expect("signed index verifies");

        assert_eq!(index.format, 1);
    }

    #[test]
    fn index_verify_rejects_tampered_catalog() {
        let original = br#"{"format":1,"releases":[]}"#;
        let tampered = br#"{"format":2,"releases":[]}"#;

        let error = RegistryIndex::verify(
            tampered,
            &signature(original),
            &signing_key().verifying_key(),
        )
        .expect_err("tampered index fails verification");

        assert!(matches!(error, HostError::InvalidSignature));
    }

    #[test]
    fn extract_package_rejects_path_traversal() {
        let package = archive(&[("../extension.toml", "bad")]);
        let destination = tempdir().expect("destination exists");

        let error =
            extract_package(&package, destination.path()).expect_err("traversal is rejected");

        assert!(matches!(error, HostError::UnsafeArchiveEntry(_)));
    }

    #[test]
    fn registry_package_install_records_verified_provenance() {
        let package = archive(&[
            (
                "extension.toml",
                r#"
                    id = "example.summary"
                    name = "Summary"
                    version = "1.2.3"
                    tier = "script"
                    entry = "main.rhai"
                "#,
            ),
            ("main.rhai", "fn activate() {}"),
        ]);
        let trusted = TrustedRegistry::new(
            "official",
            "https://registry.example/index-v1.json",
            "https://registry.example/index-v1.sig",
            signing_key().verifying_key().to_bytes(),
        )
        .expect("trusted registry is valid");
        let mut registry =
            crate::ExtensionRegistry::load(tempdir().expect("install root exists").keep())
                .expect("extension registry loads");

        let installed = registry
            .install_registry_package(&trusted, &release(&package), &package)
            .expect("signed package installs");

        assert_eq!(installed.manifest.id, "example.summary");
        assert_eq!(
            registry
                .provenance("example.summary")
                .map(|origin| origin.registry_id.as_str()),
            Some("official")
        );
    }

    struct MemoryTransport(BTreeMap<String, Vec<u8>>);

    impl RegistryTransport for MemoryTransport {
        fn fetch(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>> {
            let resource = self
                .0
                .get(url)
                .cloned()
                .ok_or_else(|| HostError::Registry(format!("missing test resource {url}")))?;
            if resource.len() > max_bytes {
                return Err(HostError::ArchiveTooLarge {
                    limit: max_bytes as u64,
                });
            }
            Ok(resource)
        }
    }

    #[test]
    fn client_fetch_index_verifies_detached_signature() {
        let index = br#"{"format":1,"releases":[]}"#.to_vec();
        let registry = TrustedRegistry::new(
            "official",
            "https://registry.example/index-v1.json",
            "https://registry.example/index-v1.sig",
            signing_key().verifying_key().to_bytes(),
        )
        .expect("trusted registry is valid");
        let transport = MemoryTransport(BTreeMap::from([
            (registry.index_url().to_owned(), index.clone()),
            (
                registry.signature_url().to_owned(),
                signature(&index).into_bytes(),
            ),
        ]));
        let client = RegistryClient::new(registry, transport);

        let verified = client.fetch_index().expect("index is verified");

        assert!(verified.releases.is_empty());
    }

    #[test]
    fn release_verify_package_rejects_digest_mismatch() {
        let package = archive(&[("main.rhai", "safe")]);
        let mut release = release(&package);
        release.sha256 = "0".repeat(64);

        let error = release
            .verify_package(&package, &signing_key().verifying_key())
            .expect_err("digest mismatch fails");

        assert!(matches!(error, HostError::PackageDigestMismatch));
    }
}
