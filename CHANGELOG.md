# Changelog

All notable changes to Rocker will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
