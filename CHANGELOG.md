# Changelog

All notable changes to Rocker will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.3] - 2026-09-09

### Changed

- AppStream metainfo renamed from `io.github.makis_san.Rocker.appdata.xml` to
  `io.github.makis_san.Rocker.metainfo.xml` (the current AppStream convention;
  clears a `flatpak-builder-lint` warning) and updated across the `.deb`,
  `.rpm`, and Flatpak packaging.
- Added a store screenshot (`packaging/linux/screenshots/screenshot1.png`) and
  referenced it from the metainfo.

## [0.1.2] - 2026-09-09

### Added

- **System tray**: Rocker minimizes to a tray icon instead of quitting, and the
  tray menu shows a live Docker summary — combined CPU and memory across running
  containers, Docker's on-disk usage (`/system/df`), and the running/stopped
  container split. "Show Rocker" and "Quit Rocker" round it out. Opt-in via the
  `tray` Cargo feature (`cargo build --features tray`), which pulls GTK 3 +
  libappindicator dev libraries on Linux; release packages enable it.
- **Open at login**: a Settings toggle registers Rocker to start automatically
  when you sign in (autostart `.desktop` on Linux, LaunchAgent on macOS, Run key
  on Windows). Does not yet take effect inside the Flatpak sandbox.
- Settings > System: "Minimize to tray", "Start hidden", and "Open at login".

## [0.1.1] - 2026-09-09

### Changed

- App-id renamed from `com.makis-san.Rocker` to `io.github.makis_san.Rocker`
  across the `.desktop` file, AppStream metainfo, and `.deb`/`.rpm`/Flatpak
  packaging — required for Flathub submission, which only accepts app-ids
  whose domain the submitter can verify (GitHub-hosted projects without a
  custom domain use the `io.github.<user>` form)
- Icon installed by `.deb`/`.rpm`/Flatpak into `hicolor/512x512/apps/` is now
  an actual 512×512 PNG (`assets/icon-512.png`) instead of the 1024×1024
  source scaled by the desktop environment at runtime; `flatpak-builder`
  validates icon dimensions against their directory and rejects the mismatch

### Added

- Flatpak manifest (`flatpak/io.github.makis_san.Rocker.yml`) now builds from
  a pinned git tag with vendored cargo sources instead of a local directory,
  installs license files, and bumped the runtime to freedesktop 25.08 (24.08's
  `rust-stable` SDK extension ships rustc 1.89, below this workspace's
  `rust-version = "1.90"`); verified with a full offline `flatpak-builder`
  build, appstream validation, and local install
- AppStream metainfo now lists real release history (0.0.1, 0.1.0) instead of
  a placeholder `0.0.0` entry

### Fixed

- Flatpak manifest's `--session-bus` finish-arg was invalid syntax (the
  correct form doesn't take a bare flag) and, moreover, unused: the
  secret-service credential store it was added for (`rocker-secrets`) is
  currently an in-memory stub with no real keyring backend yet. Removed
  rather than fixed, since Flathub review flags unused permissions; revisit
  with a scoped `--talk-name=org.freedesktop.secrets` once that backend lands

## [0.1.0] - 2026-09-09

### Added

- `.deb` and `.rpm` packaging (`.github/workflows/linux-packages.yml`), with a
  shared desktop entry, AppStream metadata, and icon (`packaging/linux/`)
- Homebrew tap publishing (`makis-san/homebrew-tap`) on every stable release
- macOS targets (`x86_64-apple-darwin`, `aarch64-apple-darwin`) back in the
  release build
- Bundled app icon shown in the window title bar, taskbar/dock, and Alt-Tab
  switcher
- `RELEASING.md` documenting the release process

### Changed

- Replaced `assets/rocker-logo.png` with a proper 1024px app icon
  (`assets/icon-1024.png`), shared across the binary, `.deb`/`.rpm`, and
  Flatpak builds
- Flatpak manifest now builds only the `rocker` binary and drops unused
  `--system-bus` / `DISPLAY` permissions

## [0.0.1] - 2026-09-09

### Added

- Initial workspace scaffold with 11 crates
- Docker daemon connection via `bollard` (unix socket, TCP, SSH)
- Container listing with live status
- Container lifecycle actions (start / stop / restart)
- Streaming logs viewer
- Stats stream with CPU and memory display
- VT/ANSI terminal emulator (`rocker-term`)
- Theme system with light/dark built-ins
- Extension API contract (WIT world definition)
- CI pipeline (fmt, clippy, tests, performance budgets) on Ubuntu, macOS, Windows
- Hand-drawn icon set (19 geometric icons)
