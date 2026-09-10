//! macOS integration: assemble a real `Rocker.app` bundle so the app shows in
//! Launchpad, Spotlight, and the Dock with its icon, and register the
//! `docker://` URL scheme. A CLI symlink keeps `rocker` on `PATH` too.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::{
    assets, home, remove_path, run_hook, write_file, Change, ChangeVerb, Diagnosis, InstallOptions,
    Report, Scope, UninstallOptions, APP_ID, APP_NAME, BIN_NAME, VERSION,
};

const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/\
LaunchServices.framework/Support/lsregister";

struct Layout {
    app: PathBuf,
    cli_symlink: PathBuf,
}

impl Layout {
    fn resolve(scope: Scope, bin_override: Option<&Path>) -> anyhow::Result<Self> {
        let apps = match scope {
            Scope::System => PathBuf::from("/Applications"),
            Scope::User => home()?.join("Applications"),
        };
        let cli = match bin_override {
            Some(dir) => dir.join(BIN_NAME),
            None => home()?.join(".local/bin").join(BIN_NAME),
        };
        Ok(Self {
            app: apps.join(format!("{APP_NAME}.app")),
            cli_symlink: cli,
        })
    }

    fn macos_dir(&self) -> PathBuf {
        self.app.join("Contents/MacOS")
    }
    fn binary(&self) -> PathBuf {
        self.macos_dir().join(BIN_NAME)
    }
    fn info_plist(&self) -> PathBuf {
        self.app.join("Contents/Info.plist")
    }
    fn icon(&self) -> PathBuf {
        self.app.join("Contents/Resources/rocker.icns")
    }
    fn pkginfo(&self) -> PathBuf {
        self.app.join("Contents/PkgInfo")
    }
}

pub(crate) fn install(opts: &InstallOptions, report: &mut Report) -> anyhow::Result<()> {
    let layout = Layout::resolve(opts.scope, opts.bin_dir.as_deref())?;

    if !opts.refresh_only {
        place_binary(&layout, report)?;
    }
    write_file(report, &layout.info_plist(), info_plist().as_bytes())?;
    write_file(report, &layout.pkginfo(), b"APPL????")?;
    write_file(report, &layout.icon(), &assets::icns())?;

    // CLI convenience symlink -> the binary inside the bundle.
    link_cli(&layout, report);

    run_hook(report, "touch", &[&layout.app.to_string_lossy()]);
    if Path::new(LSREGISTER).exists() {
        run_hook(report, LSREGISTER, &["-f", &layout.app.to_string_lossy()]);
    }
    Ok(())
}

fn place_binary(layout: &Layout, report: &mut Report) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let src = std::env::current_exe().context("resolve the running executable")?;
    let dest = layout.binary();
    let already_there = dest.exists()
        && (std::fs::canonicalize(&src).ok() == std::fs::canonicalize(&dest).ok()
            || crate::same_contents(&src, &dest));
    if already_there {
        report.push(
            Change::new(ChangeVerb::Skipped, dest.display().to_string())
                .with_note("binary already up to date"),
        );
        return Ok(());
    }
    std::fs::create_dir_all(layout.macos_dir())?;
    let tmp = dest.with_file_name(format!(".{BIN_NAME}.new"));
    std::fs::copy(&src, &tmp).with_context(|| format!("copy binary to {}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::anyhow!("place binary at {}: {e}", dest.display())
    })?;
    report.push(Change::new(ChangeVerb::Created, dest.display().to_string()));
    Ok(())
}

fn link_cli(layout: &Layout, report: &mut Report) {
    let link = &layout.cli_symlink;
    let target = layout.binary();
    if let Some(parent) = link.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::read_link(link).ok().as_deref() == Some(target.as_path()) {
        report.push(Change::new(ChangeVerb::Skipped, link.display().to_string()));
        return;
    }
    let _ = std::fs::remove_file(link);
    match std::os::unix::fs::symlink(&target, link) {
        Ok(()) => report.push(Change::new(ChangeVerb::Created, link.display().to_string())),
        Err(e) => report.push(
            Change::new(ChangeVerb::Warning, link.display().to_string())
                .with_note(format!("couldn't symlink: {e}")),
        ),
    }
}

fn info_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>              <string>{APP_NAME}</string>
  <key>CFBundleDisplayName</key>       <string>{APP_NAME}</string>
  <key>CFBundleIdentifier</key>        <string>{APP_ID}</string>
  <key>CFBundleVersion</key>           <string>{VERSION}</string>
  <key>CFBundleShortVersionString</key><string>{VERSION}</string>
  <key>CFBundleExecutable</key>        <string>{BIN_NAME}</string>
  <key>CFBundleIconFile</key>          <string>rocker.icns</string>
  <key>CFBundlePackageType</key>       <string>APPL</string>
  <key>CFBundleSignature</key>         <string>????</string>
  <key>LSMinimumSystemVersion</key>    <string>11.0</string>
  <key>NSHighResolutionCapable</key>   <true/>
  <key>LSApplicationCategoryType</key> <string>public.app-category.developer-tools</string>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key>    <string>{APP_ID}</string>
      <key>CFBundleURLSchemes</key> <array><string>docker</string></array>
    </dict>
  </array>
</dict>
</plist>
"#
    )
}

pub(crate) fn uninstall(opts: &UninstallOptions, report: &mut Report) -> anyhow::Result<()> {
    let layout = Layout::resolve(opts.scope, None)?;
    remove_path(report, &layout.app);
    if std::fs::read_link(&layout.cli_symlink).is_ok() {
        remove_path(report, &layout.cli_symlink);
    }
    if Path::new(LSREGISTER).exists() {
        run_hook(report, LSREGISTER, &["-u", &layout.app.to_string_lossy()]);
    }
    if opts.purge {
        if let Ok(h) = home() {
            remove_path(report, &h.join("Library/Application Support/rocker"));
        }
    }
    Ok(())
}

pub(crate) fn doctor() -> anyhow::Result<Diagnosis> {
    let layout = Layout::resolve(Scope::User, None)?;
    let artifacts = vec![
        ("app bundle".into(), layout.app.clone(), layout.app.is_dir()),
        (
            "bundle binary".into(),
            layout.binary(),
            layout.binary().exists(),
        ),
        (
            "Info.plist".into(),
            layout.info_plist(),
            layout.info_plist().exists(),
        ),
        (
            "CLI symlink".into(),
            layout.cli_symlink.clone(),
            layout.cli_symlink.exists(),
        ),
    ];
    let mut notes = Vec::new();
    notes.push(format!(
        "Launch Services register tool: {}",
        if Path::new(LSREGISTER).exists() {
            "present"
        } else {
            "missing"
        }
    ));

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
