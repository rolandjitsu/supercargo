//! Native Docker Registry HTTP API v2 probe.
//!
//! Existence is a `HEAD` on `/v2/<repository>/manifests/<reference>`: 200
//! means the manifest exists, 404 means it does not. A 401 carrying a
//! `WWW-Authenticate: Bearer` challenge triggers the standard token exchange
//! and one retry. Credentials come from the files docker and podman already
//! maintain -- `~/.docker/config.json` and the containers `auth.json` --
//! including `credHelpers`/`credsStore` entries, which are resolved by running
//! the matching `docker-credential-<name>` helper (that is how OS keychains
//! and credential stores are reached).

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;

use crate::registry::RegistryProbe;
use crate::{Error, Result};

/// Registry probe over plain HTTPS (HTTP for localhost registries).
pub struct HttpProbe {
    auth_files: Vec<PathBuf>,
    agent: ureq::Agent,
}

impl HttpProbe {
    /// Probe reading credentials from the standard locations, most specific
    /// first: `$DOCKER_CONFIG/config.json`, `~/.docker/config.json`,
    /// `$REGISTRY_AUTH_FILE`, `$XDG_RUNTIME_DIR/containers/auth.json`, and
    /// `~/.config/containers/auth.json`.
    pub fn from_env() -> Self {
        let mut files = Vec::new();
        if let Some(dir) = std::env::var_os("DOCKER_CONFIG") {
            files.push(PathBuf::from(dir).join("config.json"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            files.push(PathBuf::from(&home).join(".docker/config.json"));
        }
        if let Some(path) = std::env::var_os("REGISTRY_AUTH_FILE") {
            files.push(PathBuf::from(path));
        }
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            files.push(PathBuf::from(dir).join("containers/auth.json"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            files.push(PathBuf::from(home).join(".config/containers/auth.json"));
        }
        Self::with_auth_files(files)
    }

    /// Probe reading credentials only from the given auth files (docker
    /// `config.json` format), in order.
    pub fn with_auth_files(auth_files: Vec<PathBuf>) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build();
        Self {
            auth_files,
            agent: config.new_agent(),
        }
    }

    fn head_manifest(
        &self,
        url: &str,
        authorization: Option<&str>,
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let mut req = self.agent.head(url).header(
            "Accept",
            "application/vnd.oci.image.index.v1+json, \
             application/vnd.oci.image.manifest.v1+json, \
             application/vnd.docker.distribution.manifest.list.v2+json, \
             application/vnd.docker.distribution.manifest.v2+json",
        );
        if let Some(auth) = authorization {
            req = req.header("Authorization", auth);
        }
        req.call().map_err(|e| registry_error(url, &e.to_string()))
    }

    /// Exchange a `Bearer` challenge for a token (RFC 6750-style flow used by
    /// registries), passing Basic credentials to the token endpoint when
    /// available.
    fn fetch_token(
        &self,
        challenge: &BearerChallenge,
        image: &ImageRef,
        basic: Option<&str>,
    ) -> Result<String> {
        let scope = challenge
            .scope
            .clone()
            .unwrap_or_else(|| format!("repository:{}:pull", image.repository));
        let mut url = format!("{}?scope={scope}", challenge.realm);
        if let Some(service) = &challenge.service {
            url.push_str(&format!("&service={service}"));
        }
        let mut req = self.agent.get(&url);
        if let Some(basic) = basic {
            req = req.header("Authorization", basic);
        }
        let mut resp = req
            .call()
            .map_err(|e| registry_error(&url, &e.to_string()))?;
        if resp.status().as_u16() != 200 {
            return Err(registry_error(
                &url,
                &format!("token endpoint returned {}", resp.status()),
            ));
        }
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| registry_error(&url, &e.to_string()))?;
        let token: TokenResponse =
            serde_json::from_str(&body).map_err(|e| registry_error(&url, &e.to_string()))?;
        token
            .token
            .or(token.access_token)
            .ok_or_else(|| registry_error(&url, "token response carried no token"))
    }

    fn basic_auth_for(&self, image: &ImageRef) -> Option<String> {
        let (user, pass) = credentials_from_files(&self.auth_files, &image.registry)?;
        Some(format!("Basic {}", BASE64.encode(format!("{user}:{pass}"))))
    }
}

impl RegistryProbe for HttpProbe {
    fn exists(&self, image_ref: &str) -> Result<bool> {
        let image = parse_image_ref(image_ref)?;
        let url = format!(
            "{}/v2/{}/manifests/{}",
            base_url(&image.registry),
            image.repository,
            image.reference
        );
        let basic = self.basic_auth_for(&image);

        let resp = self.head_manifest(&url, basic.as_deref())?;
        match resp.status().as_u16() {
            200 => return Ok(true),
            404 => return Ok(false),
            401 => {}
            code => return Err(registry_error(&url, &format!("unexpected status {code}"))),
        }

        // 401: follow the Bearer challenge once. Anonymous pulls work this way
        // too (public images get a token without credentials).
        let challenge = resp
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_bearer_challenge)
            .ok_or_else(|| registry_error(&url, "401 without a usable bearer challenge"))?;
        let token = self.fetch_token(&challenge, &image, basic.as_deref())?;
        let resp = self.head_manifest(&url, Some(&format!("Bearer {token}")))?;
        match resp.status().as_u16() {
            200 => Ok(true),
            404 => Ok(false),
            code => Err(registry_error(
                &url,
                &format!("status {code} after token exchange (check registry credentials)"),
            )),
        }
    }
}

/// One parsed image reference.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ImageRef {
    /// Registry host (and port), e.g. `ghcr.io` or `localhost:5000`.
    pub registry: String,
    /// Repository path, e.g. `acme/ingest` or `library/alpine`.
    pub repository: String,
    /// Tag or digest.
    pub reference: String,
}

/// Split `[registry/]repository[:tag][@digest]` the way container tools do:
/// the first segment is a registry only when it contains a dot or colon or is
/// `localhost`; bare names go to Docker Hub with the `library/` namespace.
pub(crate) fn parse_image_ref(image_ref: &str) -> Result<ImageRef> {
    let bad = |message: &str| Error::Registry {
        image: image_ref.to_owned(),
        message: message.to_owned(),
    };

    let (name, reference) = match image_ref.split_once('@') {
        Some((name, digest)) => (name, digest.to_owned()),
        None => {
            let slash = image_ref.rfind('/').map_or(0, |i| i + 1);
            match image_ref[slash..].rsplit_once(':') {
                Some(_) => {
                    let colon = image_ref.rfind(':').expect("tag separator present");
                    (&image_ref[..colon], image_ref[colon + 1..].to_owned())
                }
                None => (image_ref, "latest".to_owned()),
            }
        }
    };
    if name.is_empty() || reference.is_empty() {
        return Err(bad("empty name or reference"));
    }

    let (registry, repository) = match name.split_once('/') {
        Some((host, rest)) if host.contains('.') || host.contains(':') || host == "localhost" => {
            (host.to_owned(), rest.to_owned())
        }
        _ => ("docker.io".to_owned(), name.to_owned()),
    };
    let registry = match registry.as_str() {
        "docker.io" | "index.docker.io" => "registry-1.docker.io".to_owned(),
        other => other.to_owned(),
    };
    let repository = if registry == "registry-1.docker.io" && !repository.contains('/') {
        format!("library/{repository}")
    } else {
        repository
    };
    if repository.is_empty() {
        return Err(bad("empty repository"));
    }
    Ok(ImageRef {
        registry,
        repository,
        reference,
    })
}

/// Plain HTTP for loopback registries (the docker daemon treats those as
/// insecure by default); HTTPS everywhere else.
fn base_url(registry: &str) -> String {
    let host = registry.split(':').next().unwrap_or(registry);
    if host == "localhost" || host == "127.0.0.1" || host == "::1" {
        format!("http://{registry}")
    } else {
        format!("https://{registry}")
    }
}

#[derive(Debug, PartialEq, Eq)]
struct BearerChallenge {
    realm: String,
    service: Option<String>,
    scope: Option<String>,
}

fn parse_bearer_challenge(header: &str) -> Option<BearerChallenge> {
    let rest = header.strip_prefix("Bearer ")?;
    let mut fields: HashMap<&str, String> = HashMap::new();
    for part in rest.split(',') {
        if let Some((key, value)) = part.trim().split_once('=') {
            fields.insert(key.trim(), value.trim().trim_matches('"').to_owned());
        }
    }
    Some(BearerChallenge {
        realm: fields.remove("realm")?,
        service: fields.remove("service"),
        scope: fields.remove("scope"),
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

/// The subset of docker's `config.json` (and the identical containers
/// `auth.json`) the probe reads.
#[derive(Debug, Default, Deserialize)]
struct AuthFile {
    #[serde(default)]
    auths: HashMap<String, AuthEntry>,
    #[serde(default, rename = "credHelpers")]
    cred_helpers: HashMap<String, String>,
    #[serde(default, rename = "credsStore")]
    creds_store: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AuthEntry {
    #[serde(default)]
    auth: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

/// Keys under which a registry may appear in an auth file. Docker Hub has
/// accumulated several historical spellings.
fn registry_keys(registry: &str) -> Vec<String> {
    let mut keys = vec![
        registry.to_owned(),
        format!("https://{registry}"),
        format!("http://{registry}"),
    ];
    if registry == "registry-1.docker.io" {
        for k in [
            "https://index.docker.io/v1/",
            "index.docker.io",
            "docker.io",
        ] {
            keys.push(k.to_owned());
        }
    }
    keys
}

fn credentials_from_files(files: &[PathBuf], registry: &str) -> Option<(String, String)> {
    for file in files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let Ok(config) = serde_json::from_str::<AuthFile>(&text) else {
            continue;
        };
        if let Some(creds) = credentials_from_config(&config, registry) {
            return Some(creds);
        }
    }
    None
}

/// Resolution order within one file, matching docker: a registry-specific
/// credential helper, then the store-wide helper, then inline `auths`.
fn credentials_from_config(config: &AuthFile, registry: &str) -> Option<(String, String)> {
    let keys = registry_keys(registry);

    for key in &keys {
        if let Some(helper) = config.cred_helpers.get(key)
            && let Some(creds) = run_cred_helper(&format!("docker-credential-{helper}"), key)
        {
            return Some(creds);
        }
    }
    if let Some(store) = &config.creds_store {
        for key in &keys {
            if let Some(creds) = run_cred_helper(&format!("docker-credential-{store}"), key) {
                return Some(creds);
            }
        }
    }
    for key in &keys {
        if let Some(entry) = config.auths.get(key) {
            if let (Some(user), Some(pass)) = (&entry.username, &entry.password) {
                return Some((user.clone(), pass.clone()));
            }
            if let Some(auth) = &entry.auth {
                let decoded = BASE64.decode(auth).ok()?;
                let decoded = String::from_utf8(decoded).ok()?;
                let (user, pass) = decoded.split_once(':')?;
                return Some((user.to_owned(), pass.to_owned()));
            }
        }
    }
    None
}

/// The `docker-credential-*` protocol: the server key on stdin of `get`, a
/// JSON `{"Username", "Secret"}` on stdout. Failure (helper missing, no
/// stored credential) falls through to the next source rather than aborting:
/// the registry request itself reports auth problems.
fn run_cred_helper(program: &str, server: &str) -> Option<(String, String)> {
    #[derive(Deserialize)]
    struct HelperOutput {
        #[serde(rename = "Username")]
        username: String,
        #[serde(rename = "Secret")]
        secret: String,
    }

    let mut child = Command::new(program)
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(server.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let parsed: HelperOutput = serde_json::from_slice(&out.stdout).ok()?;
    Some((parsed.username, parsed.secret))
}

fn registry_error(url: &str, message: &str) -> Error {
    Error::Registry {
        image: url.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use crate::Error;
    use crate::registry::RegistryProbe;
    use crate::registry::http::{
        AuthFile, HttpProbe, ImageRef, base_url, credentials_from_config, parse_bearer_challenge,
        parse_image_ref, run_cred_helper,
    };

    #[test]
    fn image_refs_parse_like_container_tools() {
        let cases = [
            ("ghcr.io/acme/img:h1-abc", ("ghcr.io", "acme/img", "h1-abc")),
            ("localhost:5000/img", ("localhost:5000", "img", "latest")),
            (
                "registry.example.com/a/b/c:v1",
                ("registry.example.com", "a/b/c", "v1"),
            ),
            (
                "alpine",
                ("registry-1.docker.io", "library/alpine", "latest"),
            ),
            ("acme/img:tag", ("registry-1.docker.io", "acme/img", "tag")),
            (
                "docker.io/library/alpine:3",
                ("registry-1.docker.io", "library/alpine", "3"),
            ),
            ("ghcr.io/a/b@sha256:0011", ("ghcr.io", "a/b", "sha256:0011")),
        ];
        for (input, (registry, repository, reference)) in cases {
            let parsed = parse_image_ref(input).unwrap();
            assert_eq!(
                parsed,
                ImageRef {
                    registry: registry.into(),
                    repository: repository.into(),
                    reference: reference.into(),
                },
                "input: {input}"
            );
        }
        assert!(matches!(parse_image_ref(""), Err(Error::Registry { .. })));
    }

    #[test]
    fn loopback_registries_use_plain_http() {
        assert_eq!(base_url("localhost:5000"), "http://localhost:5000");
        assert_eq!(base_url("127.0.0.1:5000"), "http://127.0.0.1:5000");
        assert_eq!(base_url("ghcr.io"), "https://ghcr.io");
    }

    #[test]
    fn bearer_challenges_parse() {
        let c = parse_bearer_challenge(
            "Bearer realm=\"https://ghcr.io/token\",service=\"ghcr.io\",scope=\"repository:a/b:pull\"",
        )
        .unwrap();
        assert_eq!(c.realm, "https://ghcr.io/token");
        assert_eq!(c.service.as_deref(), Some("ghcr.io"));
        assert_eq!(c.scope.as_deref(), Some("repository:a/b:pull"));

        assert!(parse_bearer_challenge("Basic realm=\"x\"").is_none());
        assert!(parse_bearer_challenge("Bearer service=\"no-realm\"").is_none());
    }

    #[test]
    fn credentials_resolve_from_inline_auths() {
        let config: AuthFile =
            serde_json::from_str(r#"{"auths": {"ghcr.io": {"auth": "dXNlcjpzM2NyZXQ="}}}"#)
                .unwrap();
        assert_eq!(
            credentials_from_config(&config, "ghcr.io"),
            Some(("user".into(), "s3cret".into()))
        );
        assert_eq!(credentials_from_config(&config, "other.io"), None);

        // Docker Hub's historical key spelling still matches.
        let config: AuthFile = serde_json::from_str(
            r#"{"auths": {"https://index.docker.io/v1/": {"username": "u", "password": "p"}}}"#,
        )
        .unwrap();
        assert_eq!(
            credentials_from_config(&config, "registry-1.docker.io"),
            Some(("u".into(), "p".into()))
        );
    }

    #[test]
    fn missing_credential_helpers_fall_through_to_auths() {
        let config: AuthFile = serde_json::from_str(
            r#"{
                "credHelpers": {"ghcr.io": "supercargo-no-such-helper"},
                "credsStore": "supercargo-no-such-store",
                "auths": {"ghcr.io": {"auth": "dXNlcjpzM2NyZXQ="}}
            }"#,
        )
        .unwrap();
        assert_eq!(
            credentials_from_config(&config, "ghcr.io"),
            Some(("user".into(), "s3cret".into()))
        );
    }

    #[test]
    fn cred_helper_protocol_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("helper.sh");
        std::fs::write(
            &helper,
            "#!/bin/sh\nread server\n[ \"$server\" = \"ghcr.io\" ] || exit 1\n\
             printf '{\"Username\":\"u\",\"Secret\":\"s\"}'\n",
        )
        .unwrap();
        make_executable(&helper);

        let program = helper.to_str().unwrap();
        assert_eq!(
            run_cred_helper(program, "ghcr.io"),
            Some(("u".into(), "s".into()))
        );
        assert_eq!(
            run_cred_helper(program, "other.io"),
            None,
            "helper exit 1 means no creds"
        );
        assert_eq!(
            run_cred_helper("supercargo-no-such-helper", "ghcr.io"),
            None
        );
    }

    #[test]
    fn head_200_exists_404_does_not() {
        let host = spawn_server(|req| {
            if req.starts_with("HEAD /v2/demo/app/manifests/h1-aaa ") {
                response(200, &[], "")
            } else {
                response(404, &[], "")
            }
        });
        let probe = HttpProbe::with_auth_files(vec![]);
        assert!(probe.exists(&format!("{host}/demo/app:h1-aaa")).unwrap());
        assert!(!probe.exists(&format!("{host}/demo/app:h1-bbb")).unwrap());
    }

    #[test]
    fn bearer_challenge_flow_fetches_a_token_and_retries() {
        let host = spawn_server(move |req| {
            if req.starts_with("GET /token") {
                assert!(req.contains("scope=repository:demo/app:pull"), "req: {req}");
                return response(200, &[], "{\"token\": \"tok123\"}");
            }
            // ureq lowercases header names on the wire; match on the value.
            if req.contains("Bearer tok123") {
                return response(200, &[], "");
            }
            assert!(
                !req.contains("Bearer"),
                "unexpected authorization in: {req}"
            );
            response(
                401,
                &["WWW-Authenticate: Bearer realm=\"http://HOST/token\",service=\"test\""],
                "",
            )
        });
        let probe = HttpProbe::with_auth_files(vec![]);
        // The realm placeholder needs the real host; patch via a tiny proxy
        // handler is overkill, so the server stores it globally instead.
        set_server_host(&host);
        assert!(probe.exists(&format!("{host}/demo/app:h1-aaa")).unwrap());
    }

    #[test]
    fn basic_credentials_from_auth_file_are_sent() {
        let dir = tempfile::tempdir().unwrap();
        let auth_file = dir.path().join("config.json");

        let host = spawn_server(|req| {
            // user:s3cret; header names arrive lowercased, match on the value.
            if req.contains("Basic dXNlcjpzM2NyZXQ=") {
                response(200, &[], "")
            } else {
                response(401, &[], "")
            }
        });
        std::fs::write(
            &auth_file,
            format!("{{\"auths\": {{\"{host}\": {{\"auth\": \"dXNlcjpzM2NyZXQ=\"}}}}}}"),
        )
        .unwrap();

        let probe = HttpProbe::with_auth_files(vec![auth_file.clone()]);
        assert!(probe.exists(&format!("{host}/demo/app:h1-aaa")).unwrap());

        // Without a usable challenge on 401, the probe errors instead of
        // silently rebuilding the world.
        let probe = HttpProbe::with_auth_files(vec![]);
        let err = probe
            .exists(&format!("{host}/demo/app:h1-aaa"))
            .unwrap_err();
        assert!(matches!(err, Error::Registry { .. }));
    }

    #[test]
    fn unexpected_statuses_are_errors() {
        let host = spawn_server(|_| response(500, &[], ""));
        let probe = HttpProbe::with_auth_files(vec![]);
        assert!(matches!(
            probe.exists(&format!("{host}/demo/app:h1-aaa")),
            Err(Error::Registry { .. })
        ));
    }

    // Helpers below the tests, per AGENTS.md.

    use std::io::{Read as _, Write as _};
    use std::sync::OnceLock;

    /// Host the bearer-flow server substitutes into its challenge realm.
    static SERVER_HOST: OnceLock<String> = OnceLock::new();

    fn set_server_host(host: &str) {
        let _ = SERVER_HOST.set(host.to_owned());
    }

    /// Minimal single-threaded HTTP server: the handler receives the request
    /// head (request line + headers) and returns a full response.
    fn spawn_server(handler: impl Fn(&str) -> String + Send + 'static) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let host = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut head = Vec::new();
                let mut buf = [0u8; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).to_string();
                let response = handler(&text).replace(
                    "HOST",
                    SERVER_HOST.get().map(String::as_str).unwrap_or("HOST"),
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        host
    }

    fn response(status: u16, headers: &[&str], body: &str) -> String {
        let reason = match status {
            200 => "OK",
            401 => "Unauthorized",
            404 => "Not Found",
            _ => "Error",
        };
        let mut out = format!("HTTP/1.1 {status} {reason}\r\n");
        for h in headers {
            out.push_str(h);
            out.push_str("\r\n");
        }
        out.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        out
    }

    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
}
