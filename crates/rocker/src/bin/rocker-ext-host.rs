// Keep the supervised helper's implementation in its dedicated crate while
// compiling it as part of the single Rocker distribution package.
use std::process::ExitCode;

include!("../../../rocker-ext-host/src/bin/rocker-ext-host.rs");

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rocker-ext-host: {error}");
            ExitCode::FAILURE
        }
    }
}
