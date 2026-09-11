//! Isolated process for one Tier 1 Rocker extension.
//!
//! The parent application owns Docker access. This binary executes only the
//! sandboxed Rhai script and writes requested actions to stdout as
//! newline-delimited JSON.

use std::{
    io::{self, BufRead, BufReader, BufWriter},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
};

use rocker_ext_api::Capability;
use rocker_ext_host::{
    ExtensionRuntime, HostMessage, HostRequest, ProtocolApi, ScriptHostApi, ScriptRuntime,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rocker-ext-host: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let extension_dir = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "missing extension directory argument".to_owned())?;
    let grants = arguments
        .next()
        .ok_or_else(|| "missing JSON capability grants argument".to_owned())?;
    if arguments.next().is_some() {
        return Err("expected exactly two arguments".to_owned());
    }
    let grants: Vec<Capability> = serde_json::from_str(
        grants
            .to_str()
            .ok_or_else(|| "capability grants are not valid UTF-8".to_owned())?,
    )
    .map_err(|error| format!("invalid capability grants: {error}"))?;

    let protocol = Arc::new(ProtocolApi::new(BufWriter::new(io::stdout())));
    let host_api: Arc<dyn ScriptHostApi> = protocol.clone();
    let mut runtime =
        ScriptRuntime::load(extension_dir, grants, host_api).map_err(|error| error.to_string())?;

    let stdin = io::stdin();
    for line in BufReader::new(stdin.lock()).lines() {
        let line = line.map_err(|error| error.to_string())?;
        let request: HostRequest =
            serde_json::from_str(&line).map_err(|error| format!("invalid request: {error}"))?;
        let shutdown = matches!(request, HostRequest::Shutdown);
        let result = match request {
            HostRequest::Activate => runtime.activate(),
            HostRequest::Event { event } => runtime.handle_event(&event),
            HostRequest::RenderPanel { context } => runtime
                .render_panel(&context)
                .and_then(|node| protocol.emit(&HostMessage::Ui { node })),
            HostRequest::Schedule => runtime.run_schedule(),
            HostRequest::Shutdown => Ok(()),
        };
        protocol
            .emit(&HostMessage::Completed {
                ok: result.is_ok(),
                error: result.err().map(|error| error.to_string()),
            })
            .map_err(|error| error.to_string())?;
        if shutdown {
            break;
        }
    }
    Ok(())
}
