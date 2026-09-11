//! A real `/v2/` auth probe (PLAN §5.2): verify a registry credential
//! actually authenticates before the UI marks it "verified".
//!
//! Covers the two challenge shapes real v2 registries use: an OAuth2-ish
//! Bearer-token exchange (Docker Hub, GHCR, GitLab, quay.io, ...) per the
//! [Docker Registry token spec], and HTTP Basic sent straight to `/v2/`
//! (some self-hosted registries with no separate token service).
//! [`test_helper_connection`] covers a third case the same way: a
//! `Helper`-typed registry (AWS ECR via `ecr-login`, GCR, ...), whose
//! credential comes from [`crate::credential_helper`] instead of the
//! keychain.
//!
//! [Docker Registry token spec]: https://distribution.github.io/distribution/spec/auth/token/

use std::collections::HashMap;

use base64::Engine as _;
use ureq::http::StatusCode;

/// What a `WWW-Authenticate` challenge on a `/v2/` ping asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthChallenge {
    Bearer {
        realm: String,
        service: Option<String>,
        scope: Option<String>,
    },
    Basic,
}

/// Result of probing one registry with one credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionOutcome {
    /// `/v2/` answered 200 with no challenge at all — this host needs no
    /// credential for a bare ping.
    AnonymousOk,
    /// The credential authenticated, via Bearer token exchange or Basic.
    Authenticated,
    /// The registry understood the credential and rejected it.
    AuthFailed,
    /// `/v2/` responded, but with something this probe can't satisfy (an
    /// unrecognized challenge scheme, or an unexpected status).
    Unsupported { status: u16 },
    /// Couldn't complete the exchange (DNS, TCP, TLS, timeout, ...).
    NetworkError(String),
    /// A `Helper`-typed registry's `docker-credential-*` helper couldn't be
    /// run, or didn't hand back a usable credential (not installed, no
    /// credential stored for this host, malformed response, ...).
    HelperError(String),
}

/// Probe `host` with `username`/`password`. An empty `username`/`password`
/// is a valid probe of anonymous access (some registries and some scopes
/// allow it).
pub fn test_connection(host: &str, username: &str, password: &str) -> ConnectionOutcome {
    let base = v2_base_url(host);
    let ping_url = format!("{base}/v2/");

    let resp = match get(&ping_url, None) {
        Ok(r) => r,
        Err(e) => return ConnectionOutcome::NetworkError(e),
    };

    if resp.0.is_success() {
        return ConnectionOutcome::AnonymousOk;
    }
    if resp.0 != StatusCode::UNAUTHORIZED {
        return ConnectionOutcome::Unsupported {
            status: resp.0.as_u16(),
        };
    }

    match resp.1.and_then(|v| parse_www_authenticate(&v)) {
        Some(AuthChallenge::Basic) => basic_probe(&ping_url, username, password),
        Some(AuthChallenge::Bearer {
            realm,
            service,
            scope,
        }) => bearer_probe(
            &base,
            &realm,
            service.as_deref(),
            scope.as_deref(),
            username,
            password,
        ),
        None => ConnectionOutcome::Unsupported { status: 401 },
    }
}

/// Probe a `Helper`-typed registry: resolve a fresh credential from its
/// `docker-credential-<helper>` binary — the same request `docker login`/
/// `docker pull` make against a `credHelpers`-configured host — then run the
/// same `/v2/` auth check [`test_connection`] does with any other credential.
/// This is how a host set up for AWS ECR, GCR, or any other helper-backed
/// registry gets tested without Rocker ever storing a secret for it.
pub fn test_helper_connection(host: &str, helper: &str) -> ConnectionOutcome {
    match crate::credential_helper::fetch(helper, host) {
        Ok(credential) => test_connection(host, &credential.username, &credential.secret),
        Err(error) => ConnectionOutcome::HelperError(error.to_string()),
    }
}

/// Docker Hub's `config.json` key is the legacy v1 index host; the real v2
/// API lives elsewhere.
fn v2_base_url(host: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    if host.contains("index.docker.io") {
        "https://registry-1.docker.io".to_string()
    } else if host.starts_with("http://") || host.starts_with("https://") {
        host.to_string()
    } else {
        format!("https://{host}")
    }
}

fn basic_probe(ping_url: &str, username: &str, password: &str) -> ConnectionOutcome {
    let auth = basic_auth_header(username, password);
    match get(ping_url, Some(&auth)) {
        Ok((status, _)) if status.is_success() => ConnectionOutcome::Authenticated,
        Ok((status, _)) if status == StatusCode::UNAUTHORIZED => ConnectionOutcome::AuthFailed,
        Ok((status, _)) => ConnectionOutcome::Unsupported {
            status: status.as_u16(),
        },
        Err(e) => ConnectionOutcome::NetworkError(e),
    }
}

fn bearer_probe(
    base: &str,
    realm: &str,
    service: Option<&str>,
    scope: Option<&str>,
    username: &str,
    password: &str,
) -> ConnectionOutcome {
    let mut url = realm.to_string();
    let mut sep = if url.contains('?') { '&' } else { '?' };
    if let Some(s) = service {
        url.push(sep);
        url.push_str("service=");
        url.push_str(&percent_encode(s));
        sep = '&';
    }
    if let Some(sc) = scope {
        url.push(sep);
        url.push_str("scope=");
        url.push_str(&percent_encode(sc));
    }

    // Only send Basic credentials to the token endpoint if we have one —
    // an empty username/password probes anonymous token issuance, which
    // several public registries (e.g. Docker Hub, for an unscoped ping)
    // allow.
    let auth = (!username.is_empty() || !password.is_empty())
        .then(|| basic_auth_header(username, password));

    let body = match get_body(&url, auth.as_deref()) {
        Ok((status, _, body)) if status.is_success() => body,
        Ok((status, _, _))
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN =>
        {
            return ConnectionOutcome::AuthFailed;
        }
        Ok((status, _, _)) => {
            return ConnectionOutcome::Unsupported {
                status: status.as_u16(),
            };
        }
        Err(e) => return ConnectionOutcome::NetworkError(e),
    };

    let token = match serde_json::from_str::<TokenResponse>(&body) {
        Ok(t) => t.token.or(t.access_token),
        Err(_) => None,
    };
    let Some(token) = token else {
        return ConnectionOutcome::Unsupported { status: 200 };
    };

    // Retry the ping with the minted token: minting alone only proves the
    // token service accepted the credential, not that this registry does.
    let ping_url = format!("{base}/v2/");
    match get(&ping_url, Some(&format!("Bearer {token}"))) {
        Ok((status, _)) if status.is_success() => ConnectionOutcome::Authenticated,
        Ok((status, _))
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN =>
        {
            ConnectionOutcome::AuthFailed
        }
        Ok((status, _)) => ConnectionOutcome::Unsupported {
            status: status.as_u16(),
        },
        Err(e) => ConnectionOutcome::NetworkError(e),
    }
}

#[derive(Debug, serde::Deserialize)]
struct TokenResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

/// GET `url`, optionally with an `Authorization` header, treating every
/// status as a normal response rather than an error.
fn get(url: &str, authorization: Option<&str>) -> Result<(StatusCode, Option<String>), String> {
    let (status, header, _) = get_body(url, authorization)?;
    Ok((status, header))
}

fn get_body(
    url: &str,
    authorization: Option<&str>,
) -> Result<(StatusCode, Option<String>, String), String> {
    let mut req = ureq::get(url).config().http_status_as_error(false).build();
    if let Some(auth) = authorization {
        req = req.header("Authorization", auth);
    }
    let mut resp = req.call().map_err(|e| e.to_string())?;
    let status = resp.status();
    let challenge = resp
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    Ok((status, challenge, body))
}

fn basic_auth_header(username: &str, password: &str) -> String {
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    format!("Basic {encoded}")
}

/// Minimal percent-encoding, sufficient for the `service`/`scope` values a
/// `WWW-Authenticate: Bearer` challenge hands back (already token-shaped:
/// `repository:name:pull`, a hostname, ...). Not a general URL encoder.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Parse a `WWW-Authenticate` header value into the challenge it describes.
/// Returns `None` for a scheme this probe doesn't understand.
pub fn parse_www_authenticate(value: &str) -> Option<AuthChallenge> {
    let value = value.trim();
    if let Some(rest) = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
    {
        let params = parse_challenge_params(rest);
        let realm = params.get("realm")?.clone();
        Some(AuthChallenge::Bearer {
            realm,
            service: params.get("service").cloned(),
            scope: params.get("scope").cloned(),
        })
    } else if value.eq_ignore_ascii_case("Basic") || value.to_ascii_lowercase().starts_with("basic")
    {
        Some(AuthChallenge::Basic)
    } else {
        None
    }
}

/// Parse `key1="val1", key2="val2"` challenge parameters.
fn parse_challenge_params(s: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for part in split_unquoted_commas(s) {
        if let Some((k, v)) = part.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    out
}

/// Split on commas that are not inside a double-quoted value.
fn split_unquoted_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                cur.push(c);
            }
            ',' if !in_quotes => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_bearer_challenge_with_service_and_scope() {
        let value = r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/alpine:pull""#;
        assert_eq!(
            parse_www_authenticate(value),
            Some(AuthChallenge::Bearer {
                realm: "https://auth.docker.io/token".to_string(),
                service: Some("registry.docker.io".to_string()),
                scope: Some("repository:library/alpine:pull".to_string()),
            })
        );
    }

    #[test]
    fn parses_a_bearer_challenge_with_only_realm() {
        let value = r#"Bearer realm="https://ghcr.io/token""#;
        assert_eq!(
            parse_www_authenticate(value),
            Some(AuthChallenge::Bearer {
                realm: "https://ghcr.io/token".to_string(),
                service: None,
                scope: None,
            })
        );
    }

    #[test]
    fn parses_a_basic_challenge() {
        assert_eq!(
            parse_www_authenticate(r#"Basic realm="Registry Realm""#),
            Some(AuthChallenge::Basic)
        );
        assert_eq!(parse_www_authenticate("Basic"), Some(AuthChallenge::Basic));
    }

    #[test]
    fn rejects_an_unrecognized_scheme() {
        assert_eq!(parse_www_authenticate("Digest realm=\"x\""), None);
    }

    #[test]
    fn v2_base_url_maps_docker_hubs_legacy_v1_host() {
        assert_eq!(
            v2_base_url("https://index.docker.io/v1/"),
            "https://registry-1.docker.io"
        );
    }

    #[test]
    fn v2_base_url_defaults_to_https_for_a_bare_host() {
        assert_eq!(v2_base_url("ghcr.io"), "https://ghcr.io");
    }

    #[test]
    fn v2_base_url_keeps_an_explicit_scheme() {
        assert_eq!(
            v2_base_url("http://localhost:5000"),
            "http://localhost:5000"
        );
    }

    #[test]
    fn basic_auth_header_is_rfc7617_shaped() {
        // "octo:hunter2" base64-encoded.
        assert_eq!(
            basic_auth_header("octo", "hunter2"),
            "Basic b2N0bzpodW50ZXIy"
        );
    }

    #[test]
    fn percent_encode_leaves_scope_tokens_readable_and_escapes_the_rest() {
        assert_eq!(
            percent_encode("repository:library/alpine:pull"),
            "repository%3Alibrary%2Falpine%3Apull"
        );
    }

    /// Real registries need real network access, which CI runners either
    /// lack or shouldn't be hammering on every run. Run manually with
    /// `cargo test -p rocker-secrets -- --ignored`.
    #[test]
    #[ignore = "hits a real registry over the network"]
    fn docker_hub_anonymous_ping_authenticates() {
        // An unscoped `/v2/` ping needs no credential: Docker Hub's token
        // service issues an anonymous token for it.
        let outcome = test_connection("https://index.docker.io/v1/", "", "");
        assert_eq!(outcome, ConnectionOutcome::Authenticated);
    }

    /// Unlike Docker Hub, GHCR's token service refuses to mint a token for
    /// an unscoped ping with no credential — this exercises the same Bearer
    /// round trip end to end, just landing on the `AuthFailed` branch
    /// instead of `Authenticated`.
    #[test]
    #[ignore = "hits a real registry over the network"]
    fn ghcr_anonymous_ping_is_rejected_by_its_token_service() {
        let outcome = test_connection("ghcr.io", "", "");
        assert_eq!(outcome, ConnectionOutcome::AuthFailed);
    }

    #[test]
    #[ignore = "hits a real registry over the network"]
    fn unreachable_host_is_a_network_error() {
        let outcome = test_connection(
            "this-host-does-not-resolve.rocker-secrets-test.invalid",
            "",
            "",
        );
        assert!(matches!(outcome, ConnectionOutcome::NetworkError(_)));
    }

    /// No fixture needed, and no network touched: a helper binary that isn't
    /// installed fails before `test_connection` would ever run, so this
    /// exercises the `HelperError` branch without `#[ignore]`.
    #[test]
    fn a_missing_credential_helper_is_a_helper_error() {
        let outcome =
            test_helper_connection("example.com", "this-does-not-exist-anywhere");
        assert!(matches!(outcome, ConnectionOutcome::HelperError(_)));
    }
}
