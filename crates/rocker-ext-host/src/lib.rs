//! Extension host supervisor (PLAN §5.5).
//!
//! Runs as a **separate supervised process** from the UI for crash isolation and
//! per-extension resource ceilings. This scaffold defines the capability gate
//! and the supervisor's public surface; the `rhai` runtime (Phase 5a) and the
//! `wasmtime` + Component Model runtime (Phase 5b) attach behind
//! [`ExtensionRuntime`].

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
};

use rhai::{Engine, EvalAltResult, Position, Scope, AST};
use rocker_ext_api::{Capability, ContainerAction, Manifest, ManifestError, Tier};
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
    /// Shut down the host process cleanly.
    Shutdown,
}

/// An intent or completion emitted by the isolated extension-host process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Report the result of a host command.
    Completed { ok: bool, error: Option<String> },
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
    /// message. The caller executes those intents against its own Docker API.
    pub fn request(&mut self, request: &HostRequest) -> Result<Vec<HostMessage>> {
        serde_json::to_writer(&mut self.stdin, request)
            .map_err(|error| HostError::Protocol(error.to_string()))?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;

        let mut messages = Vec::new();
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line)? == 0 {
                return Err(HostError::Crashed(
                    "extension host closed its protocol stream".into(),
                ));
            }
            let message: HostMessage = serde_json::from_str(&line)
                .map_err(|error| HostError::Protocol(error.to_string()))?;
            let completed = matches!(message, HostMessage::Completed { .. });
            messages.push(message);
            if completed {
                return Ok(messages);
            }
        }
    }

    /// Request a clean shutdown, then wait for the child to exit.
    pub fn shutdown(mut self) -> Result<()> {
        self.request(&HostRequest::Shutdown)?;
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

impl Drop for HostProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An intent emitted by an extension, paired with its stable extension id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionIntent {
    /// The installed extension that emitted this intent.
    pub extension_id: String,
    /// The capability-gated operation requested by the extension.
    pub message: HostMessage,
}

/// Owns isolated child processes for the enabled Tier 1 extensions.
///
/// The supervisor deliberately returns intents rather than performing Docker
/// work itself. This retains the application's single Engine client and makes
/// the UI/engine event bridge the sole authority that can execute them.
pub struct ExtensionSupervisor {
    host_program: PathBuf,
    hosts: BTreeMap<String, HostProcess>,
}

impl ExtensionSupervisor {
    /// Create a supervisor using the path to the `rocker-ext-host` executable.
    pub fn new(host_program: impl Into<PathBuf>) -> Self {
        Self {
            host_program: host_program.into(),
            hosts: BTreeMap::new(),
        }
    }

    /// Launch all enabled script extensions from one registry discovery pass.
    /// Component extensions remain discoverable but cannot be launched until
    /// their Component Model runtime is attached.
    pub fn launch_enabled(&mut self, discovery: Discovery) -> Result<()> {
        self.shutdown_all()?;
        for extension in discovery.extensions {
            if extension.settings.enabled && extension.manifest.tier == Tier::Script {
                let host = HostProcess::spawn(
                    &self.host_program,
                    &extension.directory,
                    &extension.settings.granted_capabilities,
                )?;
                self.hosts.insert(extension.manifest.id, host);
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

    /// Stop every child process. The first shutdown failure is returned after
    /// all remaining children have still been given a shutdown request.
    pub fn shutdown_all(&mut self) -> Result<()> {
        let hosts = std::mem::take(&mut self.hosts);
        let mut first_error = None;
        for (_, host) in hosts {
            if let Err(error) = host.shutdown() {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn request_all(&mut self, request: HostRequest) -> Result<Vec<ExtensionIntent>> {
        let mut intents = Vec::new();
        for (extension_id, host) in &mut self.hosts {
            let messages = host.request(&request)?;
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
}
