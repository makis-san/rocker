//! Resolving a credential from a `docker-credential-*` helper binary
//! (PLAN §5.2): the same request the Docker CLI makes right before a pull or
//! push against a helper-backed registry.
//!
//! This is deliberately generic — it knows nothing about AWS, GCP, or any
//! other cloud provider. A host becomes `AuthType::Helper` because
//! [`crate::docker_config`] found it under `~/.docker/config.json`'s
//! `credHelpers` (or the global `credsStore`), which is also how a user sets
//! up AWS ECR: install `docker-credential-ecr-login`, point a `credHelpers`
//! glob at it, and `docker login`/`docker pull` already works with no
//! Rocker-specific setup. This module is what lets Rocker do the same thing
//! instead of requiring its own copy of the credential, native SDK, or
//! extension.

use std::io::Write;
use std::process::{Command, Stdio};

use thiserror::Error;

/// A credential a helper handed back for one host. Fetched fresh on every
/// call — helpers like `ecr-login` mint a new short-lived token per
/// invocation, so nothing here is cached by Rocker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperCredential {
    pub username: String,
    pub secret: String,
}

#[derive(Debug, Error)]
pub enum HelperError {
    #[error("docker-credential-{0} is not installed, or not on PATH")]
    NotInstalled(String),
    #[error("docker-credential-{0}: {1}")]
    Failed(String, String),
    #[error("docker-credential-{0} returned a response Rocker couldn't parse: {1}")]
    Malformed(String, String),
    #[error("docker-credential-{0}: {1}")]
    Io(String, String),
}

/// Run `docker-credential-<helper> get` for `host`.
pub fn fetch(helper: &str, host: &str) -> Result<HelperCredential, HelperError> {
    fetch_with_program(&format!("docker-credential-{helper}"), helper, host)
}

/// [`fetch`], with the program to run split out so a test can point it at a
/// fixture script instead of a real, installed credential helper.
fn fetch_with_program(
    program: &str,
    helper_name: &str,
    host: &str,
) -> Result<HelperCredential, HelperError> {
    let mut child = Command::new(program)
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => HelperError::NotInstalled(helper_name.to_string()),
            _ => HelperError::Io(helper_name.to_string(), error.to_string()),
        })?;

    // The helper protocol (docker/docker-credential-helpers) reads the
    // server URL as a single line on stdin and answers on stdout.
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(format!("{host}\n").as_bytes())
        .map_err(|error| HelperError::Io(helper_name.to_string(), error.to_string()))?;

    let output = child
        .wait_with_output()
        .map_err(|error| HelperError::Io(helper_name.to_string(), error.to_string()))?;

    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if message.is_empty() {
            format!("exited with {}", output.status)
        } else {
            message
        };
        return Err(HelperError::Failed(helper_name.to_string(), message));
    }

    let response: HelperResponse = serde_json::from_slice(&output.stdout)
        .map_err(|error| HelperError::Malformed(helper_name.to_string(), error.to_string()))?;

    Ok(HelperCredential {
        username: response.username,
        secret: response.secret,
    })
}

/// The helper protocol's response shape for `get`
/// (docker/docker-credential-helpers's `credentials.Credentials`).
#[derive(Debug, serde::Deserialize)]
struct HelperResponse {
    #[serde(rename = "Username")]
    username: String,
    #[serde(rename = "Secret")]
    secret: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// Write a fake helper script that echoes a fixed JSON response,
    /// regardless of what's piped to its stdin, and return its path.
    #[cfg(unix)]
    fn fake_helper(dir: &tempfile::TempDir, body: &str) -> std::path::PathBuf {
        write_script(dir, "fake-helper", &format!("#!/bin/sh\ncat <<'EOF'\n{body}\nEOF\n"))
    }

    /// Write and chmod an executable script, closing the write handle before
    /// returning. Some sandboxed filesystems briefly report a
    /// just-created-and-closed executable as busy (`ETXTBSY`) if it's exec'd
    /// immediately, so callers that spawn the result right away should
    /// tolerate one retry rather than this function papering over it.
    #[cfg(unix)]
    fn write_script(dir: &tempfile::TempDir, name: &str, contents: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        {
            let mut file = std::fs::File::create(&path).expect("fixture script is created");
            file.write_all(contents.as_bytes())
                .expect("fixture script is written");
            file.sync_all().expect("fixture script is flushed");
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("fixture script is made executable");
        path
    }

    /// [`fetch_with_program`], retrying past the sandboxed-filesystem
    /// `ETXTBSY` flake described on [`write_script`].
    #[cfg(unix)]
    fn fetch_with_retry(
        program: &str,
        helper_name: &str,
        host: &str,
    ) -> Result<HelperCredential, HelperError> {
        for attempt in 0..5 {
            match fetch_with_program(program, helper_name, host) {
                Err(HelperError::Io(_, message)) if message.contains("busy") && attempt < 4 => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                result => return result,
            }
        }
        unreachable!()
    }

    #[test]
    #[cfg(unix)]
    fn fetch_parses_a_helpers_credential() {
        let dir = tempfile::tempdir().expect("temp dir is created");
        let program = fake_helper(
            &dir,
            r#"{"ServerURL":"123456789012.dkr.ecr.us-east-1.amazonaws.com","Username":"AWS","Secret":"ecr-token"}"#,
        );

        let credential = fetch_with_retry(
            program.to_str().expect("fixture path is UTF-8"),
            "fake",
            "123456789012.dkr.ecr.us-east-1.amazonaws.com",
        )
        .expect("the fixture helper succeeds");

        assert_eq!(credential.username, "AWS");
        assert_eq!(credential.secret, "ecr-token");
    }

    #[test]
    fn a_missing_helper_binary_is_reported_as_not_installed() {
        let error = fetch_with_program(
            "docker-credential-this-does-not-exist-anywhere",
            "this-does-not-exist-anywhere",
            "example.com",
        )
        .expect_err("no such binary exists on PATH");

        assert!(matches!(error, HelperError::NotInstalled(name) if name == "this-does-not-exist-anywhere"));
    }

    #[test]
    #[cfg(unix)]
    fn a_nonzero_exit_is_reported_with_the_helpers_stderr() {
        let dir = tempfile::tempdir().expect("temp dir is created");
        let path = write_script(
            &dir,
            "failing-helper",
            "#!/bin/sh\necho 'credentials not found in native keychain' >&2\nexit 1\n",
        );

        let error =
            fetch_with_retry(path.to_str().expect("fixture path is UTF-8"), "fake", "example.com")
                .expect_err("the fixture helper fails");

        assert!(matches!(
            error,
            HelperError::Failed(name, message)
                if name == "fake" && message.contains("credentials not found")
        ));
    }

    #[test]
    #[cfg(unix)]
    fn a_malformed_response_is_reported_rather_than_panicking() {
        let dir = tempfile::tempdir().expect("temp dir is created");
        let program = fake_helper(&dir, "not json");

        let error = fetch_with_retry(
            program.to_str().expect("fixture path is UTF-8"),
            "fake",
            "example.com",
        )
        .expect_err("the fixture helper's output doesn't parse");

        assert!(matches!(error, HelperError::Malformed(name, _) if name == "fake"));
    }
}
