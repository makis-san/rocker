# AGENTS.md - Rocker

## Project Overview

Rocker is a native desktop client for Docker containers, built in pure Rust with `egui`/`eframe`. No webview, no bundled Chromium, no Electron. It communicates with the Docker Engine API directly over Unix socket, TLS-secured TCP, or SSH.

## Architecture

Cargo workspace with 11 crates. The core split is UI-agnostic libraries vs. the `egui` frontend:

| Crate             | Role                                                        |
| ----------------- | ---------------------------------------------------------- |
| `rocker-core`     | Domain models and pure logic. No I/O, no UI.               |
| `rocker-engine`   | `bollard` client, command/event protocol, background task. |
| `rocker-store`    | TOML config plus `redb` history.                           |
| `rocker-secrets`  | OS keychain seam.                                           |
| `rocker-term`     | VT parser plus Docker-exec backend.                        |
| `rocker-theme`    | Design-token schema plus theme loading.                    |
| `rocker-ext-api`  | Extension contract: capabilities, manifest, declarative UI.|
| `rocker-ext-host` | Supervised extension host plus capability gate.            |
| `rocker-ui`       | `egui` widgets and the `eframe::App`.                      |
| `rocker`          | Binary: runtime, logging, CLI wiring.                      |
| `xtask`           | Build, package, and release automation plus budget checks. |

Communication: UI thread posts `Command`s over mpsc, engine posts `Event`s back. The UI thread never `.await`s.

## Build & Test Commands

```sh
cargo run -p rocker               # launch the app
cargo test --workspace            # run all tests
cargo clippy --workspace          # lint
cargo fmt --all --check           # check formatting
cargo xtask budgets               # check performance budgets
```

## Code Style

- Follow standard Rust conventions (clippy + rustfmt)
- No `unwrap()` in production paths; use `thiserror` for error types
- Keep UI-agnostic core free of `egui` dependencies
- Document public APIs with `///` doc comments
- MSRV: Rust 1.90

## Commit Convention

Follow Conventional Commits: `feat(engine): add SSH transport support`, `fix(term): handle escape sequences`, etc.

## Workspace Structure

- `crates/` - all library and binary crates
- `xtask/` - build, package, release automation
- `flatpak/` - Flatpak manifest
- `packaging/` - installer scripts and assets
- `wit/` - WASM interface types for extensions
