<p align="center">
  <img src="assets/rocker-logo.png" alt="Rocker" width="128" />
</p>

<h1 align="center">Rocker</h1>

<p align="center">
  A lightweight, native-first desktop app to manage Docker containers. Pure Rust on
  <code>egui</code>/<code>eframe</code> — no webview, no bundled Chromium.
</p>

<p align="center">
  <a href="https://github.com/makis-san/rocker/actions/workflows/ci.yml"><img src="https://github.com/makis-san/rocker/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://crates.io/crates/rocker"><img src="https://img.shields.io/crates/v/rocker.svg" alt="crates.io" /></a>
  <a href="https://docs.rs/rocker"><img src="https://img.shields.io/docsrs/rocker" alt="docs.rs" /></a>
  <a href="https://github.com/makis-san/rocker/blob/main/LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg" alt="License" /></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-1.90+-orange.svg" alt="Rust" /></a>
</p>

## Features

- Direct Docker Engine API communication (no CLI wrapper)
- Container lifecycle management (start / stop / restart / pause / kill)
- Streaming logs with follow, tail, regex filter, and ANSI color
- Live CPU, memory, network, and disk I/O stats
- Terminal exec via Docker's hijacked stream
- Container groups (smart / manual / rule-based)
- Custom themes (light / dark / system)
- Extension platform (rhai scripting + WASM components)
- Cross-platform: Linux, macOS, Windows

## Getting Started

### Prerequisites

- **Rust** 1.90+ ([rustup](https://rustup.rs/))
- **Docker** running locally (rootless Docker recommended)

### Install

```sh
git clone https://github.com/makis-san/rocker.git
cd rocker
cargo run -p rocker
```

### Build a Release

```sh
cargo build --release -p rocker
```

The binary will be at `target/release/rocker`.

## Development

```sh
cargo run -p rocker          # launch the app
cargo test --workspace        # run all tests
cargo clippy --workspace      # lint
cargo fmt --all --check       # check formatting
cargo xtask budgets           # check performance budgets
```

### Workspace

| Crate             | Role                                                        |
| ----------------- | ----------------------------------------------------------- |
| `rocker-core`     | Domain models and pure logic. No I/O, no UI.                |
| `rocker-engine`   | `bollard` client, command/event protocol, background task.  |
| `rocker-store`    | TOML config + `redb` history.                               |
| `rocker-secrets`  | OS keychain seam.                                           |
| `rocker-term`     | VT parser + Docker-exec backend.                            |
| `rocker-theme`    | Design-token schema + theme loading.                        |
| `rocker-ext-api`  | Extension contract: capabilities, manifest, declarative UI. |
| `rocker-ext-host` | Supervised extension host + capability gate.                |
| `rocker-ui`       | `egui` widgets and the `eframe::App`.                       |
| `rocker`          | Binary: runtime, logging, CLI wiring.                       |
| `xtask`           | Build / package / release automation + budget checks.       |

`rocker-core` + `rocker-engine` + `rocker-store` are the UI-agnostic core,
reusable by a future menubar or TUI front-end.

## Architecture

```
+-----------------------------------------------------------+
|  rocker-ui (egui/eframe)                                 |
|  container list | logs | terminal | plots | ext panels    |
+---------------------------+-------------------------------+
                            | Command mpsc  /  Event mpsc
+---------------------------v-------------------------------+
|  Core crates (Rust, UI-agnostic)                          |
|  Connection manager | Docker service | Event bus          |
|  Group store | Credential manager | Stats collector       |
|  Extension host | Theme registry | Persistence            |
+---------------------------------------------------------+
```

## Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for
guidelines.

## Security

For reporting security vulnerabilities, please see [SECURITY.md](SECURITY.md).

## License

Licensed under either of:

- [MIT License](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.
# rocker
