// This crate's own binary target, kept only so `cargo test -p rocker-ext-host`
// can spawn `CARGO_BIN_EXE_rocker-ext-host` for process-isolation tests. The
// build the app ships uses `rocker`'s bin wrapper, which `include!`s
// `rocker-ext-host.rs` into the main distribution package instead.
#[path = "rocker-ext-host.rs"]
mod imp;

fn main() -> std::process::ExitCode {
    match imp::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rocker-ext-host: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
