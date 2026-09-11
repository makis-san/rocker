//! Turns the standalone `rocker` binary into something that installs, removes,
//! and updates itself — with real desktop integration (menu entry, icon, URL
//! handler) on every OS and desktop, no package manager required.
//!
//! The `rocker` binary calls [`cli::run`] before it starts the GUI: if the
//! first argument is a management verb it runs here and the process exits;
//! otherwise control returns and the window opens as usual.
//!
//! Everything is best-effort and userspace by default. `install` writes to
//! `~/.local`, `~/Applications`, or `%LOCALAPPDATA%`; `--system` opts into the
//! shared prefix and is the only path that may need elevation.

use std::path::PathBuf;

mod assets;
pub mod cli;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
#[path = "unsupported.rs"]
mod platform;

#[cfg(feature = "self-update")]
mod update;
#[cfg(feature = "self-update")]
pub use update::{UpdateOptions, UpdateOutcome};

/// Reverse-DNS application id — the desktop-entry stem, the macOS bundle id,
/// and the Windows registry key name.
pub const APP_ID: &str = "io.github.makis_san.Rocker";
/// Human-facing name (window title, menu label, `.app` name).
pub const APP_NAME: &str = "Rocker";
/// On-disk binary name.
pub const BIN_NAME: &str = "rocker";
/// On-disk name of the companion process used to run extensions.
pub const EXT_HOST_BIN_NAME: &str = "rocker-ext-host";
/// `owner/repo` the self-updater queries for releases.
pub const GITHUB_REPO: &str = "makis-san/rocker";
/// Version of this build (the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where an install lands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scope {
    /// `~/.local`, `~/Applications`, `%LOCALAPPDATA%` — no elevation.
    #[default]
    User,
    /// The shared prefix (`/usr/local`, `/Applications`, `%PROGRAMFILES%`).
    System,
}

/// Options for [`install`].
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    pub scope: Scope,
    /// Add the binary directory to the user's `PATH` (shell rc / registry).
    /// Off by default: we print the line and let the user opt in.
    pub modify_path: bool,
    /// Override the directory the binary is copied into.
    pub bin_dir: Option<PathBuf>,
    /// Optional extension-host executable to install beside the main binary.
    pub ext_host: Option<PathBuf>,
    /// Rewrite the desktop/bundle metadata only; don't touch the binary.
    /// Used by `self-update` after it swaps the executable.
    pub refresh_only: bool,
}

/// Options for [`uninstall`].
#[derive(Debug, Clone, Default)]
pub struct UninstallOptions {
    pub scope: Scope,
    /// Also delete `~/.config/rocker` and the history database.
    pub purge: bool,
}

/// One filesystem or system change a verb made, for the CLI to print and for
/// tests to assert on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub verb: ChangeVerb,
    /// What changed — a path, a registry key, or a hook command.
    pub target: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeVerb {
    Created,
    Updated,
    Removed,
    Skipped,
    Hook,
    Warning,
}

impl Change {
    fn new(verb: ChangeVerb, target: impl Into<String>) -> Self {
        Self {
            verb,
            target: target.into(),
            note: None,
        }
    }
    fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// The outcome of a verb: an ordered log of what it did.
#[derive(Debug, Default)]
pub struct Report {
    pub changes: Vec<Change>,
}

impl Report {
    fn push(&mut self, c: Change) {
        self.changes.push(c);
    }
    /// Did anything actually change (vs. everything already in place)?
    pub fn made_changes(&self) -> bool {
        self.changes.iter().any(|c| {
            matches!(
                c.verb,
                ChangeVerb::Created | ChangeVerb::Updated | ChangeVerb::Removed
            )
        })
    }
}

/// Install the running binary and its desktop integration.
pub fn install(opts: &InstallOptions) -> anyhow::Result<Report> {
    let mut report = Report::default();
    platform::install(opts, &mut report)?;
    Ok(report)
}

/// Remove everything [`install`] created (keeps user config unless `purge`).
pub fn uninstall(opts: &UninstallOptions) -> anyhow::Result<Report> {
    let mut report = Report::default();
    platform::uninstall(opts, &mut report)?;
    Ok(report)
}

/// A read-only health check: where the binary is, whether the desktop
/// integration is in place, and — with the `self-update` feature — whether a
/// newer release exists.
pub fn doctor() -> anyhow::Result<Diagnosis> {
    platform::doctor()
}

/// Structured [`doctor`] output.
#[derive(Debug)]
pub struct Diagnosis {
    pub version: &'static str,
    pub running_exe: Option<PathBuf>,
    pub target_triple: &'static str,
    /// `(label, path, present)` for each integration artifact.
    pub artifacts: Vec<(String, PathBuf, bool)>,
    /// Human-readable notes (PATH state, available hooks, sandbox).
    pub notes: Vec<String>,
    /// `Some((latest_tag, is_newer))` when the update check ran.
    pub latest_release: Option<(String, bool)>,
}

/// Best-effort: run `program args...` with a short timeout, swallow failure,
/// and record it as a hook in `report`. Missing programs are silently skipped.
fn run_hook(report: &mut Report, program: &str, args: &[&str]) {
    use std::process::{Command, Stdio};
    let found = Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match found {
        Ok(status) if status.success() => {
            report.push(Change::new(
                ChangeVerb::Hook,
                format!("{program} {}", args.join(" ")),
            ));
        }
        Ok(_) => {
            report.push(
                Change::new(ChangeVerb::Hook, format!("{program} {}", args.join(" ")))
                    .with_note("non-zero exit (ignored)"),
            );
        }
        Err(_) => { /* not installed — nothing to do */ }
    }
}

/// `true` when we're running inside a Flatpak sandbox, where writes to the
/// host's `~/.local` aren't visible outside the sandbox.
fn is_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || std::path::Path::new("/.flatpak-info").exists()
}

/// Resolve `$HOME`, erroring clearly if it's unset.
fn home() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| anyhow::anyhow!("$HOME is not set; can't resolve the install location"))
}

/// Write `contents` to `path` atomically (temp file in the same dir, then
/// rename) and record the change. Marks Updated vs Created.
fn write_file(report: &mut Report, path: &std::path::Path, contents: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    let existed = path.exists();
    if existed {
        if let Ok(current) = std::fs::read(path) {
            if current == contents {
                report.push(Change::new(ChangeVerb::Skipped, path.display().to_string()));
                return Ok(());
            }
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("create {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension(format!(
        "tmp-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let mut f = std::fs::File::create(&tmp)
        .map_err(|e| anyhow::anyhow!("create {}: {e}", tmp.display()))?;
    f.write_all(contents)
        .and_then(|_| f.sync_all())
        .map_err(|e| anyhow::anyhow!("write {}: {e}", tmp.display()))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::anyhow!("place {}: {e}", path.display())
    })?;
    report.push(Change::new(
        if existed {
            ChangeVerb::Updated
        } else {
            ChangeVerb::Created
        },
        path.display().to_string(),
    ));
    Ok(())
}

/// Do two files have identical contents? Used to skip re-copying an unchanged
/// binary. Cheap guard on length first.
fn same_contents(a: &std::path::Path, b: &std::path::Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if ma.len() != mb.len() {
        return false;
    }
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Install an already-extracted companion executable beside the main binary.
///
/// The main binary is the process invoking `rocker install`, so it keeps its
/// platform-specific placement logic. The extension host is supplied by the
/// release installer and uses this shared atomic copy path on every platform.
pub(crate) fn place_companion(
    source: &std::path::Path,
    destination: &std::path::Path,
    report: &mut Report,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        source.is_file(),
        "extension host archive did not contain a regular executable: {}",
        source.display()
    );
    if same_contents(source, destination) {
        report.push(Change::new(
            ChangeVerb::Skipped,
            destination.display().to_string(),
        ));
        return Ok(());
    }
    let existed = destination.exists();
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("create {}: {e}", parent.display()))?;
    }
    let temporary = destination.with_file_name(format!(".{EXT_HOST_BIN_NAME}.new"));
    std::fs::copy(source, &temporary).map_err(|e| {
        anyhow::anyhow!(
            "copy extension host from {} to {}: {e}",
            source.display(),
            destination.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(windows)]
    {
        let old = destination.with_file_name(format!(".{EXT_HOST_BIN_NAME}.old"));
        let _ = std::fs::remove_file(&old);
        if destination.exists() {
            std::fs::rename(destination, &old).map_err(|e| {
                anyhow::anyhow!("replace extension host at {}: {e}", destination.display())
            })?;
        }
    }
    std::fs::rename(&temporary, destination).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        anyhow::anyhow!("place extension host at {}: {e}", destination.display())
    })?;
    #[cfg(windows)]
    {
        let old = destination.with_file_name(format!(".{EXT_HOST_BIN_NAME}.old"));
        let _ = std::fs::remove_file(old);
    }
    report.push(Change::new(
        if existed {
            ChangeVerb::Updated
        } else {
            ChangeVerb::Created
        },
        destination.display().to_string(),
    ));
    Ok(())
}

/// Remove `path` if present (file, symlink, or directory) and record it.
fn remove_path(report: &mut Report, path: &std::path::Path) {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return,
    };
    let res = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match res {
        Ok(()) => report.push(Change::new(ChangeVerb::Removed, path.display().to_string())),
        Err(e) => report.push(
            Change::new(ChangeVerb::Warning, path.display().to_string())
                .with_note(format!("couldn't remove: {e}")),
        ),
    }
}
