//! Wasmtime Component Model runtime for Tier 2 extensions.

use std::{collections::HashMap, path::Path, sync::Arc};

use rocker_ext_api::{Capability, ContainerAction, Manifest, Tier, UiNode};
use wasmtime::{
    component::{Component, HasSelf, Linker},
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder,
};

use crate::{CapabilityGate, HostError, Result};

wasmtime::component::bindgen!({
    world: "rocker-extension",
    path: "../../wit",
    imports: { default: trappable },
});

/// A container exposed through the component host API.
///
/// Also carried over the isolated-process protocol as the answer to a
/// [`crate::HostQuery::ListContainers`] query, so it round-trips as JSON.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComponentContainer {
    /// Docker container identifier.
    pub id: String,
    /// Human-readable container name.
    pub name: String,
    /// Image reference used by the container.
    pub image: String,
    /// Stable lifecycle state.
    pub state: String,
    /// Docker's human-readable status text.
    pub status: String,
}

/// Capability-gated operations supplied by the Rocker application.
pub trait ComponentHostApi: Send + Sync + 'static {
    /// Return the containers visible to this extension's host scope.
    fn list_containers(&self) -> Result<Vec<ComponentContainer>>;
    /// Ask Rocker to run a lifecycle operation.
    fn lifecycle(&self, container: &str, action: ContainerAction) -> Result<()>;
    /// Return a bounded tail of container logs.
    fn logs_tail(&self, container: &str, lines: u32) -> Result<Vec<String>>;
    /// Display an application-owned notification.
    fn notify(&self, level: ToastLevel, text: &str) -> Result<()>;
}

/// Notification severity used by component extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Warn,
    Error,
}

/// Resource limits applied to each component instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentLimits {
    /// Wasmtime fuel replenished before each exported call.
    pub fuel: u64,
    /// Maximum linear-memory bytes owned by the component store.
    pub memory_bytes: usize,
    /// Maximum elements across component tables.
    pub table_elements: usize,
}

impl Default for ComponentLimits {
    fn default() -> Self {
        Self {
            fuel: 10_000_000,
            memory_bytes: 64 * 1024 * 1024,
            table_elements: 10_000,
        }
    }
}

struct ComponentState {
    gate: CapabilityGate,
    api: Arc<dyn ComponentHostApi>,
    storage: HashMap<String, String>,
    limits: StoreLimits,
}

impl rocker::extension::types::Host for ComponentState {}

impl rocker::extension::containers::Host for ComponentState {
    fn list_containers(&mut self) -> wasmtime::Result<Vec<rocker::extension::types::Container>> {
        self.gate.require(Capability::ContainersRead)?;
        self.api
            .list_containers()?
            .into_iter()
            .map(|container| {
                Ok(rocker::extension::types::Container {
                    id: container.id,
                    name: container.name,
                    image: container.image,
                    state: container.state,
                    status: container.status,
                })
            })
            .collect()
    }

    fn lifecycle(
        &mut self,
        id: String,
        action: rocker::extension::types::LifecycleAction,
    ) -> wasmtime::Result<std::result::Result<(), String>> {
        self.gate.require(Capability::ContainersLifecycle)?;
        let action = match action {
            rocker::extension::types::LifecycleAction::Start => ContainerAction::Start,
            rocker::extension::types::LifecycleAction::Stop => ContainerAction::Stop,
            rocker::extension::types::LifecycleAction::Restart => ContainerAction::Restart,
            rocker::extension::types::LifecycleAction::Pause => ContainerAction::Pause,
            rocker::extension::types::LifecycleAction::Unpause => ContainerAction::Unpause,
            rocker::extension::types::LifecycleAction::Kill => ContainerAction::Kill,
        };
        Ok(self
            .api
            .lifecycle(&id, action)
            .map_err(|error| error.to_string()))
    }

    fn logs_tail(&mut self, id: String, lines: u32) -> wasmtime::Result<Vec<String>> {
        self.gate.require(Capability::LogsRead)?;
        Ok(self.api.logs_tail(&id, lines)?)
    }
}

impl rocker::extension::storage::Host for ComponentState {
    fn get(&mut self, key: String) -> wasmtime::Result<Option<String>> {
        self.gate.require(Capability::Storage)?;
        Ok(self.storage.get(&key).cloned())
    }

    fn set(&mut self, key: String, value: String) -> wasmtime::Result<()> {
        self.gate.require(Capability::Storage)?;
        self.storage.insert(key, value);
        Ok(())
    }

    fn delete(&mut self, key: String) -> wasmtime::Result<()> {
        self.gate.require(Capability::Storage)?;
        self.storage.remove(&key);
        Ok(())
    }
}

impl rocker::extension::notify::Host for ComponentState {
    fn toast(
        &mut self,
        level: rocker::extension::types::ToastLevel,
        text: String,
    ) -> wasmtime::Result<()> {
        self.gate.require(Capability::Notifications)?;
        let level = match level {
            rocker::extension::types::ToastLevel::Info => ToastLevel::Info,
            rocker::extension::types::ToastLevel::Warn => ToastLevel::Warn,
            rocker::extension::types::ToastLevel::Error => ToastLevel::Error,
        };
        Ok(self.api.notify(level, &text)?)
    }
}

impl rocker::extension::events::Host for ComponentState {
    fn subscribe(&mut self, _kinds: Vec<String>) -> wasmtime::Result<()> {
        self.gate.require(Capability::ContainersRead)?;
        Ok(())
    }
}

/// A compiled and instantiated Tier 2 extension.
pub struct ComponentRuntime {
    store: Store<ComponentState>,
    bindings: RockerExtension,
    fuel_per_call: u64,
}

impl ComponentRuntime {
    /// Load and instantiate a component from one extension directory.
    pub fn load(
        extension_dir: impl AsRef<Path>,
        granted: impl IntoIterator<Item = Capability>,
        host_api: Arc<dyn ComponentHostApi>,
    ) -> Result<Self> {
        let extension_dir = std::fs::canonicalize(extension_dir)?;
        let manifest = crate::read_manifest(&extension_dir)?;
        if manifest.tier != Tier::Component {
            return Err(HostError::WrongTier {
                id: manifest.id,
                actual: manifest.tier,
            });
        }
        let entry = manifest
            .entry
            .as_deref()
            .expect("validated above: component tier always has an entry");
        let entry_path = std::fs::canonicalize(extension_dir.join(entry))?;
        if !entry_path.starts_with(&extension_dir) {
            return Err(HostError::EntryOutsideInstall);
        }
        let bytes = std::fs::read(entry_path)?;
        Self::compile(
            &manifest,
            &bytes,
            granted,
            host_api,
            ComponentLimits::default(),
        )
    }

    /// Compile and instantiate component bytes with explicit resource limits.
    pub fn compile(
        manifest: &Manifest,
        bytes: &[u8],
        granted: impl IntoIterator<Item = Capability>,
        host_api: Arc<dyn ComponentHostApi>,
        limits: ComponentLimits,
    ) -> Result<Self> {
        manifest.validate()?;
        if manifest.tier != Tier::Component {
            return Err(HostError::WrongTier {
                id: manifest.id.clone(),
                actual: manifest.tier,
            });
        }

        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(runtime_error)?;
        let component = Component::new(&engine, bytes).map_err(runtime_error)?;
        let mut linker = Linker::new(&engine);
        RockerExtension::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(runtime_error)?;

        let store_limits = StoreLimitsBuilder::new()
            .memory_size(limits.memory_bytes)
            .table_elements(limits.table_elements)
            .build();
        let state = ComponentState {
            gate: CapabilityGate::new(manifest, granted),
            api: host_api,
            storage: HashMap::new(),
            limits: store_limits,
        };
        let mut store = Store::new(&engine, state);
        store.limiter(|state| &mut state.limits);
        store.set_fuel(limits.fuel).map_err(runtime_error)?;
        let bindings =
            RockerExtension::instantiate(&mut store, &component, &linker).map_err(runtime_error)?;

        Ok(Self {
            store,
            bindings,
            fuel_per_call: limits.fuel,
        })
    }

    /// Render the component's declarative panel and validate its JSON tree.
    pub fn render_panel(&mut self, context: &str) -> Result<UiNode> {
        self.refuel()?;
        let json = self
            .bindings
            .call_render_panel(&mut self.store, context)
            .map_err(runtime_error)?;
        serde_json::from_str(&json).map_err(|error| HostError::Runtime(error.to_string()))
    }

    fn refuel(&mut self) -> Result<()> {
        self.store
            .set_fuel(self.fuel_per_call)
            .map_err(runtime_error)
    }
}

impl crate::ExtensionRuntime for ComponentRuntime {
    fn activate(&mut self) -> Result<()> {
        self.refuel()?;
        self.bindings
            .call_activate(&mut self.store)
            .map_err(runtime_error)
    }
}

fn runtime_error(error: impl std::fmt::Display) -> HostError {
    HostError::Runtime(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExtensionRuntime;
    use rocker_ext_api::{Capability, Tier};

    fn manifest(caps: Vec<Capability>) -> Manifest {
        Manifest {
            id: "example.component".into(),
            name: "Example".into(),
            version: "0.1.0".into(),
            tier: Tier::Component,
            capabilities: caps,
            entry: Some("extension.wasm".into()),
            schedule_seconds: None,
            theme_variants: Vec::new(),
        }
    }

    /// Escape arbitrary bytes as a WAT string literal using `\XX` hex escapes,
    /// so the JSON payload never has to be manually re-encoded by hand.
    fn wat_bytes(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("\\{byte:02x}")).collect()
    }

    /// Build a minimal Component Model binary implementing the
    /// `rocker-extension` world: `activate` is a no-op, and `render-panel`
    /// ignores its argument and returns the fixed `panel_json` string. A tiny
    /// bump allocator backs the canonical ABI's `realloc` so the host can copy
    /// the `ctx` argument into the guest's linear memory.
    fn minimal_component(panel_json: &str) -> Vec<u8> {
        let json_bytes = panel_json.as_bytes();
        let json_offset: u32 = 1024;
        let json_len = json_bytes.len() as u32;
        let return_area_offset: u32 = 0;

        let wat = format!(
            r#"(component
                (core module $m
                    (memory (export "memory") 1)
                    (global $heap (mut i32) (i32.const 2048))
                    (data (i32.const {return_area_offset}) "{ptr_bytes}{len_bytes}")
                    (data (i32.const {json_offset}) "{json_escaped}")
                    (func (export "cabi_realloc")
                        (param $orig_ptr i32) (param $orig_size i32)
                        (param $align i32) (param $new_size i32) (result i32)
                        (local $ret i32)
                        (local.set $ret (global.get $heap))
                        (global.set $heap (i32.add (global.get $heap) (local.get $new_size)))
                        (local.get $ret))
                    (func (export "activate"))
                    (func (export "render_panel") (param i32 i32) (result i32)
                        (i32.const {return_area_offset}))
                )
                (core instance $i (instantiate $m))
                (func (export "activate")
                    (canon lift (core func $i "activate")))
                (func (export "render-panel") (param "ctx" string) (result string)
                    (canon lift (core func $i "render_panel")
                        (memory (core memory $i "memory"))
                        (realloc (core func $i "cabi_realloc"))))
            )"#,
            ptr_bytes = wat_bytes(&json_offset.to_le_bytes()),
            len_bytes = wat_bytes(&json_len.to_le_bytes()),
            json_escaped = wat_bytes(json_bytes),
        );

        wat::parse_str(wat).expect("hand-authored test component parses")
    }

    struct StubHostApi;

    impl ComponentHostApi for StubHostApi {
        fn list_containers(&self) -> Result<Vec<ComponentContainer>> {
            Ok(Vec::new())
        }

        fn lifecycle(&self, _container: &str, _action: ContainerAction) -> Result<()> {
            Ok(())
        }

        fn logs_tail(&self, _container: &str, _lines: u32) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        fn notify(&self, _level: ToastLevel, _text: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn component_runtime_activates_and_renders_a_declarative_panel() {
        let bytes = minimal_component(r#"{"node":"label","text":"hi"}"#);
        let manifest = manifest(vec![]);
        let mut runtime = ComponentRuntime::compile(
            &manifest,
            &bytes,
            [],
            Arc::new(StubHostApi),
            ComponentLimits::default(),
        )
        .expect("minimal component compiles and instantiates");

        runtime.activate().expect("activate runs");

        assert_eq!(
            runtime.render_panel("{}").expect("panel renders"),
            UiNode::Label { text: "hi".into() }
        );
    }

    #[test]
    fn component_runtime_rejects_a_script_tier_manifest() {
        let mut script_manifest = manifest(vec![]);
        script_manifest.tier = Tier::Script;
        let bytes = minimal_component("{}");

        let result = ComponentRuntime::compile(
            &script_manifest,
            &bytes,
            [],
            Arc::new(StubHostApi),
            ComponentLimits::default(),
        );

        assert!(matches!(result, Err(HostError::WrongTier { .. })));
    }

    // The tests below call `ComponentState`'s `Host` trait implementations
    // directly, bypassing wasmtime and any wasm bytes entirely. They exercise
    // the capability-gate + host-API dispatch logic that a real component
    // would trigger through an import call, without the risk or ceremony of
    // hand-authoring canonical-ABI WAT for every capability. The end-to-end
    // wasm path (activate + render-panel, above) already proves that a real
    // guest can drive `ComponentState` through wasmtime.

    #[derive(Default)]
    struct RecordingApi {
        calls: std::sync::Mutex<Vec<String>>,
        containers: Vec<ComponentContainer>,
        logs: Vec<String>,
    }

    impl RecordingApi {
        fn new(containers: Vec<ComponentContainer>, logs: Vec<String>) -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                containers,
                logs,
            }
        }
    }

    impl ComponentHostApi for RecordingApi {
        fn list_containers(&self) -> Result<Vec<ComponentContainer>> {
            self.calls
                .lock()
                .expect("lock")
                .push("list_containers".into());
            Ok(self.containers.clone())
        }

        fn lifecycle(&self, container: &str, action: ContainerAction) -> Result<()> {
            self.calls
                .lock()
                .expect("lock")
                .push(format!("lifecycle:{container}:{action:?}"));
            Ok(())
        }

        fn logs_tail(&self, container: &str, lines: u32) -> Result<Vec<String>> {
            self.calls
                .lock()
                .expect("lock")
                .push(format!("logs_tail:{container}:{lines}"));
            Ok(self.logs.clone())
        }

        fn notify(&self, level: ToastLevel, text: &str) -> Result<()> {
            self.calls
                .lock()
                .expect("lock")
                .push(format!("notify:{level:?}:{text}"));
            Ok(())
        }
    }

    fn state(
        requested: Vec<Capability>,
        granted: Vec<Capability>,
        api: Arc<dyn ComponentHostApi>,
    ) -> ComponentState {
        ComponentState {
            gate: CapabilityGate::new(&manifest(requested), granted),
            api,
            storage: HashMap::new(),
            limits: StoreLimitsBuilder::new().build(),
        }
    }

    #[test]
    fn host_dispatch_denies_list_containers_without_the_capability() {
        let mut host = state(vec![], vec![], Arc::new(RecordingApi::default()));

        assert!(rocker::extension::containers::Host::list_containers(&mut host).is_err());
    }

    #[test]
    fn host_dispatch_lists_containers_when_granted() {
        let container = ComponentContainer {
            id: "abc123".into(),
            name: "web".into(),
            image: "nginx".into(),
            state: "running".into(),
            status: "Up 2 minutes".into(),
        };
        let api = Arc::new(RecordingApi::new(vec![container], Vec::new()));
        let mut host = state(
            vec![Capability::ContainersRead],
            vec![Capability::ContainersRead],
            api.clone(),
        );

        let containers = rocker::extension::containers::Host::list_containers(&mut host)
            .expect("granted call succeeds");

        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].id, "abc123");
        assert_eq!(containers[0].name, "web");
        assert_eq!(*api.calls.lock().expect("lock"), ["list_containers"]);
    }

    #[test]
    fn host_dispatch_routes_granted_lifecycle_actions() {
        let api = Arc::new(RecordingApi::default());
        let mut host = state(
            vec![Capability::ContainersLifecycle],
            vec![Capability::ContainersLifecycle],
            api.clone(),
        );

        let outcome = rocker::extension::containers::Host::lifecycle(
            &mut host,
            "abc123".into(),
            rocker::extension::types::LifecycleAction::Restart,
        )
        .expect("host call does not trap");

        assert_eq!(outcome, Ok(()));
        assert_eq!(
            *api.calls.lock().expect("lock"),
            ["lifecycle:abc123:Restart"]
        );
    }

    #[test]
    fn host_dispatch_denies_lifecycle_without_the_capability() {
        let mut host = state(vec![], vec![], Arc::new(RecordingApi::default()));

        let result = rocker::extension::containers::Host::lifecycle(
            &mut host,
            "abc123".into(),
            rocker::extension::types::LifecycleAction::Kill,
        );

        assert!(result.is_err());
    }

    #[test]
    fn host_dispatch_tails_logs_when_granted() {
        let api = Arc::new(RecordingApi::new(Vec::new(), vec!["line one".into()]));
        let mut host = state(
            vec![Capability::LogsRead],
            vec![Capability::LogsRead],
            api.clone(),
        );

        let lines = rocker::extension::containers::Host::logs_tail(&mut host, "abc123".into(), 10)
            .expect("granted call succeeds");

        assert_eq!(lines, ["line one"]);
        assert_eq!(*api.calls.lock().expect("lock"), ["logs_tail:abc123:10"]);
    }

    #[test]
    fn host_dispatch_round_trips_storage_when_granted() {
        let mut host = state(
            vec![Capability::Storage],
            vec![Capability::Storage],
            Arc::new(RecordingApi::default()),
        );

        rocker::extension::storage::Host::set(&mut host, "k".into(), "v".into())
            .expect("set succeeds");
        assert_eq!(
            rocker::extension::storage::Host::get(&mut host, "k".into()).expect("get succeeds"),
            Some("v".into())
        );

        rocker::extension::storage::Host::delete(&mut host, "k".into()).expect("delete succeeds");
        assert_eq!(
            rocker::extension::storage::Host::get(&mut host, "k".into()).expect("get succeeds"),
            None
        );
    }

    #[test]
    fn host_dispatch_denies_storage_without_the_capability() {
        let mut host = state(vec![], vec![], Arc::new(RecordingApi::default()));

        assert!(rocker::extension::storage::Host::set(&mut host, "k".into(), "v".into()).is_err());
    }

    #[test]
    fn host_dispatch_toasts_when_granted() {
        let api = Arc::new(RecordingApi::default());
        let mut host = state(
            vec![Capability::Notifications],
            vec![Capability::Notifications],
            api.clone(),
        );

        rocker::extension::notify::Host::toast(
            &mut host,
            rocker::extension::types::ToastLevel::Warn,
            "disk almost full".into(),
        )
        .expect("granted call succeeds");

        assert_eq!(
            *api.calls.lock().expect("lock"),
            ["notify:Warn:disk almost full"]
        );
    }

    #[test]
    fn host_dispatch_denies_events_subscribe_without_the_capability() {
        let mut host = state(vec![], vec![], Arc::new(RecordingApi::default()));

        assert!(
            rocker::extension::events::Host::subscribe(&mut host, vec!["start".into()]).is_err()
        );
    }
}
