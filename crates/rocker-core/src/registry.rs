//! Built-in extension registry identity shared by config and verification.

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
    0xa1, 0xd8, 0x55, 0x7a, 0x08, 0x06, 0x5f, 0x34, 0x9b, 0x96, 0x99, 0x9a, 0x69, 0xd4, 0x3c, 0xc4,
    0x90, 0x0a, 0xad, 0x0b, 0x8a, 0x21, 0x6d, 0xd3, 0x2e, 0xb0, 0x22, 0xa2, 0x4b, 0xf7, 0x45, 0xf6,
];
