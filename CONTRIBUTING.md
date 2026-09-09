# Contributing to Rocker

Thanks for your interest in contributing! This document provides guidelines and
information for contributors.

## Getting Started

1. **Fork and clone** the repository
2. **Install Rust** (MSRV 1.90) via [rustup](https://rustup.rs/)
3. **Start Docker** (the app connects to the local Docker daemon)
4. **Run** `cargo run -p rocker` to launch the app

## Development

```sh
cargo run -p rocker          # launch the app
cargo test --workspace        # run all tests
cargo clippy --workspace      # lint
cargo fmt --all --check       # check formatting
cargo xtask budgets           # check performance budgets
```

### Architecture

Rocker is a Cargo workspace with 11 crates. The key split is between the
UI-agnostic core and the `egui` frontend:

- **`rocker-core`** + **`rocker-engine`** + **`rocker-store`** are reusable
  library crates with no UI dependency. A future menubar app or TUI could use
  them directly.
- **`rocker-ui`** is the `egui`/`eframe` frontend.
- **`rocker`** is the binary entry point.

Communication between the UI thread and the async engine happens over `mpsc`
channels. The UI thread never `.await`s.

### Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(engine): add SSH transport support
fix(term): handle escape sequences with mixed params
docs(readme): add installation instructions
```

### Pull Requests

- Keep PRs focused on a single change
- Include a clear description of what changed and why
- Add tests for new functionality
- Ensure `cargo clippy --workspace` and `cargo test --workspace` pass
- Link related issues

### Testing

- **Core**: unit tests in each crate, integration tests against Docker where
  applicable
- **UI**: `egui_kittest` for widget snapshot tests
- **CI**: tests run on Ubuntu, macOS, and Windows

## Code Style

- Follow standard Rust conventions (`clippy` + `rustfmt`)
- No `unwrap()` in production paths; use `thiserror` for error types
- Keep the UI-agnostic core free of `egui` dependencies
- Document public APIs with `///` doc comments

## Reporting Issues

- Use the [GitHub issue tracker](https://github.com/makis-san/rocker/issues)
- Include your OS, Rust version, and Docker version
- For crashes, include the backtrace if possible

## License

By contributing, you agree that your contributions will be licensed under the
same dual license as the project: [MIT](LICENSE-MIT) OR
[Apache-2.0](LICENSE-APACHE).
