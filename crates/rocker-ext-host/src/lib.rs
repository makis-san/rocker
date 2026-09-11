//! Extension host supervisor (PLAN §5.5).
//!
//! Runs as a **separate supervised process** from the UI for crash isolation and
//! per-extension resource ceilings. This scaffold defines the capability gate
//! and the supervisor's public surface; the `rhai` runtime (Phase 5a) and the
//! `wasmtime` + Component Model runtime (Phase 5b) attach behind
//! [`ExtensionRuntime`].

use std::{collections::HashSet, path::Path, sync::Arc};

use rhai::{Engine, EvalAltResult, Position, Scope, AST};
use rocker_ext_api::{Capability, Manifest, ManifestError, Tier};
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

    engine.register_fn("notify", move |message: String| {
        gate.require(Capability::Notifications)
            .and_then(|()| host_api.notify(&message))
            .map_err(runtime_error)
    });
}

fn runtime_error(error: HostError) -> Box<EvalAltResult> {
    EvalAltResult::ErrorRuntime(error.to_string().into(), Position::NONE).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocker_ext_api::Tier;
    use std::sync::Mutex;

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
}
