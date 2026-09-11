use std::fs;

use rocker_ext_api::Capability;
use rocker_ext_host::{HostMessage, HostProcess, HostRequest};
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
