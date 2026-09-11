//! Extension host supervisor (PLAN §5.5).
//!
//! Runs as a **separate supervised process** from the UI for crash isolation and
//! per-extension resource ceilings. This scaffold defines the capability gate
//! and the supervisor's public surface; the `rhai` runtime (Phase 5a) and the
//! `wasmtime` + Component Model runtime (Phase 5b) attach behind
//! [`ExtensionRuntime`].

mod component;

pub use component::{
    ComponentContainer, ComponentHostApi, ComponentLimits, ComponentRuntime, ToastLevel,
};

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rhai::{Engine, EvalAltResult, Position, Scope, AST};
use rocker_ext_api::{Capability, ContainerAction, Manifest, ManifestError, Tier, UiNode};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostError {
    #[error("extension {ext} requested capability {cap:?} that was not granted")]
    CapabilityDenied { ext: String, cap: Capability },
    #[error("extension {0} crashed")]
    Crashed(String),
    #[error("runtime: {0}")]
    Runtime(String),
    #[error("extension manifest is invalid: {0}")]
    Manifest(#[from] ManifestError),
    #[error("manifest TOML: {0}")]
    ManifestToml(String),
    #[error("extension `{id}` has tier {actual:?}; expected script")]
    WrongTier { id: String, actual: Tier },
    #[error("extension entry resolves outside its installation directory")]
    EntryOutsideInstall,
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("extension `{0}` is already installed")]
    AlreadyInstalled(String),
    #[error("extension source contains a symbolic link: {0}")]
    SymbolicLink(PathBuf),
    #[error("extension state TOML: {0}")]
    StateToml(String),
    #[error("host protocol: {0}")]
    Protocol(String),
}

pub type Result<T> = std::result::Result<T, HostError>;

/// The set of capabilities a user granted one extension. All host API calls are
/// checked against this before dispatch.
#[derive(Debug, Clone, Default)]
pub struct CapabilityGate {
    ext_id: String,
    granted: HashSet<Capability>,
}

impl CapabilityGate {
    /// Build a gate from permissions the user has explicitly granted at
    /// installation time. A grant not requested by the manifest is ignored.
    pub fn new(manifest: &Manifest, granted: impl IntoIterator<Item = Capability>) -> Self {
        let requested: HashSet<_> = manifest.capabilities.iter().copied().collect();
        Self {
            ext_id: manifest.id.clone(),
            granted: granted
                .into_iter()
                .filter(|capability| requested.contains(capability))
                .collect(),
        }
    }

    pub fn require(&self, cap: Capability) -> Result<()> {
        if self.granted.contains(&cap) {
            Ok(())
        } else {
            Err(HostError::CapabilityDenied {
                ext: self.ext_id.clone(),
                cap,
            })
        }
    }
}

/// Implemented by each runtime tier.
pub trait ExtensionRuntime {
    fn activate(&mut self) -> Result<()>;
}

/// The narrow, capability-gated API made available to Rhai extensions.
///
/// The engine connection belongs to the application, not the script. Future
/// Docker operations are added here one at a time, with a corresponding
/// [`Capability`] check in [`ScriptRuntime`].
pub trait ScriptHostApi: Send + Sync + 'static {
    /// Ask the application to perform a read-only container operation.
    fn containers_read(&self) -> Result<()>;

    /// Ask the application to execute one container lifecycle action.
    fn containers_lifecycle(&self, container: &str, action: ContainerAction) -> Result<()>;

    /// Display a notification owned and rendered by the application.
    fn notify(&self, message: &str) -> Result<()>;
}

/// Resource limits applied to every script evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptLimits {
    /// Maximum number of Rhai operations before evaluation is interrupted.
    pub max_operations: u64,
    /// Maximum nested function-call depth allowed during evaluation.
    pub max_call_levels: usize,
    /// Maximum expression and function-expression depth.
    pub max_expression_depth: usize,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            max_operations: 50_000,
            max_call_levels: 32,
            max_expression_depth: 64,
        }
    }
}

/// A compiled Tier 1 extension.
///
/// Rhai is configured without its `sync`, `metadata`, `serde`, filesystem, or
/// network features. Scripts only reach the application through the two
/// explicitly registered functions below, each checked by [`CapabilityGate`].
pub struct ScriptRuntime {
    engine: Engine,
    ast: AST,
}

impl ScriptRuntime {
    /// Load an `extension.toml` and its entry script from one installation
    /// directory. Symlink resolution is checked after parsing to preserve the
    /// manifest's directory boundary.
    pub fn load(
        extension_dir: impl AsRef<Path>,
        granted: impl IntoIterator<Item = Capability>,
        host_api: Arc<dyn ScriptHostApi>,
    ) -> Result<Self> {
        let extension_dir = std::fs::canonicalize(extension_dir)?;
        let manifest_text = std::fs::read_to_string(extension_dir.join("extension.toml"))?;
        let manifest: Manifest = toml::from_str(&manifest_text)
            .map_err(|error| HostError::ManifestToml(error.to_string()))?;
        manifest.validate()?;

        if manifest.tier != Tier::Script {
            return Err(HostError::WrongTier {
                id: manifest.id,
                actual: manifest.tier,
            });
        }

        let entry_path = std::fs::canonicalize(extension_dir.join(&manifest.entry))?;
        if !entry_path.starts_with(&extension_dir) {
            return Err(HostError::EntryOutsideInstall);
        }

        let source = std::fs::read_to_string(entry_path)?;
        Self::compile(
            &manifest,
            &source,
            granted,
            host_api,
            ScriptLimits::default(),
        )
    }

    /// Compile a script from trusted installer input while applying resource
    /// limits and registering only the granted application functions.
    pub fn compile(
        manifest: &Manifest,
        source: &str,
        granted: impl IntoIterator<Item = Capability>,
        host_api: Arc<dyn ScriptHostApi>,
        limits: ScriptLimits,
    ) -> Result<Self> {
        manifest.validate()?;
        if manifest.tier != Tier::Script {
            return Err(HostError::WrongTier {
                id: manifest.id.clone(),
                actual: manifest.tier,
            });
        }

        let gate = CapabilityGate::new(manifest, granted);
        let mut engine = Engine::new_raw();
        engine.set_max_operations(limits.max_operations);
        engine.set_max_call_levels(limits.max_call_levels);
        engine.set_max_expr_depths(limits.max_expression_depth, limits.max_expression_depth);

        register_api(&mut engine, gate, host_api);
        let ast = engine
            .compile(source)
            .map_err(|error| HostError::Runtime(error.to_string()))?;

        Ok(Self { engine, ast })
    }

    /// Route a lifecycle or Docker event to the extension's `on_event` hook.
    pub fn handle_event(&self, event: &str) -> Result<()> {
        let mut scope = Scope::new();
        self.engine
            .call_fn::<()>(&mut scope, &self.ast, "on_event", (event.to_owned(),))
            .map_err(|error| HostError::Runtime(error.to_string()))
    }

    /// Render a constrained declarative panel from the script's
    /// `render_panel` hook. The script returns JSON so it cannot execute any
    /// immediate-mode UI code inside Rocker's process.
    pub fn render_panel(&self, context: &str) -> Result<UiNode> {
        let mut scope = Scope::new();
        let json = self
            .engine
            .call_fn::<String>(&mut scope, &self.ast, "render_panel", (context.to_owned(),))
            .map_err(|error| HostError::Runtime(error.to_string()))?;
        serde_json::from_str(&json).map_err(|error| HostError::Runtime(error.to_string()))
    }

    /// Invoke the script's optional recurring-work hook.
    pub fn run_schedule(&self) -> Result<()> {
        let mut scope = Scope::new();
        self.engine
            .call_fn::<()>(&mut scope, &self.ast, "on_schedule", ())
            .map_err(|error| HostError::Runtime(error.to_string()))
    }
}

impl ExtensionRuntime for ScriptRuntime {
    fn activate(&mut self) -> Result<()> {
        let mut scope = Scope::new();
        self.engine
            .call_fn::<()>(&mut scope, &self.ast, "activate", ())
            .map_err(|error| HostError::Runtime(error.to_string()))
    }
}

fn register_api(engine: &mut Engine, gate: CapabilityGate, host_api: Arc<dyn ScriptHostApi>) {
    let containers_gate = gate.clone();
    let containers_api = Arc::clone(&host_api);
    engine.register_fn("containers_read", move || {
        containers_gate
            .require(Capability::ContainersRead)
            .and_then(|()| containers_api.containers_read())
            .map_err(runtime_error)
    });

    let lifecycle_gate = gate.clone();
    let lifecycle_api = Arc::clone(&host_api);
    engine.register_fn(
        "containers_lifecycle",
        move |container: String, action: String| {
            let Some(action) = ContainerAction::parse(&action) else {
                return Err(runtime_error(HostError::Runtime(format!(
                    "unsupported container lifecycle action `{action}`"
                ))));
            };
            lifecycle_gate
                .require(Capability::ContainersLifecycle)
                .and_then(|()| lifecycle_api.containers_lifecycle(&container, action))
                .map_err(runtime_error)
        },
    );

    engine.register_fn("notify", move |message: String| {
        gate.require(Capability::Notifications)
            .and_then(|()| host_api.notify(&message))
            .map_err(runtime_error)
    });
}

fn runtime_error(error: HostError) -> Box<EvalAltResult> {
    EvalAltResult::ErrorRuntime(error.to_string().into(), Position::NONE).into()
}

/// A persisted permission decision for one locally installed extension.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtensionSettings {
    /// Whether Rocker activates the extension.
    pub enabled: bool,
    /// Enables reload-oriented developer tooling for this local extension.
    pub dev_mode: bool,
    /// Capabilities explicitly approved by the user at installation time.
    pub granted_capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct ExtensionStateFile {
    extensions: BTreeMap<String, ExtensionSettings>,
}

/// An extension discovered in the local installation directory.
#[derive(Debug, Clone)]
pub struct InstalledExtension {
    /// Validated extension manifest.
    pub manifest: Manifest,
    /// Canonical extension installation directory.
    pub directory: PathBuf,
    /// Persisted enablement and grant state.
    pub settings: ExtensionSettings,
}

/// The result of discovering local extension folders.
#[derive(Debug, Default)]
pub struct Discovery {
    /// Extensions whose manifests parsed and validated successfully.
    pub extensions: Vec<InstalledExtension>,
    /// Broken folders are reported without hiding healthy extensions.
    pub failures: Vec<(PathBuf, HostError)>,
}

/// Local extension installer and persistent permission registry.
///
/// Each extension lives below `root/<extension-id>`. The registry state remains
/// adjacent to those folders instead of mixing executable extension metadata
/// into Rocker's general application config.
#[derive(Debug)]
pub struct ExtensionRegistry {
    root: PathBuf,
    state: ExtensionStateFile,
}

impl ExtensionRegistry {
    const STATE_FILE: &'static str = "extensions-state.toml";

    /// Open a local registry, returning an empty state before the first install.
    pub fn load(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let state_path = root.join(Self::STATE_FILE);
        let state = match fs::read_to_string(state_path) {
            Ok(contents) => toml::from_str(&contents)
                .map_err(|error| HostError::StateToml(error.to_string()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ExtensionStateFile::default()
            }
            Err(error) => return Err(error.into()),
        };

        Ok(Self { root, state })
    }

    /// Copy a folder into the local registry. Symlinks are rejected so the
    /// installed script and manifest cannot escape their own directory later.
    pub fn install_from_dir(&mut self, source: impl AsRef<Path>) -> Result<InstalledExtension> {
        let source = fs::canonicalize(source)?;
        let manifest = read_manifest(&source)?;
        let destination = self.root.join(&manifest.id);
        if destination.exists() {
            return Err(HostError::AlreadyInstalled(manifest.id));
        }

        copy_extension_dir(&source, &destination)?;
        let settings = self
            .state
            .extensions
            .entry(manifest.id.clone())
            .or_default()
            .clone();
        self.save()?;

        Ok(InstalledExtension {
            manifest,
            directory: destination,
            settings,
        })
    }

    /// Update a local extension's enablement, developer mode, and user grants.
    pub fn set_settings(
        &mut self,
        extension_id: String,
        settings: ExtensionSettings,
    ) -> Result<()> {
        self.state.extensions.insert(extension_id, settings);
        self.save()
    }

    /// Discover every direct child extension directory, retaining errors for a
    /// settings UI to surface while still activating healthy extensions.
    pub fn discover(&self) -> Result<Discovery> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Discovery::default())
            }
            Err(error) => return Err(error.into()),
        };

        let mut discovery = Discovery::default();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_dir() {
                continue;
            }

            match read_manifest(&path) {
                Ok(manifest) => {
                    let settings = self
                        .state
                        .extensions
                        .get(&manifest.id)
                        .cloned()
                        .unwrap_or_default();
                    discovery.extensions.push(InstalledExtension {
                        manifest,
                        directory: fs::canonicalize(path)?,
                        settings,
                    });
                }
                Err(error) => discovery.failures.push((path, error)),
            }
        }
        discovery
            .extensions
            .sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
        Ok(discovery)
    }

    fn save(&self) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let contents = toml::to_string_pretty(&self.state)
            .map_err(|error| HostError::StateToml(error.to_string()))?;
        fs::write(self.root.join(Self::STATE_FILE), contents)?;
        Ok(())
    }
}

fn read_manifest(extension_dir: &Path) -> Result<Manifest> {
    let contents = fs::read_to_string(extension_dir.join("extension.toml"))?;
    let manifest: Manifest =
        toml::from_str(&contents).map_err(|error| HostError::ManifestToml(error.to_string()))?;
    manifest.validate()?;
    Ok(manifest)
}

/// Read and validate one extension's manifest without instantiating a
/// runtime. The isolated `rocker-ext-host` binary uses this to pick the
/// correct tier (`ScriptRuntime` vs [`component::ComponentRuntime`]) before
/// loading it.
pub fn extension_manifest(extension_dir: impl AsRef<Path>) -> Result<Manifest> {
    read_manifest(extension_dir.as_ref())
}

fn copy_extension_dir(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Err(HostError::SymbolicLink(source.to_path_buf()));
    }
    fs::create_dir_all(destination)?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(HostError::SymbolicLink(source_path));
        }
        if file_type.is_dir() {
            copy_extension_dir(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

/// A command sent from Rocker to the isolated extension-host process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "command")]
pub enum HostRequest {
    /// Call the extension's `activate` function.
    Activate,
    /// Route a Docker lifecycle or health event to `on_event`.
    Event { event: String },
    /// Request a declarative panel for the supplied host context JSON.
    RenderPanel { context: String },
    /// Invoke the script's recurring-work hook.
    Schedule,
    /// Answer a [`HostQuery`] the extension emitted mid-command. Only ever
    /// sent in reply to a [`HostMessage::Query`]; never a top-level command.
    Answer { answer: HostAnswer },
    /// Shut down the host process cleanly.
    Shutdown,
}

/// An intent or completion emitted by the isolated extension-host process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "message")]
pub enum HostMessage {
    /// Request a read-only Docker container operation from the application.
    ContainersRead,
    /// Request a lifecycle action for one container.
    ContainersLifecycle {
        /// Docker container identifier.
        container: String,
        /// Requested lifecycle action.
        action: ContainerAction,
    },
    /// Request an application-owned notification.
    Notify { text: String },
    /// A declarative panel that the application renders with `egui`.
    Ui { node: UiNode },
    /// A data query the extension is blocked on. Unlike the intents above,
    /// which the application executes independently and never answers, the
    /// process cannot resume running the extension until it receives a
    /// matching [`HostRequest::Answer`] on its standard input.
    Query { query: HostQuery },
    /// Report the result of a host command.
    Completed { ok: bool, error: Option<String> },
}

/// A blocking data query emitted by a Tier 2 (component) extension through
/// [`crate::component::ComponentHostApi`]. Tier 1 (rhai) extensions never
/// emit these today: [`ScriptHostApi`] is fire-and-forget by design.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "query")]
pub enum HostQuery {
    /// Ask for the containers visible to this extension's host scope.
    ListContainers,
    /// Ask for a bounded tail of one container's logs.
    LogsTail { container: String, lines: u32 },
}

/// The application's reply to a [`HostQuery`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "answer")]
pub enum HostAnswer {
    /// Reply to [`HostQuery::ListContainers`].
    Containers { containers: Vec<ComponentContainer> },
    /// Reply to [`HostQuery::LogsTail`].
    Logs { lines: Vec<String> },
    /// The application could not answer the query (e.g. no live Docker
    /// connection). Carried as data rather than a transport failure so the
    /// extension's own error handling sees it, the same way
    /// [`HostMessage::Completed`] carries a script's runtime error.
    Error { message: String },
}

/// A synchronous source of Docker data the supervisor consults while a Tier 2
/// extension is blocked on a [`HostQuery`].
///
/// Kept separate from the fire-and-forget [`HostMessage`] intents: this is
/// the one path where an extension needs a real answer, not just permission,
/// before it can continue running. Implemented by whichever part of the
/// application owns the live Docker connection (the UI/engine bridge); until
/// that's wired in, [`NoDataSource`] answers every query with an error so
/// Component-tier extensions still run, just without live data.
pub trait ExtensionDataSource: Send + Sync {
    /// Answer [`HostQuery::ListContainers`].
    fn list_containers(&self) -> Result<Vec<ComponentContainer>>;
    /// Answer [`HostQuery::LogsTail`].
    fn logs_tail(&self, container: &str, lines: u32) -> Result<Vec<String>>;
}

/// An [`ExtensionDataSource`] that answers every query with an error.
/// The default until the application wires in a real, Docker-backed source.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoDataSource;

impl ExtensionDataSource for NoDataSource {
    fn list_containers(&self) -> Result<Vec<ComponentContainer>> {
        Err(HostError::Runtime(
            "no live data source is configured for this extension host".into(),
        ))
    }

    fn logs_tail(&self, _container: &str, _lines: u32) -> Result<Vec<String>> {
        Err(HostError::Runtime(
            "no live data source is configured for this extension host".into(),
        ))
    }
}

fn answer_query(data_source: &dyn ExtensionDataSource, query: &HostQuery) -> HostAnswer {
    let result = match query {
        HostQuery::ListContainers => data_source
            .list_containers()
            .map(|containers| HostAnswer::Containers { containers }),
        HostQuery::LogsTail { container, lines } => data_source
            .logs_tail(container, *lines)
            .map(|lines| HostAnswer::Logs { lines }),
    };
    result.unwrap_or_else(|error| HostAnswer::Error {
        message: error.to_string(),
    })
}

/// Emit a [`HostQuery`] and block for the matching [`HostRequest::Answer`].
///
/// Used by the isolated `rocker-ext-host` binary so a Tier 2 extension's
/// blocked import call can resume once the supervising application has
/// replied. Generic over the reader/writer so the blocking exchange can be
/// exercised with in-memory buffers in tests, without a real child process or
/// any wasm involved.
pub fn ask_query<W, R>(
    protocol: &ProtocolApi<W>,
    reader: &mut R,
    query: HostQuery,
) -> Result<HostAnswer>
where
    W: Write,
    R: BufRead,
{
    protocol.emit(&HostMessage::Query { query })?;
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(HostError::Protocol(
            "extension host's answer stream closed".into(),
        ));
    }
    match serde_json::from_str::<HostRequest>(&line)
        .map_err(|error| HostError::Protocol(error.to_string()))?
    {
        HostRequest::Answer { answer } => Ok(answer),
        other => Err(HostError::Protocol(format!(
            "expected an answer to a pending query, got {other:?}"
        ))),
    }
}

/// A [`ScriptHostApi`] that forwards script intents over the line-delimited
/// JSON host protocol. The application remains the only Docker client.
pub struct ProtocolApi<W> {
    writer: Mutex<W>,
}

/// A supervised, separate process hosting one extension runtime.
///
/// Requests and intents use [`HostRequest`] and [`HostMessage`] over the
/// process's line-delimited JSON standard streams. The UI process can therefore
/// terminate a misbehaving extension without taking down Docker management.
pub struct HostProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl HostProcess {
    /// Spawn the `rocker-ext-host` binary for an installed extension.
    pub fn spawn(
        host_program: impl AsRef<Path>,
        extension_dir: impl AsRef<Path>,
        granted: &[Capability],
    ) -> Result<Self> {
        let grants = serde_json::to_string(granted)
            .map_err(|error| HostError::Protocol(error.to_string()))?;
        let mut child = Command::new(host_program.as_ref())
            .arg(extension_dir.as_ref())
            .arg(grants)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| {
            HostError::Runtime("extension host did not expose standard input".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            HostError::Runtime("extension host did not expose standard output".into())
        })?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    /// Send one command and collect its API intents through the completion
    /// message. Any [`HostMessage::Query`] the extension emits mid-command is
    /// answered from `data_source` and does not appear in the returned
    /// intents; the caller executes the remaining, fire-and-forget intents
    /// against its own Docker API.
    pub fn request(
        &mut self,
        request: &HostRequest,
        data_source: &dyn ExtensionDataSource,
    ) -> Result<Vec<HostMessage>> {
        drive_request(&mut self.stdout, &mut self.stdin, request, data_source)
    }

    /// Request a clean shutdown, then wait for the child to exit.
    pub fn shutdown(mut self) -> Result<()> {
        self.request(&HostRequest::Shutdown, &NoDataSource)?;
        let status = self.child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(HostError::Crashed(format!(
                "extension host exited with {status}"
            )))
        }
    }
}

/// Send `request` over `writer` and drive `reader` until the matching
/// [`HostMessage::Completed`], answering any [`HostMessage::Query`] the
/// extension emits along the way from `data_source`.
///
/// Factored out of [`HostProcess::request`] so the parent side of the
/// query/answer exchange can be exercised with in-memory buffers in tests,
/// without spawning a real child process.
fn drive_request<R, W>(
    mut reader: R,
    mut writer: W,
    request: &HostRequest,
    data_source: &dyn ExtensionDataSource,
) -> Result<Vec<HostMessage>>
where
    R: BufRead,
    W: Write,
{
    serde_json::to_writer(&mut writer, request)
        .map_err(|error| HostError::Protocol(error.to_string()))?;
    writer.write_all(b"\n")?;
    writer.flush()?;

    let mut messages = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(HostError::Crashed(
                "extension host closed its protocol stream".into(),
            ));
        }
        let message: HostMessage =
            serde_json::from_str(&line).map_err(|error| HostError::Protocol(error.to_string()))?;
        match message {
            HostMessage::Query { query } => {
                let answer = answer_query(data_source, &query);
                serde_json::to_writer(&mut writer, &HostRequest::Answer { answer })
                    .map_err(|error| HostError::Protocol(error.to_string()))?;
                writer.write_all(b"\n")?;
                writer.flush()?;
            }
            HostMessage::Completed { .. } => {
                messages.push(message);
                return Ok(messages);
            }
            other => messages.push(other),
        }
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An intent emitted by an extension, paired with its stable extension id.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionIntent {
    /// The installed extension that emitted this intent.
    pub extension_id: String,
    /// The capability-gated operation requested by the extension.
    pub message: HostMessage,
}

/// Owns isolated child processes for the enabled extensions, of either tier.
///
/// The supervisor deliberately returns intents rather than performing Docker
/// work itself. This retains the application's single Engine client and makes
/// the UI/engine event bridge the sole authority that can execute them. The
/// one exception is a Tier 2 extension's [`HostQuery`]: that's answered
/// synchronously, inline, from `data_source`, because the extension is
/// blocked on the reply and cannot be resumed later the way a fire-and-forget
/// intent can.
pub struct ExtensionSupervisor {
    host_program: PathBuf,
    hosts: BTreeMap<String, HostProcess>,
    schedules: BTreeMap<String, ScheduledExtension>,
    data_source: Arc<dyn ExtensionDataSource>,
}

struct ScheduledExtension {
    every: Duration,
    due: Instant,
}

impl ExtensionSupervisor {
    /// Create a supervisor using the path to the `rocker-ext-host` executable.
    /// `data_source` answers the [`HostQuery`]s Tier 2 extensions block on;
    /// pass [`NoDataSource`] until the application wires in a live one.
    pub fn new(
        host_program: impl Into<PathBuf>,
        data_source: Arc<dyn ExtensionDataSource>,
    ) -> Self {
        Self {
            host_program: host_program.into(),
            hosts: BTreeMap::new(),
            schedules: BTreeMap::new(),
            data_source,
        }
    }

    /// Launch every enabled extension from one registry discovery pass,
    /// script and component tier alike — both run as an isolated
    /// `rocker-ext-host` child process over the same protocol.
    pub fn launch_enabled(&mut self, discovery: Discovery) -> Result<()> {
        self.shutdown_all()?;
        for extension in discovery.extensions {
            if extension.settings.enabled {
                let host = HostProcess::spawn(
                    &self.host_program,
                    &extension.directory,
                    &extension.settings.granted_capabilities,
                )?;
                let schedule = extension.manifest.schedule_interval();
                let extension_id = extension.manifest.id;
                if let Some(every) = schedule {
                    self.schedules.insert(
                        extension_id.clone(),
                        ScheduledExtension {
                            every,
                            due: Instant::now() + every,
                        },
                    );
                }
                self.hosts.insert(extension_id, host);
            }
        }
        Ok(())
    }

    /// Activate every supervised extension and return all requested intents.
    pub fn activate_all(&mut self) -> Result<Vec<ExtensionIntent>> {
        self.request_all(HostRequest::Activate)
    }

    /// Broadcast a Docker lifecycle or health event to every supervised script.
    pub fn dispatch_event(&mut self, event: &str) -> Result<Vec<ExtensionIntent>> {
        self.request_all(HostRequest::Event {
            event: event.to_owned(),
        })
    }

    /// Ask every supervised extension for its current declarative panel tree.
    pub fn render_panels(&mut self, context: &str) -> Result<Vec<ExtensionIntent>> {
        self.request_all(HostRequest::RenderPanel {
            context: context.to_owned(),
        })
    }

    /// Run every scheduled extension whose interval has elapsed.
    pub fn tick_schedules(&mut self, now: Instant) -> Result<Vec<ExtensionIntent>> {
        let due = self
            .schedules
            .iter_mut()
            .filter_map(|(extension_id, schedule)| {
                if now >= schedule.due {
                    schedule.due = now + schedule.every;
                    Some(extension_id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        self.request_selected(&due, &HostRequest::Schedule)
    }

    /// Stop every child process. The first shutdown failure is returned after
    /// all remaining children have still been given a shutdown request.
    pub fn shutdown_all(&mut self) -> Result<()> {
        let hosts = std::mem::take(&mut self.hosts);
        self.schedules.clear();
        let mut first_error = None;
        for (_, host) in hosts {
            if let Err(error) = host.shutdown() {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn request_all(&mut self, request: HostRequest) -> Result<Vec<ExtensionIntent>> {
        let extension_ids = self.hosts.keys().cloned().collect::<Vec<_>>();
        self.request_selected(&extension_ids, &request)
    }

    fn request_selected(
        &mut self,
        extension_ids: &[String],
        request: &HostRequest,
    ) -> Result<Vec<ExtensionIntent>> {
        let mut intents = Vec::new();
        for extension_id in extension_ids {
            let Some(host) = self.hosts.get_mut(extension_id) else {
                continue;
            };
            let messages = host.request(request, self.data_source.as_ref())?;
            for message in messages {
                if !matches!(message, HostMessage::Completed { .. }) {
                    intents.push(ExtensionIntent {
                        extension_id: extension_id.clone(),
                        message,
                    });
                }
            }
        }
        Ok(intents)
    }
}

impl Drop for ExtensionSupervisor {
    fn drop(&mut self) {
        let _ = self.shutdown_all();
    }
}

impl<W> ProtocolApi<W>
where
    W: Write,
{
    /// Create a protocol API over a write stream owned by the host process.
    pub fn new(writer: W) -> Self {
        Self {
            writer: Mutex::new(writer),
        }
    }

    /// Emit a protocol message as one newline-delimited JSON object.
    pub fn emit(&self, message: &HostMessage) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|error| HostError::Protocol(error.to_string()))?;
        serde_json::to_writer(&mut *writer, message)
            .map_err(|error| HostError::Protocol(error.to_string()))?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }
}

impl<W> ScriptHostApi for ProtocolApi<W>
where
    W: Write + Send + 'static,
{
    fn containers_read(&self) -> Result<()> {
        self.emit(&HostMessage::ContainersRead)
    }

    fn containers_lifecycle(&self, container: &str, action: ContainerAction) -> Result<()> {
        self.emit(&HostMessage::ContainersLifecycle {
            container: container.to_owned(),
            action,
        })
    }

    fn notify(&self, message: &str) -> Result<()> {
        self.emit(&HostMessage::Notify {
            text: message.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocker_ext_api::Tier;
    use std::sync::Mutex;
    use tempfile::tempdir;

    fn manifest(caps: Vec<Capability>) -> Manifest {
        Manifest {
            id: "example".into(),
            name: "Example".into(),
            version: "0.1.0".into(),
            tier: Tier::Script,
            capabilities: caps,
            entry: "main.rhai".into(),
            schedule_seconds: None,
        }
    }

    #[derive(Default)]
    struct RecordingApi {
        calls: Mutex<Vec<String>>,
    }

    impl ScriptHostApi for RecordingApi {
        fn containers_read(&self) -> Result<()> {
            self.calls
                .lock()
                .map_err(|error| HostError::Runtime(error.to_string()))?
                .push("containers_read".into());
            Ok(())
        }

        fn containers_lifecycle(&self, container: &str, action: ContainerAction) -> Result<()> {
            self.calls
                .lock()
                .map_err(|error| HostError::Runtime(error.to_string()))?
                .push(format!("lifecycle:{container}:{action:?}"));
            Ok(())
        }

        fn notify(&self, message: &str) -> Result<()> {
            self.calls
                .lock()
                .map_err(|error| HostError::Runtime(error.to_string()))?
                .push(format!("notify:{message}"));
            Ok(())
        }
    }

    #[test]
    fn gate_enforces_grants() {
        let gate = CapabilityGate::new(
            &manifest(vec![Capability::ContainersRead]),
            [Capability::ContainersRead],
        );
        assert!(gate.require(Capability::ContainersRead).is_ok());
        assert!(gate.require(Capability::ContainersLifecycle).is_err());
    }

    #[test]
    fn gate_does_not_treat_a_requested_capability_as_a_grant() {
        let gate = CapabilityGate::new(&manifest(vec![Capability::ContainersRead]), []);

        assert!(gate.require(Capability::ContainersRead).is_err());
    }

    #[test]
    fn script_runtime_routes_only_granted_capabilities() {
        let host = Arc::new(RecordingApi::default());
        let api: Arc<dyn ScriptHostApi> = host.clone();
        let manifest = manifest(vec![Capability::ContainersRead, Capability::Notifications]);
        let mut runtime = ScriptRuntime::compile(
            &manifest,
            r#"
                fn activate() {
                    containers_read();
                    notify("ready");
                }
            "#,
            [Capability::ContainersRead, Capability::Notifications],
            api,
            ScriptLimits::default(),
        )
        .expect("script compiles");

        runtime.activate().expect("script activates");

        assert_eq!(
            *host.calls.lock().expect("recording API lock is available"),
            ["containers_read", "notify:ready"]
        );
    }

    #[test]
    fn script_runtime_denies_an_ungranted_api_call() {
        let host = Arc::new(RecordingApi::default());
        let api: Arc<dyn ScriptHostApi> = host.clone();
        let mut runtime = ScriptRuntime::compile(
            &manifest(vec![]),
            "fn activate() { notify(\"not allowed\"); }",
            [],
            api,
            ScriptLimits::default(),
        )
        .expect("script compiles");

        assert!(runtime.activate().is_err());
        assert!(host
            .calls
            .lock()
            .expect("recording API lock is available")
            .is_empty());
    }

    #[test]
    fn script_runtime_routes_granted_lifecycle_actions() {
        let host = Arc::new(RecordingApi::default());
        let api: Arc<dyn ScriptHostApi> = host.clone();
        let mut runtime = ScriptRuntime::compile(
            &manifest(vec![Capability::ContainersLifecycle]),
            "fn activate() { containers_lifecycle(\"abc123\", \"restart\"); }",
            [Capability::ContainersLifecycle],
            api,
            ScriptLimits::default(),
        )
        .expect("script compiles");

        runtime.activate().expect("script activates");

        assert_eq!(
            *host.calls.lock().expect("recording API lock is available"),
            ["lifecycle:abc123:Restart"]
        );
    }

    #[test]
    fn script_runtime_returns_a_declarative_ui_node() {
        let host: Arc<dyn ScriptHostApi> = Arc::new(RecordingApi::default());
        let runtime = ScriptRuntime::compile(
            &manifest(vec![]),
            r#"fn render_panel(_context) { "{\"node\":\"label\",\"text\":\"Hello\"}" }"#,
            [],
            host,
            ScriptLimits::default(),
        )
        .expect("script compiles");

        assert_eq!(
            runtime.render_panel("{}").expect("panel renders"),
            UiNode::Label {
                text: "Hello".into()
            }
        );
    }

    #[test]
    fn script_runtime_runs_a_schedule_hook() {
        let host = Arc::new(RecordingApi::default());
        let api: Arc<dyn ScriptHostApi> = host.clone();
        let runtime = ScriptRuntime::compile(
            &manifest(vec![Capability::Notifications]),
            "fn on_schedule() { notify(\"scheduled\"); }",
            [Capability::Notifications],
            api,
            ScriptLimits::default(),
        )
        .expect("script compiles");

        runtime.run_schedule().expect("schedule hook runs");

        assert_eq!(
            *host.calls.lock().expect("recording API lock is available"),
            ["notify:scheduled"]
        );
    }

    #[test]
    fn script_runtime_stops_after_its_operation_budget() {
        let host: Arc<dyn ScriptHostApi> = Arc::new(RecordingApi::default());
        let mut runtime = ScriptRuntime::compile(
            &manifest(vec![]),
            "fn activate() { while true {} }",
            [],
            host,
            ScriptLimits {
                max_operations: 128,
                ..ScriptLimits::default()
            },
        )
        .expect("script compiles");

        assert!(runtime.activate().is_err());
    }

    #[test]
    fn registry_install_persists_explicit_grants() {
        let source = tempdir().expect("source directory is created");
        let install = tempdir().expect("installation directory is created");
        fs::write(
            source.path().join("extension.toml"),
            r#"
                id = "example.extension"
                name = "Example"
                version = "0.1.0"
                tier = "script"
                capabilities = ["notifications"]
                entry = "main.rhai"
            "#,
        )
        .expect("manifest is written");
        fs::write(source.path().join("main.rhai"), "fn activate() {}").expect("script is written");

        let mut registry =
            ExtensionRegistry::load(install.path().join("extensions")).expect("registry loads");
        registry
            .install_from_dir(source.path())
            .expect("extension installs");
        registry
            .set_settings(
                "example.extension".into(),
                ExtensionSettings {
                    enabled: true,
                    dev_mode: true,
                    granted_capabilities: vec![Capability::Notifications],
                },
            )
            .expect("settings save");

        let reloaded =
            ExtensionRegistry::load(install.path().join("extensions")).expect("registry reloads");
        let discovery = reloaded.discover().expect("extension discovery succeeds");

        assert_eq!(discovery.extensions.len(), 1);
        assert!(discovery.extensions[0].settings.enabled);
        assert!(discovery.extensions[0].settings.dev_mode);
        assert_eq!(
            discovery.extensions[0].settings.granted_capabilities,
            [Capability::Notifications]
        );
    }

    #[test]
    fn protocol_messages_round_trip_as_json() {
        let request = HostRequest::Event {
            event: "health_status".into(),
        };
        let message = HostMessage::Notify {
            text: "healthy".into(),
        };

        assert_eq!(
            serde_json::from_str::<HostRequest>(
                &serde_json::to_string(&request).expect("serializes")
            )
            .expect("deserializes"),
            request
        );
        assert_eq!(
            serde_json::from_str::<HostMessage>(
                &serde_json::to_string(&message).expect("serializes")
            )
            .expect("deserializes"),
            message
        );
    }

    #[test]
    fn reference_extensions_compile_with_their_requested_grants() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/examples");
        let host: Arc<dyn ScriptHostApi> = Arc::new(RecordingApi::default());

        for extension in ["container-notifier", "container-summary"] {
            let directory = root.join(extension);
            let manifest = read_manifest(&directory).expect("reference manifest is valid");
            let source = fs::read_to_string(directory.join(&manifest.entry))
                .expect("reference script is readable");
            ScriptRuntime::compile(
                &manifest,
                &source,
                manifest.capabilities.clone(),
                Arc::clone(&host),
                ScriptLimits::default(),
            )
            .expect("reference script compiles");
        }
    }

    // The isolated process's query/answer exchange (`drive_request` on the
    // supervisor side, `ask_query` on the child side) is tested here with
    // in-memory readers/writers standing in for the two ends of the pipe.
    // That proves the actual bidirectional protocol logic without spawning a
    // real child process or compiling any wasm.

    #[derive(Default)]
    struct StubDataSource {
        containers: Vec<ComponentContainer>,
        logs: Vec<String>,
        fail: bool,
    }

    impl ExtensionDataSource for StubDataSource {
        fn list_containers(&self) -> Result<Vec<ComponentContainer>> {
            if self.fail {
                Err(HostError::Runtime("no docker connection".into()))
            } else {
                Ok(self.containers.clone())
            }
        }

        fn logs_tail(&self, _container: &str, _lines: u32) -> Result<Vec<String>> {
            if self.fail {
                Err(HostError::Runtime("no docker connection".into()))
            } else {
                Ok(self.logs.clone())
            }
        }
    }

    /// A `Write` sink that keeps its bytes reachable after the writer has
    /// been moved into whatever it's driving, so a test can inspect what was
    /// sent.
    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn ndjson(messages: &[impl Serialize]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for message in messages {
            serde_json::to_writer(&mut bytes, message).expect("serializes");
            bytes.push(b'\n');
        }
        bytes
    }

    fn lines_of<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Vec<T> {
        std::str::from_utf8(bytes)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("deserializes"))
            .collect()
    }

    #[test]
    fn drive_request_answers_a_query_and_does_not_surface_it_as_an_intent() {
        let container = ComponentContainer {
            id: "abc123".into(),
            name: "web".into(),
            image: "nginx".into(),
            state: "running".into(),
            status: "Up 2 minutes".into(),
        };
        let reader = std::io::Cursor::new(ndjson(&[
            HostMessage::Query {
                query: HostQuery::ListContainers,
            },
            HostMessage::Completed {
                ok: true,
                error: None,
            },
        ]));
        let data_source = StubDataSource {
            containers: vec![container.clone()],
            ..Default::default()
        };
        let mut sent = Vec::new();

        let messages = drive_request(reader, &mut sent, &HostRequest::Activate, &data_source)
            .expect("request completes");

        assert_eq!(
            messages,
            [HostMessage::Completed {
                ok: true,
                error: None
            }]
        );

        let written: Vec<HostRequest> = lines_of(&sent);
        assert_eq!(
            written,
            [
                HostRequest::Activate,
                HostRequest::Answer {
                    answer: HostAnswer::Containers {
                        containers: vec![container]
                    }
                }
            ]
        );
    }

    #[test]
    fn drive_request_turns_a_failed_query_into_an_error_answer_not_a_crash() {
        let reader = std::io::Cursor::new(ndjson(&[
            HostMessage::Query {
                query: HostQuery::LogsTail {
                    container: "abc123".into(),
                    lines: 10,
                },
            },
            HostMessage::Completed {
                ok: true,
                error: None,
            },
        ]));
        let data_source = StubDataSource {
            fail: true,
            ..Default::default()
        };
        let mut sent = Vec::new();

        drive_request(reader, &mut sent, &HostRequest::Activate, &data_source)
            .expect("request still completes");

        let written: Vec<HostRequest> = lines_of(&sent);
        assert_eq!(
            written[1],
            HostRequest::Answer {
                answer: HostAnswer::Error {
                    message: "runtime: no docker connection".into()
                }
            }
        );
    }

    #[test]
    fn ask_query_blocks_for_and_returns_the_matching_answer() {
        let writer = SharedBuffer::default();
        let protocol = ProtocolApi::new(writer.clone());
        let mut reader = std::io::Cursor::new(ndjson(&[HostRequest::Answer {
            answer: HostAnswer::Logs {
                lines: vec!["hello".into()],
            },
        }]));

        let answer = ask_query(
            &protocol,
            &mut reader,
            HostQuery::LogsTail {
                container: "abc123".into(),
                lines: 5,
            },
        )
        .expect("query is answered");

        assert_eq!(
            answer,
            HostAnswer::Logs {
                lines: vec!["hello".into()]
            }
        );
        let written: Vec<HostMessage> = lines_of(&writer.0.lock().expect("lock"));
        assert_eq!(
            written,
            [HostMessage::Query {
                query: HostQuery::LogsTail {
                    container: "abc123".into(),
                    lines: 5
                }
            }]
        );
    }

    #[test]
    fn ask_query_rejects_a_reply_that_is_not_an_answer() {
        let protocol = ProtocolApi::new(Vec::new());
        let mut reader = std::io::Cursor::new(ndjson(&[HostRequest::Shutdown]));

        let result = ask_query(&protocol, &mut reader, HostQuery::ListContainers);

        assert!(matches!(result, Err(HostError::Protocol(_))));
    }
}
