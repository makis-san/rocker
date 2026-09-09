# Flathub Packaging

This directory contains the files needed to package Rocker for Flathub.

## Prerequisites

- `flatpak` and `flatpak-builder` installed
- Freedesktop SDK extension for Rust: `flatpak install flathub org.freedesktop.Sdk.Extension.rust-stable`
- `flatpak-cargo-generator` or `cargo2flatpak` for generating cargo sources

## Building

### 1. Generate Cargo Sources

```bash
# Using flatpak-cargo-generator
./generate-sources.sh

# Or using cargo2flatpak
pip3 install cargo2flatpak
cargo2flatpak --manifest-path=../Cargo.lock --output=cargo-sources.json
```

### 2. Build the App

```bash
flatpak-builder --force-clean build-dir com.makis-san.Rocker.yml
```

### 3. Test Locally

```bash
flatpak-builder --user --install --force-clean build-dir com.makis-san.Rocker.yml
flatpak run com.makis-san.Rocker
```

## Submitting to Flathub

1. Fork the [flathub repository](https://github.com/flathub/flathub)
2. Create a new directory: `com.makis-san.Rocker`
3. Copy these files into the directory
4. Generate and include `cargo-sources.json`
5. Create a Pull Request

## Files

- `com.makis-san.Rocker.yml` - Flatpak manifest
- `com.makis-san.Rocker.appdata.xml` - AppStream metadata
- `com.makis-san.Rocker.desktop` - Desktop entry file
- `generate-sources.sh` - Script to generate cargo sources
- `cargo-sources.json` - Generated cargo dependencies (not in repo)

## Notes

- The app requires access to the Docker socket (`/var/run/docker.sock`)
- Network access is needed to connect to Docker daemon
- The manifest uses the freedesktop runtime 24.08 with the Rust SDK extension
