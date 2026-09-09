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
flatpak-builder --force-clean build-dir io.github.makis_san.Rocker.yml
```

### 3. Test Locally

```bash
flatpak-builder --user --install --force-clean build-dir io.github.makis_san.Rocker.yml
flatpak run io.github.makis_san.Rocker
```

## Submitting to Flathub

Flathub builds manifests from a *separate* per-app repository, not from a
subfolder of this one, so the initial submission PR must already carry the
final manifest (`type: git`, pinned to a real tag — not `type: dir`, which is
for local testing only):

1. Fork [flathub/flathub](https://github.com/flathub/flathub) with "Copy the
   master branch only" unchecked, then clone your fork on the `new-pr` branch:
   `git clone --branch=new-pr git@github.com:<you>/flathub.git`
2. Create a feature branch off `new-pr`, add a new `io.github.makis_san.Rocker/`
   directory containing `io.github.makis_san.Rocker.yml` (pinned to a tag of
   this repo) and `cargo-sources.json`, and commit
3. Open a PR **against the `new-pr` base branch** (not `master`), titled
   `Add io.github.makis_san.Rocker` (see Flathub's
   [submission guide](https://docs.flathub.org/docs/for-app-authors/submission))
4. Reviewers may request changes; comment `bot, build` once addressed to
   trigger a test build
5. Once approved, Flathub merges into a new repo under the Flathub org and
   invites you with write access (accept within a week, 2FA required); that
   repo becomes the permanent source of truth for the published manifest —
   this directory stays for local testing and preparing manifest changes

Once published, end users install with:

```sh
flatpak install flathub io.github.makis_san.Rocker
```

## Files

- `io.github.makis_san.Rocker.yml` - Flatpak manifest
- `generate-sources.sh` - Script to generate cargo sources
- `cargo-sources.json` - Generated cargo dependencies (not in repo)

The desktop entry and AppStream metadata are shared with the `.deb`/`.rpm`
packages and live in [`../packaging/linux/`](../packaging/linux/).

## Notes

- The app requires access to the Docker socket (`/var/run/docker.sock`)
- Network access is needed to connect to Docker daemon
- The manifest uses the freedesktop runtime 24.08 with the Rust SDK extension
