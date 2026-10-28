# Dependency security review — 2026-10-27

Audited the full Cargo.lock against the RustSec advisory database, including
dependencies for optional features and other platforms. This detects known
affected versions; it does not establish whether each vulnerable function was
reachable in Rocker.

The original lockfile had 29 vulnerability findings. After targeted updates,
cargo-audit 0.22.2 reports zero vulnerability findings, with the warnings below.

| Dependency | Before | After |
| --- | --- | --- |
| Wasmtime and its compiler/runtime family | 41.0.4 | 36.0.19 |
| zip | 8.6.0 | 9.1.3 |
| rustls | 0.23.44 | 0.23.46 |
| rustls-webpki | 0.103.15 | 0.103.16 |
| webpki-roots | 1.0.9 | 1.0.11 |
| tokio | 1.53.1 | 1.54.1 |
| clipboard-win (yanked version) | 5.4.1 | 5.4.3 |

Wasmtime deliberately moves to the patched 36.x LTS line. Patched 48.0.7
requires Rust 1.95, whereas 36.0.19 supports Rust 1.86 and preserves Rocker's
declared Rust 1.90 minimum. This is a runtime compatibility change, so the
component and extension-host integration tests must pass before release.

Relevant upstream advisories include
[Wasmtime compiler issue](https://rustsec.org/advisories/RUSTSEC-2026-0379.html),
[ZIP extraction](https://rustsec.org/advisories/RUSTSEC-2026-0369.html),
[Rustls handshake validation](https://rustsec.org/advisories/RUSTSEC-2026-0285.html),
and [certificate distrust](https://rustsec.org/advisories/RUSTSEC-2026-0382.html).

## Remaining warnings

- **GLib 0.18.5, RUSTSEC-2024-0429 (unsound):** pulled in by GTK 3 and the
  optional Linux system tray. Fixed upstream in GLib >=0.20, but GTK 3's Rust
  bindings require the 0.18 family. This needs a tray/backend migration or an
  upstream backport; adding a second GLib version does not fix GTK's copy.
  Release builds enable the tray, so this remains relevant to shipped builds.
- **Unmaintained crates:** paste, proc-macro-error, smartstring, ttf-parser,
  zbus_names, zvariant, and zvariant_derive. Smartstring comes through Rhai;
  the zbus types come through the accessibility stack; proc-macro-error comes
  through GTK macros. These require upstream migrations rather than a blanket
  lockfile refresh.

No advisories are suppressed. The scheduled security workflow checks current
advisories daily and fails for vulnerability findings. Informational warnings
remain visible in its output; they do not fail the default cargo-audit policy.
Dependabot checks Cargo dependencies and GitHub Actions weekly.

## Other updates

`cargo update --dry-run` also found compatible updates outside this security
patch, including Rhai 1.27.0, redb 4.4.0, ureq 3.5.0, and serde_json 1.0.154.
These remain available for separate review; they were not required to clear
the vulnerability findings. The dry run did not modify the lockfile.

## Validation

- `cargo audit`: passes, zero vulnerability findings, seven unmaintained
  warnings and one GLib unsoundness warning.
- `cargo test --workspace --locked`: 135 tests passed, including extension-host
  process tests and the archive traversal/symlink rejection tests.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`:
  passes.
- `cargo fmt --all --check` and `git diff --check`: pass.
- `cargo xtask budgets`: exits successfully, but no distribution archive is
  available to measure and its runtime checks remain placeholders.

Validation ran on Linux with Rust 1.99.1; Windows, macOS, and Rust 1.90 execution
were not performed locally. Dependency MSRV declarations informed the Wasmtime
selection, rather than an actual minimum-toolchain build.
