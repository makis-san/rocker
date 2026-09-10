//! Windows integration for the script-install path: a Start-menu shortcut, an
//! "Apps & features" uninstall entry, and the binary on the user `PATH`. (The
//! `.msi` from `dist` does the same for double-click installs.)

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
use winreg::RegKey;

use crate::{
    assets, remove_path, write_file, Change, ChangeVerb, Diagnosis, InstallOptions, Report, Scope,
    UninstallOptions, APP_NAME, BIN_NAME, VERSION,
};

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Rocker";

struct Layout {
    dir: PathBuf,
    start_menu_lnk: PathBuf,
}

impl Layout {
    fn resolve(scope: Scope, bin_override: Option<&Path>) -> anyhow::Result<Self> {
        let dir = match (bin_override, scope) {
            (Some(d), _) => d.to_path_buf(),
            (None, Scope::System) => PathBuf::from(env_var("PROGRAMFILES")?).join(APP_NAME),
            (None, Scope::User) => PathBuf::from(env_var("LOCALAPPDATA")?)
                .join("Programs")
                .join(APP_NAME),
        };
        let start_menu = PathBuf::from(env_var("APPDATA")?)
            .join(r"Microsoft\Windows\Start Menu\Programs")
            .join(format!("{APP_NAME}.lnk"));
        Ok(Self {
            dir,
            start_menu_lnk: start_menu,
        })
    }

    fn exe(&self) -> PathBuf {
        self.dir.join(format!("{BIN_NAME}.exe"))
    }
    fn icon(&self) -> PathBuf {
        self.dir.join("rocker.ico")
    }
}

fn env_var(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("%{name}% is not set"))
}

pub(crate) fn install(opts: &InstallOptions, report: &mut Report) -> anyhow::Result<()> {
    let layout = Layout::resolve(opts.scope, opts.bin_dir.as_deref())?;
    std::fs::create_dir_all(&layout.dir)
        .with_context(|| format!("create {}", layout.dir.display()))?;

    if !opts.refresh_only {
        place_binary(&layout, report)?;
    }
    write_file(report, &layout.icon(), &assets::ico())?;
    create_shortcut(&layout, report)?;
    write_uninstall_entry(&layout, report)?;
    if !opts.refresh_only {
        add_to_path(&layout.dir, opts.modify_path, report)?;
    }
    Ok(())
}

fn place_binary(layout: &Layout, report: &mut Report) -> anyhow::Result<()> {
    let src = std::env::current_exe().context("resolve the running executable")?;
    let dest = layout.exe();
    if src == dest && dest.exists() {
        report.push(
            Change::new(ChangeVerb::Skipped, dest.display().to_string())
                .with_note("already running from the install location"),
        );
        return Ok(());
    }
    let tmp = dest.with_extension("new");
    std::fs::copy(&src, &tmp).with_context(|| format!("copy binary to {}", tmp.display()))?;
    // A running target exe can't be overwritten; move it aside first.
    if dest.exists() {
        let _ = std::fs::rename(&dest, dest.with_extension("old"));
    }
    std::fs::rename(&tmp, &dest)
        .map_err(|e| anyhow::anyhow!("place binary at {}: {e}", dest.display()))?;
    let _ = std::fs::remove_file(dest.with_extension("old"));
    report.push(Change::new(ChangeVerb::Created, dest.display().to_string()));
    Ok(())
}

fn create_shortcut(layout: &Layout, report: &mut Report) -> anyhow::Result<()> {
    let mut link = mslnk::ShellLink::new(layout.exe())
        .with_context(|| format!("build shortcut for {}", layout.exe().display()))?;
    link.set_name(Some(APP_NAME.to_string()));
    link.set_icon_location(Some(layout.icon().to_string_lossy().into_owned()));
    link.set_working_dir(Some(layout.dir.to_string_lossy().into_owned()));
    if let Some(parent) = layout.start_menu_lnk.parent() {
        std::fs::create_dir_all(parent)?;
    }
    link.create_lnk(&layout.start_menu_lnk)
        .with_context(|| format!("write {}", layout.start_menu_lnk.display()))?;
    report.push(Change::new(
        ChangeVerb::Created,
        layout.start_menu_lnk.display().to_string(),
    ));
    Ok(())
}

fn write_uninstall_entry(layout: &Layout, report: &mut Report) -> anyhow::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(UNINSTALL_KEY)
        .context("open Uninstall key")?;
    let exe = layout.exe();
    key.set_value("DisplayName", &APP_NAME)?;
    key.set_value("DisplayVersion", &VERSION)?;
    key.set_value("Publisher", &"Rocker contributors")?;
    key.set_value("DisplayIcon", &exe.to_string_lossy().into_owned())?;
    key.set_value(
        "InstallLocation",
        &layout.dir.to_string_lossy().into_owned(),
    )?;
    key.set_value(
        "UninstallString",
        &format!("\"{}\" uninstall", exe.to_string_lossy()),
    )?;
    key.set_value("NoModify", &1u32)?;
    key.set_value("NoRepair", &1u32)?;
    report.push(Change::new(
        ChangeVerb::Updated,
        format!("HKCU\\{UNINSTALL_KEY}"),
    ));
    Ok(())
}

fn add_to_path(dir: &Path, modify: bool, report: &mut Report) -> anyhow::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let env = hkcu
        .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
        .context("open HKCU\\Environment")?;
    let current: String = env.get_value("Path").unwrap_or_default();
    let dir_s = dir.to_string_lossy();
    let present = current
        .split(';')
        .any(|e| e.trim().eq_ignore_ascii_case(dir_s.trim()));
    if present {
        return Ok(());
    }
    if !modify {
        report.push(
            Change::new(
                ChangeVerb::Warning,
                format!("{} is not on PATH", dir.display()),
            )
            .with_note("re-run with --modify-path, or add it in System Settings"),
        );
        return Ok(());
    }
    let updated = if current.is_empty() {
        dir_s.into_owned()
    } else {
        format!("{dir_s};{current}")
    };
    env.set_value("Path", &updated)?;
    report.push(
        Change::new(ChangeVerb::Updated, "HKCU\\Environment\\Path")
            .with_note("sign out and back in, or restart your shell, to pick it up"),
    );
    Ok(())
}

pub(crate) fn uninstall(opts: &UninstallOptions, report: &mut Report) -> anyhow::Result<()> {
    let layout = Layout::resolve(opts.scope, None)?;
    remove_path(report, &layout.start_menu_lnk);
    remove_path(report, &layout.icon());
    // Leave the running exe; drop the rest of the dir on a best-effort basis.
    remove_path(report, &layout.dir);

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if hkcu.delete_subkey_all(UNINSTALL_KEY).is_ok() {
        report.push(Change::new(
            ChangeVerb::Removed,
            format!("HKCU\\{UNINSTALL_KEY}"),
        ));
    }
    if let Ok(env) = hkcu.open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE) {
        let current: String = env.get_value("Path").unwrap_or_default();
        let dir_s = layout.dir.to_string_lossy();
        let filtered: Vec<&str> = current
            .split(';')
            .filter(|e| !e.trim().eq_ignore_ascii_case(dir_s.trim()) && !e.is_empty())
            .collect();
        let joined = filtered.join(";");
        if joined != current {
            let _ = env.set_value("Path", &joined);
            report.push(Change::new(ChangeVerb::Updated, "HKCU\\Environment\\Path"));
        }
    }

    if opts.purge {
        if let Ok(appdata) = std::env::var("APPDATA") {
            remove_path(report, &PathBuf::from(appdata).join("rocker"));
        }
    }
    Ok(())
}

pub(crate) fn doctor() -> anyhow::Result<Diagnosis> {
    let layout = Layout::resolve(Scope::User, None)?;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let uninstall_present = hkcu.open_subkey(UNINSTALL_KEY).is_ok();

    let artifacts = vec![
        ("binary".into(), layout.exe(), layout.exe().exists()),
        (
            "Start-menu shortcut".into(),
            layout.start_menu_lnk.clone(),
            layout.start_menu_lnk.exists(),
        ),
        (
            "uninstall entry".into(),
            PathBuf::from(UNINSTALL_KEY),
            uninstall_present,
        ),
    ];
    let mut notes = Vec::new();
    if let Ok(env) = hkcu.open_subkey("Environment") {
        let current: String = env.get_value("Path").unwrap_or_default();
        let dir_s = layout.dir.to_string_lossy();
        let on_path = current
            .split(';')
            .any(|e| e.trim().eq_ignore_ascii_case(dir_s.trim()));
        notes.push(format!(
            "{} on PATH: {}",
            layout.dir.display(),
            if on_path { "yes" } else { "no" }
        ));
    }

    let latest_release = latest_check();
    Ok(Diagnosis {
        version: VERSION,
        running_exe: std::env::current_exe().ok(),
        target_triple: env!("ROCKER_TARGET"),
        artifacts,
        notes,
        latest_release,
    })
}

#[cfg(feature = "self-update")]
fn latest_check() -> Option<(String, bool)> {
    let latest = crate::update::latest_tag().ok()?;
    Some((latest.clone(), crate::update::is_newer(&latest, VERSION)))
}

#[cfg(not(feature = "self-update"))]
fn latest_check() -> Option<(String, bool)> {
    None
}
