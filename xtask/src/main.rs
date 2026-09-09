//! `cargo xtask <command>` — repo automation that shouldn't be a shell script.
//!
//! - `cross`: local cross-compile from Linux (zigbuild + xwin), for smoke builds
//! - `dist`: passthrough to `dist` (cargo-dist); the release pipeline lives in
//!   `.github/workflows/release.yml` and is driven by git tags
//! - `budgets`: CI check for the performance targets in PLAN §8
//!
//! Signed, shippable installers (.msi, notarized macOS, archives, checksums,
//! updater manifest) are produced by `dist` in CI on native runners. `cross` is
//! only for "does it still compile / run on my box" checks.

use std::process::{Command, ExitCode};

const HELP: &str = "\
cargo xtask <command>

commands:
  cross <linux|windows|macos|all>   cross-compile rocker locally from this host
  dist  [args...]                   run dist (cargo-dist); e.g. `cargo xtask dist plan`
  budgets                           check CI-enforced performance budgets (PLAN §8)
  help                              show this message

notes:
  * `cross linux`   needs cargo-zigbuild + zig
  * `cross windows` needs cargo-xwin (downloads the MSVC CRT/SDK, no Wine)
  * `cross macos`   needs cargo-zigbuild + a macOS SDK (SDKROOT); usually CI-only
  * releases: push a tag like `v0.1.0` and `.github/workflows/release.yml` does the rest
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();

    let result = match cmd.as_str() {
        "cross" => cross::exec(&rest),
        "dist" => dist::exec(&rest),
        "budgets" => budgets::check().map(|report| print!("{report}")),
        "help" | "--help" | "-h" | "" => {
            print!("{HELP}");
            Ok(())
        }
        other => Err(anyhow::anyhow!("unknown command: {other}\n\n{HELP}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Run a command, inheriting stdio, erroring on non-zero exit.
fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    eprintln!("$ {program} {}", args.join(" "));
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn `{program}`: {e}"))?;
    anyhow::ensure!(status.success(), "`{program}` exited with {status}");
    Ok(())
}

/// Is `program` runnable (used for preflight checks)?
fn have(program: &str, probe_args: &[&str]) -> bool {
    Command::new(program)
        .args(probe_args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

mod cross {
    use super::{have, run};

    const LINUX: &[&str] = &["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];
    const MACOS: &[&str] = &["x86_64-apple-darwin", "aarch64-apple-darwin"];
    const WINDOWS: &[&str] = &["x86_64-pc-windows-msvc"];

    pub fn exec(args: &[String]) -> anyhow::Result<()> {
        let which = args.first().map(String::as_str).unwrap_or("");
        let (do_linux, do_macos, do_windows) = match which {
            "linux" => (true, false, false),
            "macos" => (false, true, false),
            "windows" => (false, false, true),
            "all" => (true, true, true),
            "" => anyhow::bail!("usage: cargo xtask cross <linux|windows|macos|all>"),
            other => anyhow::bail!("unknown cross target: {other} (want linux|windows|macos|all)"),
        };

        if do_linux {
            zigbuild(LINUX)?;
        }
        if do_macos {
            if std::env::var_os("SDKROOT").is_none() {
                eprintln!(
                    "note: SDKROOT is unset; macOS cross-compile needs a macOS SDK and will \
                     likely fail. This target is normally built in CI on a macOS runner."
                );
            }
            zigbuild(MACOS)?;
        }
        if do_windows {
            xwin(WINDOWS)?;
        }

        eprintln!("\nbinaries under target/<triple>/release/");
        Ok(())
    }

    fn zigbuild(targets: &[&str]) -> anyhow::Result<()> {
        if !have("cargo", &["zigbuild", "--version"]) {
            anyhow::bail!(
                "cargo-zigbuild not found.\n  install: cargo install --locked cargo-zigbuild\n  \
                 and a zig toolchain: https://ziglang.org/download/ (or `pip install ziglang`)"
            );
        }
        for &t in targets {
            let _ = run("rustup", &["target", "add", t]); // best-effort
            run(
                "cargo",
                &["zigbuild", "--release", "-p", "rocker", "--target", t],
            )?;
        }
        Ok(())
    }

    fn xwin(targets: &[&str]) -> anyhow::Result<()> {
        if !have("cargo", &["xwin", "--version"]) {
            anyhow::bail!("cargo-xwin not found.\n  install: cargo install --locked cargo-xwin");
        }
        for &t in targets {
            let _ = run("rustup", &["target", "add", t]); // best-effort
            run(
                "cargo",
                &["xwin", "build", "--release", "-p", "rocker", "--target", t],
            )?;
        }
        Ok(())
    }
}

mod dist {
    pub fn exec(args: &[String]) -> anyhow::Result<()> {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let forward: &[&str] = if refs.is_empty() { &["plan"] } else { &refs };
        super::run("dist", forward).map_err(|e| {
            anyhow::anyhow!(
                "{e}\n\ndist (cargo-dist) is the release driver. Install it with:\n  \
                 curl --proto '=https' --tlsv1.2 -LsSf \
                 https://github.com/astral-sh/cargo-dist/releases/download/v0.28.7/cargo-dist-installer.sh | sh\n\
                 Config lives in dist-workspace.toml; CI in .github/workflows/release.yml."
            )
        })
    }
}

mod budgets {
    use std::path::PathBuf;

    /// Installer / archive size ceiling per platform (PLAN §8).
    const MAX_ARTIFACT_BYTES: u64 = 20 * 1024 * 1024;

    pub fn check() -> anyhow::Result<String> {
        let mut out = String::new();
        out.push_str("performance budgets (PLAN §8)\n");
        out.push_str(&format!(
            "  installer/archive < {} MB ... checked below against target/distrib\n",
            MAX_ARTIFACT_BYTES / 1024 / 1024
        ));
        out.push_str("  idle RAM < 100 MB / 50 containers ... runtime harness (TODO Phase 1)\n");
        out.push_str("  cold start < 1 s ................... runtime harness (TODO Phase 1)\n");

        let Some(dir) = distrib_dir() else {
            out.push_str("\nno target/distrib yet — run `cargo xtask dist build` first\n");
            return Ok(out);
        };

        out.push_str(&format!("\nscanning {}\n", dir.display()));
        let mut over = false;
        let mut checked = 0u32;
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // Only weigh the shippable artifacts, not manifests/checksums.
            let shippable = [
                ".tar.xz",
                ".tar.gz",
                ".zip",
                ".msi",
                ".pkg",
                ".deb",
                ".AppImage",
            ]
            .iter()
            .any(|ext| name.ends_with(ext));
            if !shippable {
                continue;
            }
            let size = entry.metadata()?.len();
            let ok = size <= MAX_ARTIFACT_BYTES;
            over |= !ok;
            checked += 1;
            out.push_str(&format!(
                "  [{}] {name} ({:.1} MB)\n",
                if ok { "ok" } else { "OVER" },
                size as f64 / 1024.0 / 1024.0
            ));
        }
        if checked == 0 {
            out.push_str("  (no shippable artifacts found)\n");
        }
        anyhow::ensure!(
            !over,
            "one or more artifacts over the {} MB budget",
            MAX_ARTIFACT_BYTES / 1024 / 1024
        );
        Ok(out)
    }

    fn distrib_dir() -> Option<PathBuf> {
        let dir = workspace_root().join("target/distrib");
        dir.is_dir().then_some(dir)
    }

    fn workspace_root() -> PathBuf {
        // xtask/ is one level below the workspace root.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}
