//! Linux desktop integration via the freedesktop base-dir + hicolor icon +
//! desktop-entry contract. Every mainstream desktop (GNOME, KDE, XFCE, LXQt,
//! Cinnamon, MATE, wlroots launchers) reads these locations, so one code path
//! covers every distro with no package manager involved.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::{
    assets, companion_source, home, is_flatpak, place_companion, remove_path, run_hook, write_file,
    Change, ChangeVerb, Diagnosis, InstallOptions, Report, Scope, UninstallOptions, APP_ID,
    BIN_NAME, EXT_HOST_BIN_NAME, VERSION,
};

struct Layout {
    bin: PathBuf,
    applications: PathBuf,
    hicolor: PathBuf,
    pixmaps: PathBuf,
    metainfo: PathBuf,
}

impl Layout {
    fn resolve(opts_scope: Scope, bin_override: Option<&Path>) -> anyhow::Result<Self> {
        let (bin_dir, data_dir) = match opts_scope {
            Scope::System => (
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/usr/local/share"),
            ),
            Scope::User => (xdg_bin_home()?, xdg_data_home()?),
        };
        let bin_dir = bin_override.map(Path::to_path_buf).unwrap_or(bin_dir);
        Ok(Self {
            bin: bin_dir.join(BIN_NAME),
            applications: data_dir.join("applications"),
            hicolor: data_dir.join("icons/hicolor"),
            pixmaps: data_dir.join("pixmaps"),
            metainfo: data_dir.join("metainfo"),
        })
    }

    fn desktop_file(&self) -> PathBuf {
        self.applications.join(format!("{APP_ID}.desktop"))
    }
    fn metainfo_file(&self) -> PathBuf {
        self.metainfo.join(format!("{APP_ID}.metainfo.xml"))
    }
    fn icon_file(&self, size: u32) -> PathBuf {
        self.hicolor
            .join(format!("{size}x{size}/apps/{APP_ID}.png"))
    }
    fn pixmap_file(&self) -> PathBuf {
        self.pixmaps.join(format!("{APP_ID}.png"))
    }
    fn ext_host(&self) -> PathBuf {
        self.bin
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(EXT_HOST_BIN_NAME)
    }
}

fn xdg_data_home() -> anyhow::Result<PathBuf> {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(v) if !v.is_empty() => Ok(PathBuf::from(v)),
        _ => Ok(home()?.join(".local/share")),
    }
}

fn xdg_bin_home() -> anyhow::Result<PathBuf> {
    match std::env::var_os("XDG_BIN_HOME") {
        Some(v) if !v.is_empty() => Ok(PathBuf::from(v)),
        _ => Ok(home()?.join(".local/bin")),
    }
}

pub(crate) fn install(opts: &InstallOptions, report: &mut Report) -> anyhow::Result<()> {
    let layout = Layout::resolve(opts.scope, opts.bin_dir.as_deref())?;

    let ext_host_source = companion_source()?;

    if is_flatpak() {
        report.push(
            Change::new(ChangeVerb::Warning, "flatpak sandbox").with_note(
                "writes to ~/.local aren't visible to the host; run this outside the sandbox",
            ),
        );
    }

    let exec_target = if opts.refresh_only {
        layout.bin.clone()
    } else {
        place_binary(&layout.bin, report)?
    };
    place_companion(&ext_host_source, &layout.ext_host(), report)?;

    // Desktop entry, with every Exec= made absolute and a TryExec= guard added.
    let desktop = rewrite_desktop_entry(assets::DESKTOP_ENTRY, &exec_target);
    write_file(report, &layout.desktop_file(), desktop.as_bytes())?;

    // Icons: the full hicolor set, plus a legacy pixmap fallback.
    for (size, bytes) in assets::ICON_PNGS {
        write_file(report, &layout.icon_file(*size), bytes)?;
    }
    write_file(report, &layout.pixmap_file(), assets::png_512())?;

    // AppStream metadata (drives GNOME Software / Discover).
    write_file(
        report,
        &layout.metainfo_file(),
        assets::METAINFO_XML.as_bytes(),
    )?;

    refresh_caches(&layout, report);
    run_hook(
        report,
        "xdg-mime",
        &[
            "default",
            &format!("{APP_ID}.desktop"),
            "x-scheme-handler/docker",
        ],
    );

    if !opts.refresh_only {
        check_path(&layout.bin, opts.modify_path, report)?;
    }
    Ok(())
}

/// Copy the running executable to `dest` (0755), atomically. No-op when we're
/// already running from `dest`.
fn place_binary(dest: &Path, report: &mut Report) -> anyhow::Result<PathBuf> {
    let src = std::env::current_exe().context("resolve the running executable")?;
    let already_there = dest.exists()
        && (std::fs::canonicalize(&src).ok() == std::fs::canonicalize(dest).ok()
            || crate::same_contents(&src, dest));
    if already_there {
        report.push(
            Change::new(ChangeVerb::Skipped, dest.display().to_string())
                .with_note("binary already up to date"),
        );
        return Ok(dest.to_path_buf());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = dest.with_file_name(format!(".{BIN_NAME}.new"));
    std::fs::copy(&src, &tmp).with_context(|| format!("copy binary to {}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::anyhow!("place binary at {}: {e}", dest.display())
    })?;
    report.push(Change::new(ChangeVerb::Created, dest.display().to_string()));
    Ok(dest.to_path_buf())
}

/// Make every `Exec=` line in the entry point at `bin` (absolute), and add a
/// `TryExec=` to the main group so the launcher hides a stale entry.
fn rewrite_desktop_entry(src: &str, bin: &Path) -> String {
    let abs = shell_quote_desktop(&bin.to_string_lossy());
    let mut out = String::with_capacity(src.len() + abs.len());
    let mut in_main_group = false;
    let mut tryexec_done = false;
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_main_group = trimmed == "[Desktop Entry]";
        }
        if let Some(rest) = line.strip_prefix("Exec=") {
            let args = rest
                .split_once(char::is_whitespace)
                .map(|(_, a)| a)
                .unwrap_or("");
            if args.is_empty() {
                out.push_str(&format!("Exec={abs}\n"));
            } else {
                out.push_str(&format!("Exec={abs} {args}\n"));
            }
            if in_main_group && !tryexec_done {
                out.push_str(&format!("TryExec={abs}\n"));
                tryexec_done = true;
            }
            continue;
        }
        if line.starts_with("TryExec=") {
            continue; // replaced above
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Quote a path for a desktop-entry `Exec=` value only if it needs it.
fn shell_quote_desktop(path: &str) -> String {
    if path
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/._-+".contains(&b))
    {
        return path.to_string();
    }
    let escaped = path
        .replace('\\', r"\\")
        .replace('"', r#"\""#)
        .replace('$', r"\$");
    format!("\"{escaped}\"")
}

fn refresh_caches(layout: &Layout, report: &mut Report) {
    run_hook(
        report,
        "update-desktop-database",
        &[&layout.applications.to_string_lossy()],
    );
    run_hook(
        report,
        "gtk-update-icon-cache",
        &["-q", "-t", "-f", &layout.hicolor.to_string_lossy()],
    );
}

/// Warn (or fix, with `modify_path`) when the binary dir isn't on `PATH`.
fn check_path(bin: &Path, modify: bool, report: &mut Report) -> anyhow::Result<()> {
    let dir = bin.parent().unwrap_or(bin);
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|entry| entry == dir))
        .unwrap_or(false);
    if on_path {
        return Ok(());
    }
    let line = format!("export PATH=\"{}:$PATH\"", dir.display());
    if !modify {
        report.push(
            Change::new(
                ChangeVerb::Warning,
                format!("{} is not on PATH", dir.display()),
            )
            .with_note(format!("add to your shell rc:  {line}")),
        );
        return Ok(());
    }
    let rc = shell_rc_file()?;
    let mut body = std::fs::read_to_string(&rc).unwrap_or_default();
    if body.contains(&line) {
        report.push(Change::new(ChangeVerb::Skipped, rc.display().to_string()));
        return Ok(());
    }
    if !body.ends_with('\n') && !body.is_empty() {
        body.push('\n');
    }
    body.push_str(&format!("\n# added by `rocker install`\n{line}\n"));
    write_file(report, &rc, body.as_bytes())?;
    report.push(
        Change::new(ChangeVerb::Hook, "PATH")
            .with_note("restart your shell or `source` the rc file to pick it up"),
    );
    Ok(())
}

fn shell_rc_file() -> anyhow::Result<PathBuf> {
    let home = home()?;
    let shell = std::env::var("SHELL").unwrap_or_default();
    let name = if shell.ends_with("zsh") {
        ".zshrc"
    } else if shell.ends_with("bash") {
        ".bashrc"
    } else {
        ".profile"
    };
    Ok(home.join(name))
}

pub(crate) fn uninstall(opts: &UninstallOptions, report: &mut Report) -> anyhow::Result<()> {
    let layout = Layout::resolve(opts.scope, None)?;
    remove_path(report, &layout.bin);
    remove_path(report, &layout.ext_host());
    remove_path(report, &layout.desktop_file());
    remove_path(report, &layout.metainfo_file());
    for (size, _) in assets::ICON_PNGS {
        remove_path(report, &layout.icon_file(*size));
    }
    remove_path(report, &layout.pixmap_file());
    refresh_caches(&layout, report);

    if opts.purge {
        if let Ok(h) = home() {
            remove_path(report, &h.join(".config/rocker"));
            remove_path(report, &h.join(".local/share/rocker"));
        }
    }
    Ok(())
}

pub(crate) fn doctor() -> anyhow::Result<Diagnosis> {
    let layout = Layout::resolve(Scope::User, None)?;
    let running_exe = std::env::current_exe().ok();

    let artifacts = vec![
        ("binary".into(), layout.bin.clone(), layout.bin.exists()),
        (
            "extension host".into(),
            layout.ext_host(),
            layout.ext_host().exists(),
        ),
        (
            "desktop entry".into(),
            layout.desktop_file(),
            layout.desktop_file().exists(),
        ),
        (
            "icon (512)".into(),
            layout.icon_file(512),
            layout.icon_file(512).exists(),
        ),
        (
            "AppStream metadata".into(),
            layout.metainfo_file(),
            layout.metainfo_file().exists(),
        ),
    ];

    let mut notes = Vec::new();
    let bin_dir = layout
        .bin
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|e| e == bin_dir))
        .unwrap_or(false);
    notes.push(format!(
        "{} on PATH: {}",
        bin_dir.display(),
        if on_path { "yes" } else { "no" }
    ));
    for hook in [
        "update-desktop-database",
        "gtk-update-icon-cache",
        "xdg-mime",
    ] {
        let have = which(hook);
        notes.push(format!(
            "{hook}: {}",
            if have { "available" } else { "missing (ok)" }
        ));
    }
    if is_flatpak() {
        notes.push("running inside a Flatpak sandbox".into());
    }

    let latest_release = latest_check();

    Ok(Diagnosis {
        version: VERSION,
        running_exe,
        target_triple: env!("ROCKER_TARGET"),
        artifacts,
        notes,
        latest_release,
    })
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

#[cfg(feature = "self-update")]
fn latest_check() -> Option<(String, bool)> {
    let latest = crate::update::latest_tag().ok()?;
    let newer = crate::update::is_newer(&latest, VERSION);
    Some((latest, newer))
}

#[cfg(not(feature = "self-update"))]
fn latest_check() -> Option<(String, bool)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_exec_is_made_absolute_with_tryexec() {
        let out = rewrite_desktop_entry(
            assets::DESKTOP_ENTRY,
            Path::new("/home/x/.local/bin/rocker"),
        );
        assert!(out.contains("Exec=/home/x/.local/bin/rocker %F"));
        assert!(out.contains("TryExec=/home/x/.local/bin/rocker\n"));
        // The action group's Exec is rewritten too, but gets no TryExec.
        assert_eq!(out.matches("TryExec=").count(), 1);
        assert!(out.contains("[Desktop Action new-window]"));
        assert!(!out.contains("Exec=rocker"));
    }

    #[test]
    fn desktop_entry_survives_a_second_pass() {
        let once =
            rewrite_desktop_entry(assets::DESKTOP_ENTRY, Path::new("/opt/rocker/bin/rocker"));
        let twice = rewrite_desktop_entry(&once, Path::new("/opt/rocker/bin/rocker"));
        assert_eq!(once, twice);
    }

    #[test]
    fn quotes_only_paths_that_need_it() {
        assert_eq!(
            shell_quote_desktop("/home/x/.local/bin/rocker"),
            "/home/x/.local/bin/rocker"
        );
        assert_eq!(
            shell_quote_desktop("/opt/My Apps/rocker"),
            "\"/opt/My Apps/rocker\""
        );
    }
}
