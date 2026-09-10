//! `self-update`: fetch the latest GitHub release, verify it, and atomically
//! replace the running executable.
//!
//! Trust chain: a minisign key (compiled in) signs `SHA256SUMS`; `SHA256SUMS`
//! pins the archive by hash. Both must check out before anything touches the
//! binary. Downloads go over rustls (ureq), no system OpenSSL.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::{GITHUB_REPO, VERSION};

/// minisign public key that signs `SHA256SUMS` on every release (the base64
/// blob from `rocker.pub`, second line). The private half lives only in the
/// `MINISIGN_SECRET_KEY` CI secret; rotating it means changing this constant.
/// Set to `"UNCONFIGURED"` to make `self-update` refuse to apply an update.
const MINISIGN_PUBKEY: &str = "RWTfmkhmM6bfkPa36B5q/LZZ4LEY5tVCqAO5t5fkiGbOBp5ztbkwc3VE";

const USER_AGENT: &str = concat!("rocker-setup/", env!("CARGO_PKG_VERSION"));
/// Hard ceiling on a downloaded artifact (well above any real archive).
const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;

/// Options for [`run`].
#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// Only report whether an update exists; don't download or apply it.
    pub check_only: bool,
    /// Update to this exact tag instead of the latest release.
    pub tag: Option<String>,
}

/// Result of [`run`].
#[derive(Debug)]
pub enum UpdateOutcome {
    /// Already on the newest version (or newer).
    UpToDate { version: String },
    /// `check_only`: a newer release is available.
    UpdateAvailable { current: String, latest: String },
    /// The binary was replaced; restart to use it.
    Updated { from: String, to: String },
}

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Run the updater.
pub fn run(opts: &UpdateOptions) -> anyhow::Result<UpdateOutcome> {
    let release = fetch_release(opts.tag.as_deref())?;
    let latest = release.tag_name.clone();

    if opts.check_only {
        return Ok(if is_newer(&latest, VERSION) {
            UpdateOutcome::UpdateAvailable {
                current: VERSION.to_string(),
                latest,
            }
        } else {
            UpdateOutcome::UpToDate {
                version: VERSION.to_string(),
            }
        });
    }
    if opts.tag.is_none() && !is_newer(&latest, VERSION) {
        return Ok(UpdateOutcome::UpToDate {
            version: VERSION.to_string(),
        });
    }

    let pubkey = pubkey()?; // fail early, before any download, if unconfigured

    let archive_name = archive_name();
    let archive_url = asset_url(&release, &archive_name)
        .with_context(|| format!("release {latest} has no asset named {archive_name}"))?;
    let sums_url = asset_url(&release, "SHA256SUMS").context("release has no SHA256SUMS")?;
    let sig_url =
        asset_url(&release, "SHA256SUMS.minisig").context("release has no SHA256SUMS.minisig")?;

    let work = TempDir::new("rocker-update")?;
    let archive_path = work.path().join(&archive_name);
    download(&archive_url, &archive_path)?;
    let sums = download_string(&sums_url)?;
    let sig = download_string(&sig_url)?;

    // 1. the key vouches for SHA256SUMS ...
    let signature = minisign_verify::Signature::decode(&sig)
        .map_err(|e| anyhow::anyhow!("parse SHA256SUMS.minisig: {e}"))?;
    pubkey
        .verify(sums.as_bytes(), &signature, false)
        .map_err(|e| anyhow::anyhow!("SHA256SUMS signature does not verify: {e}"))?;

    // 2. ... and SHA256SUMS vouches for the archive.
    let want = expected_hash(&sums, &archive_name)
        .with_context(|| format!("{archive_name} is not listed in SHA256SUMS"))?;
    let got = sha256_file(&archive_path)?;
    anyhow::ensure!(
        got.eq_ignore_ascii_case(&want),
        "checksum mismatch for {archive_name}: expected {want}, got {got}"
    );

    // 3. extract, find the binary, swap it in.
    let extract_dir = work.path().join("x");
    std::fs::create_dir_all(&extract_dir)?;
    extract(&archive_path, &extract_dir)?;
    let new_bin =
        find_binary(&extract_dir).context("extracted archive did not contain the rocker binary")?;

    self_replace::self_replace(&new_bin)
        .context("replace the running executable with the new build")?;

    refresh_metadata();

    Ok(UpdateOutcome::Updated {
        from: VERSION.to_string(),
        to: latest,
    })
}

/// The tag of the latest release (used by `doctor`).
pub(crate) fn latest_tag() -> anyhow::Result<String> {
    Ok(fetch_release(None)?.tag_name)
}

fn fetch_release(tag: Option<&str>) -> anyhow::Result<Release> {
    let url = match tag {
        Some(t) => format!("https://api.github.com/repos/{GITHUB_REPO}/releases/tags/{t}"),
        None => format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest"),
    };
    let body = ureq::get(&url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .with_context(|| format!("GET {url}"))?
        .into_body()
        .read_to_string()
        .context("read GitHub API response")?;
    serde_json::from_str(&body).context("parse GitHub release JSON")
}

fn asset_url(release: &Release, name: &str) -> Option<String> {
    release
        .assets
        .iter()
        .find(|a| a.name == name)
        .map(|a| a.browser_download_url.clone())
}

/// `rocker-<triple>.tar.xz` on unix, `rocker-<triple>.zip` on Windows — the
/// names `dist` gives its archives.
fn archive_name() -> String {
    let triple = env!("ROCKER_TARGET");
    if cfg!(windows) {
        format!("rocker-{triple}.zip")
    } else {
        format!("rocker-{triple}.tar.xz")
    }
}

fn download(url: &str, dest: &Path) -> anyhow::Result<()> {
    let resp = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let mut reader = resp.into_body().into_reader().take(MAX_ARTIFACT_BYTES);
    let mut file =
        std::fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?;
    let n = std::io::copy(&mut reader, &mut file)
        .with_context(|| format!("download to {}", dest.display()))?;
    anyhow::ensure!(n > 0, "downloaded 0 bytes from {url}");
    anyhow::ensure!(
        n < MAX_ARTIFACT_BYTES,
        "artifact exceeds {MAX_ARTIFACT_BYTES} bytes"
    );
    Ok(())
}

fn download_string(url: &str) -> anyhow::Result<String> {
    ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("GET {url}"))?
        .into_body()
        .read_to_string()
        .with_context(|| format!("read {url}"))
}

/// Parse a `sha256sum`-style file: `<hex>  <name>` or `<hex> *<name>`.
fn expected_hash(sums: &str, target: &str) -> Option<String> {
    for line in sums.lines() {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        let name = Path::new(name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(name);
        if name == target {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

/// Extract a `.tar.xz` / `.tar.gz` / `.zip` via the system `tar` (GNU tar and
/// the bsdtar shipped in modern macOS and Windows all autodetect compression
/// and read zip). Keeps this crate free of an archive-format dependency.
fn extract(archive: &Path, into: &Path) -> anyhow::Result<()> {
    let status = std::process::Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .status()
        .context("run `tar` to unpack the download (is tar on PATH?)")?;
    anyhow::ensure!(status.success(), "`tar` exited with {status}");
    Ok(())
}

fn find_binary(root: &Path) -> Option<PathBuf> {
    let wanted = if cfg!(windows) {
        "rocker.exe"
    } else {
        "rocker"
    };
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some(wanted) {
                return Some(path);
            }
        }
    }
    None
}

/// After the swap, rewrite the desktop/bundle metadata so version strings and
/// `Exec=` paths match the new build. Failure here is not fatal.
fn refresh_metadata() {
    let opts = crate::InstallOptions {
        refresh_only: true,
        ..Default::default()
    };
    if let Err(e) = crate::install(&opts) {
        tracing::warn!(error = %e, "couldn't refresh desktop metadata after update");
    }
}

fn pubkey() -> anyhow::Result<minisign_verify::PublicKey> {
    anyhow::ensure!(
        MINISIGN_PUBKEY != "UNCONFIGURED",
        "this build has no release-signing key compiled in, so `self-update` can't \
         verify a download. Re-run the install script to update instead."
    );
    minisign_verify::PublicKey::from_base64(MINISIGN_PUBKEY)
        .map_err(|e| anyhow::anyhow!("embedded minisign key is invalid: {e}"))
}

/// `latest` is a newer version than `current` (both may carry a leading `v`).
pub(crate) fn is_newer(latest: &str, current: &str) -> bool {
    version_key(latest) > version_key(current)
}

fn version_key(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches(['v', 'V'])
        .split(['.', '-', '+'])
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect()
}

/// Minimal self-cleaning temp directory (no `tempfile` dep at runtime).
struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> anyhow::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create temp dir {}", dir.display()))?;
        Ok(Self(dir))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v0.1.10", "v0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.3", "0.1.3"));
        assert!(!is_newer("0.1.2", "0.1.3"));
    }

    #[test]
    fn parses_sha256sums_line() {
        let sums = "abc123  rocker-x86_64-unknown-linux-gnu.tar.xz\n\
                    def456 *SHA256SUMS\n";
        assert_eq!(
            expected_hash(sums, "rocker-x86_64-unknown-linux-gnu.tar.xz").as_deref(),
            Some("abc123")
        );
        assert_eq!(expected_hash(sums, "missing.tar.xz"), None);
    }

    #[test]
    fn embedded_key_parses() {
        // A real release key is compiled in; `self-update` should get past the
        // "unconfigured" guard.
        assert!(
            pubkey().is_ok(),
            "MINISIGN_PUBKEY should be a valid minisign key"
        );
    }

    #[test]
    fn embedded_key_verifies_a_real_signature() {
        // Fixture produced by `rsign sign -W` with the matching secret key over
        // the exact bytes below. Guards the key format + prehashed-signature
        // path that ships in the binary.
        let pk = pubkey().unwrap();
        let sig_text = "untrusted comment: signature from rsign secret key\n\
RUTfmkhmM6bfkL2aanmvJkFQwuM8mLOk6v2ko9wd86b0qHaVgTr5AuC8i7Sva9UjsFwix3xIYV6mioiVazKZ+FbiQkhOpeGNZQU=\n\
trusted comment: rocker release SHA256SUMS\n\
ZvUZT5YmohBMzmnawfjJRJKYRzKmSFQ/1w3MQdjxFpH4iZRJ634FbHYZDHP1xYuV0fKccRK5av/h2b4+kt7qDg==\n";
        let sig = minisign_verify::Signature::decode(sig_text).expect("decode signature");
        let message = b"abc123  rocker-x86_64-unknown-linux-gnu.tar.xz\n";
        pk.verify(message, &sig, false)
            .expect("signature should verify");
        // A tampered message must fail.
        assert!(pk.verify(b"tampered\n", &sig, false).is_err());
    }

    #[test]
    fn hex_encoding() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xff, 0xa5]), "000fffa5");
    }
}
