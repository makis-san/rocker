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

Flathub builds manifests from a *separate* per-app repository, not from a
subfolder of this one, so:

1. Fork the [flathub repository](https://github.com/flathub/flathub) and open a
   PR that adds a new `com.makis-san.Rocker` entry pointing at this repo (see
   Flathub's [app submission guide](https://docs.flathub.org/docs/for-app-authors/submission))
2. Once accepted, Flathub gives you a `flathub/com.makis-san.Rocker` repo — copy
   `com.makis-san.Rocker.yml` there (updated to build from a pinned git tag
   instead of `type: dir`) plus a generated `cargo-sources.json`
3. Flathub's own CI builds and publishes it from there; this directory stays the
   source of truth for local testing and manifest changes

Once published, end users install with:

```sh
flatpak install flathub com.makis-san.Rocker
```

## Files

- `com.makis-san.Rocker.yml` - Flatpak manifest
- `generate-sources.sh` - Script to generate cargo sources
- `cargo-sources.json` - Generated cargo dependencies (not in repo)

The desktop entry and AppStream metadata are shared with the `.deb`/`.rpm`
packages and live in [`../packaging/linux/`](../packaging/linux/).

## Notes

- The app requires access to the Docker socket (`/var/run/docker.sock`)
- Network access is needed to connect to Docker daemon
- The manifest uses the freedesktop runtime 24.08 with the Rust SDK extension
