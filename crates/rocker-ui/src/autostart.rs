//! "Open at login" — a thin wrapper over `auto-launch`.
//!
//! Best-effort by design: registering a startup entry can fail (read-only
//! home, a sandbox, an unsupported desktop) and none of that is worth an
//! error banner. We log, keep the user's stored intent, and retry on the next
//! toggle.

use auto_launch::AutoLaunchBuilder;

/// Name of the generated entry — the `.desktop` stem on Linux, the login-item
/// label on macOS, the registry value name on Windows.
const APP_NAME: &str = "Rocker";

fn entry() -> Option<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe()
        .inspect_err(|e| tracing::warn!(error = %e, "can't resolve own path for autostart"))
        .ok()?;
    let path = exe.to_str()?;

    let mut builder = AutoLaunchBuilder::new();
    builder.set_app_name(APP_NAME).set_app_path(path);
    // On macOS a LaunchAgent plist is the well-behaved, un-sandboxed choice.
    #[cfg(target_os = "macos")]
    builder.set_use_launch_agent(true);

    builder
        .build()
        .inspect_err(|e| tracing::warn!(error = %e, "couldn't build autostart entry"))
        .ok()
}

/// Bring the on-disk autostart entry in line with `wanted`.
pub fn sync(wanted: bool) {
    if wanted && is_flatpak() {
        tracing::warn!(
            "\"open at login\" writes ~/.config/autostart, which the host can't \
             see from inside the Flatpak sandbox. Add Rocker from your desktop's \
             startup-applications settings instead."
        );
    }

    let Some(entry) = entry() else { return };
    if entry.is_enabled().unwrap_or(false) == wanted {
        return;
    }
    let result = if wanted {
        entry.enable()
    } else {
        entry.disable()
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, wanted, "couldn't update autostart entry");
    }
}

fn is_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || std::path::Path::new("/.flatpak-info").exists()
}
