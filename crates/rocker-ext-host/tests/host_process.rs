use std::fs;

use rocker_ext_api::Capability;
use rocker_ext_host::{
    ExtensionRegistry, ExtensionSettings, ExtensionSupervisor, HostMessage, HostProcess,
    HostRequest,
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
        .request(&HostRequest::Activate)
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

    let mut supervisor = ExtensionSupervisor::new(env!("CARGO_BIN_EXE_rocker-ext-host"));
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
