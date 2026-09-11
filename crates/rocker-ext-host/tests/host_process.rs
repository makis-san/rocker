use std::fs;

use rocker_ext_api::{Capability, UiNode};
use rocker_ext_host::{
    ExtensionRegistry, ExtensionSettings, ExtensionSupervisor, HostMessage, HostProcess,
    HostRequest, NoDataSource,
};
use tempfile::tempdir;

#[test]
fn isolated_host_emits_intents_and_completes_requests() {
    let extension = tempdir().expect("extension directory is created");
    fs::write(
        extension.path().join("extension.toml"),
        r#"
            id = "example.process"
            name = "Process example"
            version = "0.1.0"
            tier = "script"
            capabilities = ["containers-read", "notifications"]
            entry = "main.rhai"
        "#,
    )
    .expect("manifest is written");
    fs::write(
        extension.path().join("main.rhai"),
        "fn activate() { containers_read(); notify(\"ready\"); }",
    )
    .expect("script is written");

    let mut host = HostProcess::spawn(
        env!("CARGO_BIN_EXE_rocker-ext-host"),
        extension.path(),
        &[Capability::ContainersRead, Capability::Notifications],
    )
    .expect("host process starts");
    let messages = host
        .request(&HostRequest::Activate, &NoDataSource)
        .expect("activation completes");

    assert_eq!(
        messages,
        [
            HostMessage::ContainersRead,
            HostMessage::Notify {
                text: "ready".into()
            },
            HostMessage::Completed {
                ok: true,
                error: None
            }
        ]
    );
    host.shutdown().expect("host process exits cleanly");
}

#[test]
fn supervisor_activates_enabled_extensions_and_returns_intents() {
    let source = tempdir().expect("source directory is created");
    let destination = tempdir().expect("destination directory is created");
    fs::write(
        source.path().join("extension.toml"),
        r#"
            id = "example.supervisor"
            name = "Supervisor example"
            version = "0.1.0"
            tier = "script"
            capabilities = ["notifications"]
            entry = "main.rhai"
        "#,
    )
    .expect("manifest is written");
    fs::write(
        source.path().join("main.rhai"),
        "fn activate() { notify(\"started\"); }",
    )
    .expect("script is written");

    let root = destination.path().join("extensions");
    let mut registry = ExtensionRegistry::load(&root).expect("registry loads");
    registry
        .install_from_dir(source.path())
        .expect("extension installs");
    registry
        .set_settings(
            "example.supervisor".into(),
            ExtensionSettings {
                enabled: true,
                dev_mode: false,
                granted_capabilities: vec![Capability::Notifications],
            },
        )
        .expect("settings save");

    let mut supervisor = ExtensionSupervisor::new(
        env!("CARGO_BIN_EXE_rocker-ext-host"),
        std::sync::Arc::new(NoDataSource),
    );
    supervisor
        .launch_enabled(registry.discover().expect("extensions discover"))
        .expect("extension host starts");

    assert_eq!(
        supervisor.activate_all().expect("activation succeeds"),
        [rocker_ext_host::ExtensionIntent {
            extension_id: "example.supervisor".into(),
            message: HostMessage::Notify {
                text: "started".into()
            }
        }]
    );
    supervisor.shutdown_all().expect("supervisor exits cleanly");
}

/// Escape arbitrary bytes as a WAT string literal using `\XX` hex escapes.
fn wat_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("\\{byte:02x}")).collect()
}

/// A minimal Component Model binary implementing the `rocker-extension`
/// world: `activate` is a no-op and `render-panel` ignores its argument and
/// returns the fixed `panel_json` string. Mirrors
/// `rocker_ext_host::component::tests::minimal_component`, duplicated here
/// because that helper is private to the library crate's own test module and
/// this integration test needs to drive a *real* isolated process, not the
/// in-process `ComponentRuntime`.
fn minimal_component(panel_json: &str) -> Vec<u8> {
    let json_bytes = panel_json.as_bytes();
    let json_offset: u32 = 1024;
    let json_len = json_bytes.len() as u32;

    let wat = format!(
        r#"(component
            (core module $m
                (memory (export "memory") 1)
                (global $heap (mut i32) (i32.const 2048))
                (data (i32.const 0) "{ptr_bytes}{len_bytes}")
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
                    (i32.const 0))
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

#[test]
fn component_extension_activates_and_renders_a_panel_through_the_isolated_host() {
    let source = tempdir().expect("source directory is created");
    let destination = tempdir().expect("destination directory is created");
    fs::write(
        source.path().join("extension.toml"),
        r#"
            id = "example.component"
            name = "Component example"
            version = "0.1.0"
            tier = "component"
            capabilities = []
            entry = "extension.wasm"
        "#,
    )
    .expect("manifest is written");
    fs::write(
        source.path().join("extension.wasm"),
        minimal_component(r#"{"node":"label","text":"hi from wasm"}"#),
    )
    .expect("component is written");

    let root = destination.path().join("extensions");
    let mut registry = ExtensionRegistry::load(&root).expect("registry loads");
    registry
        .install_from_dir(source.path())
        .expect("extension installs");
    registry
        .set_settings(
            "example.component".into(),
            ExtensionSettings {
                enabled: true,
                dev_mode: false,
                granted_capabilities: vec![],
            },
        )
        .expect("settings save");

    let mut supervisor = ExtensionSupervisor::new(
        env!("CARGO_BIN_EXE_rocker-ext-host"),
        std::sync::Arc::new(NoDataSource),
    );
    supervisor
        .launch_enabled(registry.discover().expect("extensions discover"))
        .expect("component extension host starts");

    assert_eq!(supervisor.activate_all().expect("activation succeeds"), []);

    assert_eq!(
        supervisor
            .render_panels("{}")
            .expect("panel render succeeds"),
        [rocker_ext_host::ExtensionIntent {
            extension_id: "example.component".into(),
            message: HostMessage::Ui {
                node: UiNode::Label {
                    text: "hi from wasm".into()
                }
            }
        }]
    );
    supervisor.shutdown_all().expect("supervisor exits cleanly");
}
