//! Isolated process for one Rocker extension, either tier.
//!
//! The parent application owns Docker access. This binary executes only the
//! sandboxed script or component and writes requested actions to stdout as
//! newline-delimited JSON. A Tier 2 (component) extension's data queries
//! (`HostQuery`) block on a matching `HostRequest::Answer` read back from
//! standard input before the extension can resume.

use std::{
    io::{self, BufRead, BufReader, BufWriter},
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, Mutex},
};

use rocker_ext_api::{Capability, ContainerAction, Tier};
use rocker_ext_host::{
    ask_query, extension_manifest, ComponentContainer, ComponentHostApi, ComponentRuntime,
    ExtensionRuntime, HostAnswer, HostError, HostMessage, HostQuery, HostRequest, ProtocolApi,
    Result as HostResult, ScriptHostApi, ScriptRuntime, ToastLevel,
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

    let manifest = extension_manifest(&extension_dir).map_err(|error| error.to_string())?;
    match manifest.tier {
        Tier::Script => run_script(extension_dir, grants),
        Tier::Component => run_component(extension_dir, grants),
        Tier::Theme => {
            Err("theme extensions are pure data and have no host process to run".to_owned())
        }
    }
}

/// Tier 1: the script never blocks on a reply, so a plain `.lines()` loop
/// over standard input is enough.
fn run_script(extension_dir: PathBuf, grants: Vec<Capability>) -> Result<(), String> {
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
            HostRequest::Answer { .. } => Err(HostError::Protocol(
                "received an answer with no pending query".into(),
            )),
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

/// Tier 2: `render_panel` can call a data-returning host import
/// (`list_containers`, `logs_tail`) mid-call, which must block this process
/// until the supervisor answers. That nested read has to share the same
/// standard-input stream as the outer request loop, so both go through one
/// `Mutex<BufReader<Stdin>>` instead of each owning a private reader.
fn run_component(extension_dir: PathBuf, grants: Vec<Capability>) -> Result<(), String> {
    let stdin = Arc::new(Mutex::new(BufReader::new(io::stdin())));
    let protocol = Arc::new(ProtocolApi::new(BufWriter::new(io::stdout())));
    let host_api: Arc<dyn ComponentHostApi> = Arc::new(ProtocolComponentApi {
        protocol: protocol.clone(),
        stdin: stdin.clone(),
    });
    let mut runtime = ComponentRuntime::load(extension_dir, grants, host_api)
        .map_err(|error| error.to_string())?;

    while let Some(request) = read_top_level_request(&stdin).map_err(|error| error.to_string())? {
        let shutdown = matches!(request, HostRequest::Shutdown);
        let result = match request {
            HostRequest::Activate => runtime.activate(),
            HostRequest::RenderPanel { context } => runtime
                .render_panel(&context)
                .and_then(|node| protocol.emit(&HostMessage::Ui { node })),
            HostRequest::Event { .. } => Err(HostError::Runtime(
                "component extensions do not implement on_event yet".into(),
            )),
            HostRequest::Schedule => Err(HostError::Runtime(
                "component extensions do not implement on_schedule yet".into(),
            )),
            HostRequest::Shutdown => Ok(()),
            HostRequest::Answer { .. } => Err(HostError::Protocol(
                "received an answer with no pending query".into(),
            )),
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

/// Read one top-level `HostRequest` line, or `None` at end of stream.
///
/// Only used for the outer command loop; a blocked query's own reply is read
/// through [`ask_query`] instead, sharing the same locked reader.
fn read_top_level_request(stdin: &Mutex<BufReader<io::Stdin>>) -> HostResult<Option<HostRequest>> {
    let mut reader = stdin
        .lock()
        .map_err(|_| HostError::Protocol("standard input lock was poisoned".into()))?;
    let mut line = String::new();
    if reader.read_line(&mut line).map_err(HostError::Io)? == 0 {
        return Ok(None);
    }
    serde_json::from_str(&line)
        .map(Some)
        .map_err(|error| HostError::Protocol(format!("invalid request: {error}")))
}

/// Forwards [`ComponentHostApi`] calls over the isolated-process protocol.
/// Lifecycle and notifications are fire-and-forget, like [`ScriptHostApi`];
/// `list_containers` and `logs_tail` block on [`ask_query`] for real data.
struct ProtocolComponentApi {
    protocol: Arc<ProtocolApi<BufWriter<io::Stdout>>>,
    stdin: Arc<Mutex<BufReader<io::Stdin>>>,
}

impl ComponentHostApi for ProtocolComponentApi {
    fn list_containers(&self) -> HostResult<Vec<ComponentContainer>> {
        match self.ask(HostQuery::ListContainers)? {
            HostAnswer::Containers { containers } => Ok(containers),
            HostAnswer::Error { message } => Err(HostError::Runtime(message)),
            other => Err(unexpected_answer("list-containers", &other)),
        }
    }

    fn lifecycle(&self, container: &str, action: ContainerAction) -> HostResult<()> {
        self.protocol.emit(&HostMessage::ContainersLifecycle {
            container: container.to_owned(),
            action,
        })
    }

    fn logs_tail(&self, container: &str, lines: u32) -> HostResult<Vec<String>> {
        match self.ask(HostQuery::LogsTail {
            container: container.to_owned(),
            lines,
        })? {
            HostAnswer::Logs { lines } => Ok(lines),
            HostAnswer::Error { message } => Err(HostError::Runtime(message)),
            other => Err(unexpected_answer("logs-tail", &other)),
        }
    }

    fn notify(&self, level: ToastLevel, text: &str) -> HostResult<()> {
        // `HostMessage::Notify` carries plain text; fold the level in rather
        // than widening the wire format for one caller.
        let prefix = match level {
            ToastLevel::Info => "info",
            ToastLevel::Warn => "warn",
            ToastLevel::Error => "error",
        };
        self.protocol.emit(&HostMessage::Notify {
            text: format!("[{prefix}] {text}"),
        })
    }
}

impl ProtocolComponentApi {
    fn ask(&self, query: HostQuery) -> HostResult<HostAnswer> {
        let mut reader = self
            .stdin
            .lock()
            .map_err(|_| HostError::Protocol("standard input lock was poisoned".into()))?;
        ask_query(&self.protocol, &mut *reader, query)
    }
}

fn unexpected_answer(query: &str, answer: &HostAnswer) -> HostError {
    HostError::Protocol(format!("unexpected answer to a {query} query: {answer:?}"))
}
