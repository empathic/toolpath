//! Shared Pathbase client helpers.
//!
//! Hosts the HTTP client and session-storage logic used by `cmd_auth`,
//! `cmd_import`, and `cmd_export`. Config-dir resolution lives in the
//! sibling `config` module so `cmd_cache` (which doesn't depend on
//! reqwest and must build on emscripten) can reuse it.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config::config_dir;

pub(crate) const CREDENTIALS_FILE: &str = "credentials.json";
pub(crate) const DEFAULT_URL: &str = "https://pathbase.dev";
pub(crate) const PATHBASE_URL_ENV: &str = "PATHBASE_URL";

/// JSON blob persisted at `credentials.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredSession {
    pub url: String,
    pub token: String,
    pub user: User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct User {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// Response from `POST /api/v1/anon/paths` (matches the OpenAPI
/// `AnonUploadResponse` shape).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AnonUploadResponse {
    pub id: String,
    pub url: String,
}

/// Subset of the `TracePath` response from `POST /api/v1/repos/{owner}/{repo}/paths`
/// that the CLI actually uses. The spec carries more fields (timestamps,
/// step_count, repo_id, …); we only deserialize what we need.
///
/// `id` is the public share-token UUID. For secret paths the canonical
/// share URL is `<base>/<owner>/<repo>/paths/<id>` — the slug URL is the
/// owner-facing stub and isn't a reliable share link.
///
/// `is_public` is echoed from the server response (not the request) so
/// the caller can react to server-side overrides (rate limits, policy,
/// future feature flags) instead of trusting that what we asked for is
/// what landed.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CreatedPath {
    pub id: String,
    pub slug: String,
    pub is_public: bool,
}

// ── URL + prompt helpers ────────────────────────────────────────────────

pub(crate) fn resolve_url(cli_url: Option<String>) -> String {
    let raw = cli_url
        .or_else(|| std::env::var(PATHBASE_URL_ENV).ok())
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    raw.trim_end_matches('/').to_string()
}

pub(crate) fn prompt_line(prompt: &str) -> Result<String> {
    use std::io::{BufRead, Write};
    let mut stdout = std::io::stdout();
    stdout.write_all(prompt.as_bytes())?;
    stdout.flush()?;
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

// ── HTTP layer ──────────────────────────────────────────────────────────

pub(crate) fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("path-cli/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")
}

#[derive(Deserialize)]
pub(crate) struct RedeemResponse {
    pub token: String,
    pub user: User,
}

pub(crate) fn api_redeem(base_url: &str, code: &str) -> Result<(String, User)> {
    let client = http_client()?;
    let resp = client
        .post(format!("{base_url}/api/v1/auth/cli/redeem"))
        .json(&serde_json::json!({ "code": code }))
        .send()
        .with_context(|| format!("connect to {base_url}"))?;

    let status = resp.status();
    let body = resp.text().unwrap_or_default();

    if !status.is_success() {
        if status == reqwest::StatusCode::UNAUTHORIZED {
            bail!("code is invalid, already used, or expired — generate a new one");
        }
        if status == reqwest::StatusCode::BAD_REQUEST {
            let msg = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
                .unwrap_or_else(|| body.clone());
            bail!("{msg}");
        }
        bail!("redeem failed ({status}): {body}");
    }

    let parsed: RedeemResponse =
        serde_json::from_str(&body).with_context(|| format!("parsing redeem response: {body}"))?;
    Ok((parsed.token, parsed.user))
}

pub(crate) fn api_logout(base_url: &str, token: &str) -> Result<()> {
    let client = http_client()?;
    let resp = client
        .post(format!("{base_url}/api/v1/auth/logout"))
        .bearer_auth(token)
        .send()
        .with_context(|| format!("connect to {base_url}"))?;
    if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NO_CONTENT {
        bail!("server returned {}", resp.status());
    }
    Ok(())
}

pub(crate) fn api_me(base_url: &str, token: &str) -> Result<User> {
    let client = http_client()?;
    let resp = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .bearer_auth(token)
        .send()
        .with_context(|| format!("connect to {base_url}"))?;

    let status = resp.status();
    let body = resp.text().unwrap_or_default();

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        bail!(
            "{base_url} rejected your stored credentials ({status}). \
             Run `path auth login --url {base_url}` to authenticate against this server, \
             or pass `--anon` to upload anonymously."
        );
    }
    if !status.is_success() {
        bail!(
            "GET {base_url}/api/v1/auth/me returned {status}: {}",
            short_body(&body)
        );
    }
    serde_json::from_str(&body).map_err(|e| {
        anyhow!(
            "{base_url} returned a non-JSON response from /api/v1/auth/me ({status}): {} \
             ({e}). The URL may not be a Pathbase deployment.",
            short_body(&body)
        )
    })
}

/// Trim a response body to a single-line snippet for error messages.
/// Replaces newlines, collapses long bodies down to ~200 chars with an ellipsis.
fn short_body(body: &str) -> String {
    const MAX: usize = 200;
    let cleaned: String = body.replace(['\n', '\r'], " ");
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return "<empty body>".to_string();
    }
    if trimmed.chars().count() > MAX {
        let head: String = trimmed.chars().take(MAX - 1).collect();
        format!("{head}…")
    } else {
        trimmed.to_string()
    }
}

// ── pathbase-client bridge ─────────────────────────────────────────────
//
// Pathbase's documented surface is talked to through the typed
// `pathbase-client` crate, generated at build time from `openapi.json`.
// The generated client is async; the rest of path-cli is sync, so we
// tunnel through a `OnceLock`-cached current-thread tokio runtime via
// [`block_on`]. The whole module — auth, paths, downloads, async upload
// — runs on a single reqwest version (0.13). The auth flow stays
// hand-rolled only because the redeem endpoint isn't in the OpenAPI
// spec, not because of any HTTP-stack difference.

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::sync::OnceLock;
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime")
    });
    rt.block_on(f)
}

/// Build a `pathbase_client::Client` whose underlying reqwest client
/// carries a default `Authorization: Bearer <token>` header when one is
/// supplied. Progenitor doesn't expose a bearer-token setter, so we
/// pre-bake the header into the http client and hand it via
/// `Client::new_with_client`.
fn pathbase_client(base_url: &str, token: Option<&str>) -> Result<pathbase_client::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("path-cli/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30));
    if let Some(t) = token {
        let mut headers = reqwest::header::HeaderMap::new();
        let mut auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {t}"))
            .context("invalid characters in auth token")?;
        auth.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, auth);
        builder = builder.default_headers(headers);
    }
    let client = builder.build().context("build pathbase http client")?;
    Ok(pathbase_client::Client::new_with_client(base_url, client))
}

/// Decode a toolpath JSON string into the `Map` shape the generated
/// upload bodies expect. All three Document variants (`Step`, `Path`,
/// `Graph`) are externally-tagged objects, so the top-level is always a
/// JSON object — the parse can't fail on a well-formed toolpath doc.
fn parse_document(json: &str) -> Result<serde_json::Map<String, serde_json::Value>> {
    serde_json::from_str(json).context("parse toolpath document")
}

/// `POST /api/v1/anon/paths` — public, rate-limited, 5 MB cap. No auth.
/// Anon paths are URL-addressable share tokens (the UUID in the returned
/// URL is intentionally public); the trade-off is that they aren't
/// listable from any user account. For listable uploads use
/// [`paths_post`] against an authenticated session.
pub(crate) fn anon_paths_post(base_url: &str, document_json: &str) -> Result<AnonUploadResponse> {
    let body = pathbase_client::types::AnonUploadBody {
        document: parse_document(document_json)?,
    };
    let client = pathbase_client(base_url, None)?;
    match block_on(client.create_anon_path(&body)) {
        Ok(resp) => {
            let inner = resp.into_inner();
            Ok(AnonUploadResponse {
                id: inner.id,
                url: inner.url,
            })
        }
        Err(pathbase_client::Error::ErrorResponse(resp)) => match resp.status().as_u16() {
            413 => bail!(
                "anon upload exceeds the 5 MB cap — log in (`path auth login`) for a listable upload without that limit"
            ),
            429 => bail!("anon upload rate-limited; retry shortly or log in"),
            code => bail!("anon upload failed (HTTP {code})"),
        },
        Err(pathbase_client::Error::UnexpectedResponse(resp)) => {
            bail!(
                "anon upload returned unexpected status: HTTP {}",
                resp.status()
            )
        }
        Err(e) => Err(anyhow!("anon upload failed: {e}")),
    }
}

/// `POST /api/v1/repos/{owner}/{repo}/paths` — listable upload to a
/// repo the authenticated user owns. `is_public=false` writes a
/// pathstash-style secret/unlisted path; the URL is still publicly
/// addressable (UUIDs are public for both secret and public paths) but
/// won't appear in any user's listing.
pub(crate) fn paths_post(
    base_url: &str,
    token: &str,
    owner: &str,
    repo: &str,
    slug: &str,
    document_json: &str,
    is_public: bool,
) -> Result<CreatedPath> {
    let body = pathbase_client::types::UploadPathBody {
        document: parse_document(document_json)?,
        is_public: Some(is_public),
        slug: slug.to_string(),
    };
    let client = pathbase_client(base_url, Some(token))?;
    match block_on(client.create_path(owner, repo, &body)) {
        Ok(resp) => {
            let inner = resp.into_inner();
            Ok(CreatedPath {
                id: inner.id.to_string(),
                slug: inner.slug,
                is_public: inner.is_public,
            })
        }
        Err(pathbase_client::Error::ErrorResponse(resp)) => match resp.status().as_u16() {
            401 => bail!(
                "{base_url} rejected your stored credentials (HTTP 401). \
                 Run `path auth login --url {base_url}` to authenticate against this server, \
                 or pass `--anon` to upload anonymously."
            ),
            code => bail!("upload to {owner}/{repo} failed (HTTP {code})"),
        },
        Err(pathbase_client::Error::UnexpectedResponse(resp)) => {
            let status = resp.status();
            let body = block_on(resp.text()).unwrap_or_default();
            let msg = error_message(&body).unwrap_or(body);
            if msg.is_empty() {
                bail!("upload to {owner}/{repo} returned unexpected status: HTTP {status}")
            } else {
                bail!("upload to {owner}/{repo} failed ({status}): {msg}")
            }
        }
        Err(e) => Err(anyhow!("upload to {owner}/{repo} failed: {e}")),
    }
}

fn error_message(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
}

/// `POST /api/v1/repos` — create a repo owned by the authenticated user.
/// Treats 409 (already exists) as success so callers can use this
/// idempotently to ensure pathstash exists before uploading to it.
pub(crate) fn repos_post(base_url: &str, token: &str, name: &str) -> Result<()> {
    let body = pathbase_client::types::CreateRepoBody {
        name: name.to_string(),
        description: None,
    };
    let client = pathbase_client(base_url, Some(token))?;
    match block_on(client.create_repo(&body)) {
        Ok(_) => Ok(()),
        Err(pathbase_client::Error::ErrorResponse(resp)) => match resp.status().as_u16() {
            401 => bail!(
                "{base_url} rejected your stored credentials (HTTP 401). \
                 Run `path auth login --url {base_url}` to authenticate against this server, \
                 or pass `--anon` to upload anonymously."
            ),
            409 => Ok(()),
            code => bail!("creating repo {name} failed (HTTP {code})"),
        },
        Err(pathbase_client::Error::UnexpectedResponse(resp)) => match resp.status().as_u16() {
            // The OpenAPI spec only documents 200 and 401 for create_repo,
            // so a 409 lands in UnexpectedResponse rather than ErrorResponse.
            // Treat it as success — repo already exists.
            409 => Ok(()),
            code => bail!("creating repo {name} returned unexpected status: HTTP {code}"),
        },
        Err(e) => Err(anyhow!("creating repo {name} failed: {e}")),
    }
}

/// `GET /api/v1/repos/{owner}/{repo}/paths/{slug}/download` — fetch the
/// raw toolpath JSON for a path. Public paths and unlisted-but-shared
/// paths both download without authentication; only fully private paths
/// (gated by an ACL beyond `is_public=false`) require auth.
///
/// **Why this doesn't go through `pathbase-client`.** progenitor's
/// generated client decodes the response body into
/// `serde_json::Map<String, Value>` (per the spec's
/// `application/json` content type) and we'd then re-serialize to get a
/// String back. That's a wasted round-trip — and the BTreeMap-backed
/// `serde_json::Map` reorders keys, so the bytes the caller sees aren't
/// the bytes the server sent. For a "give me back the document I just
/// uploaded" endpoint, byte-fidelity matters. We use blocking reqwest
/// directly and forward the response body verbatim.
pub(crate) fn paths_download(
    base_url: &str,
    token: Option<&str>,
    owner: &str,
    repo: &str,
    slug: &str,
) -> Result<String> {
    let client = http_client()?;
    let mut req = client.get(format!(
        "{base_url}/api/v1/repos/{owner}/{repo}/paths/{slug}/download"
    ));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req
        .send()
        .with_context(|| format!("connect to {base_url}"))?;

    let status = resp.status();
    let text = resp.text().unwrap_or_default();

    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!(
            "this path is private and requires authentication — run `path auth login --url {base_url}` and retry"
        );
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        bail!("path {owner}/{repo}/{slug} not found on {base_url}");
    }
    if !status.is_success() {
        let msg = error_message(&text).unwrap_or(text);
        bail!("download of {owner}/{repo}/{slug} failed ({status}): {msg}");
    }
    Ok(text)
}

// ── File storage ────────────────────────────────────────────────────────

pub(crate) fn credentials_path() -> Result<PathBuf> {
    Ok(config_dir()?.join(CREDENTIALS_FILE))
}

pub(crate) fn store_session(path: &Path, s: &StoredSession) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("credentials path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }

    let payload = serde_json::to_string_pretty(s)?;
    std::fs::write(path, payload).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn load_session(path: &Path) -> Result<Option<StoredSession>> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(None),
        Ok(s) => Ok(Some(serde_json::from_str(&s).with_context(|| {
            format!("decode credentials at {}", path.display())
        })?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow!("read {}: {e}", path.display())),
    }
}

pub(crate) fn clear_session(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow!("remove {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StoredSession {
        StoredSession {
            url: "https://pathbase.dev".into(),
            token: "tok".into(),
            user: User {
                id: "u1".into(),
                username: "alice".into(),
                email: Some("alice@example.com".into()),
                display_name: None,
                avatar_url: None,
            },
        }
    }

    #[test]
    fn resolve_url_prefers_cli_flag() {
        let got = resolve_url(Some("https://example.com/".into()));
        assert_eq!(got, "https://example.com");
    }

    #[test]
    fn short_body_handles_empty_and_whitespace() {
        assert_eq!(short_body(""), "<empty body>");
        assert_eq!(short_body("   \n\t  "), "<empty body>");
    }

    #[test]
    fn short_body_collapses_newlines_to_spaces() {
        assert_eq!(short_body("line1\nline2\r\nline3"), "line1 line2  line3");
    }

    #[test]
    fn short_body_truncates_long_input_with_ellipsis() {
        let long = "x".repeat(500);
        let s = short_body(&long);
        assert_eq!(s.chars().count(), 200);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn store_then_load_roundtrips_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        assert!(load_session(&path).unwrap().is_none());
        store_session(&path, &sample()).unwrap();
        let back = load_session(&path).unwrap().unwrap();
        assert_eq!(back.user.username, "alice");
        assert_eq!(back.token, "tok");
    }

    #[test]
    fn store_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("nested")
            .join("dir")
            .join("credentials.json");
        store_session(&path, &sample()).unwrap();
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn store_sets_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        store_session(&path, &sample()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "expected 0600 on credentials file, got {mode:o}"
        );
    }

    #[test]
    fn clear_on_missing_file_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(clear_session(&path).is_ok());
    }

    #[test]
    fn load_empty_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, "").unwrap();
        assert!(load_session(&path).unwrap().is_none());
    }

    // ── Mock HTTP server ─────────────────────────────────────────────

    /// A one-shot HTTP/1.1 responder. Binds to 127.0.0.1 on a free port,
    /// reads one request (headers + body), writes a canned response, closes.
    struct MockServer {
        port: u16,
        thread: Option<std::thread::JoinHandle<Vec<u8>>>,
    }

    impl MockServer {
        fn start(status_line: &'static str, body: &'static str) -> Self {
            use std::io::{BufRead, BufReader, Write};
            use std::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let thread = std::thread::spawn(move || {
                let (mut stream, _addr) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut req = Vec::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 {
                        break;
                    }
                    req.extend_from_slice(line.as_bytes());
                    if line == "\r\n" {
                        break;
                    }
                }
                let content_length = req
                    .split(|b| *b == b'\n')
                    .find_map(|line| {
                        let line = std::str::from_utf8(line).ok()?;
                        let (name, value) = line.trim_end_matches('\r').split_once(':')?;
                        if name.eq_ignore_ascii_case("content-length") {
                            value.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                if content_length > 0 {
                    use std::io::Read;
                    let mut body_buf = vec![0u8; content_length];
                    reader.read_exact(&mut body_buf).ok();
                    req.extend_from_slice(&body_buf);
                }

                let response = format!(
                    "{status_line}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
                req
            });
            MockServer {
                port,
                thread: Some(thread),
            }
        }

        fn base(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        fn request(mut self) -> Vec<u8> {
            self.thread.take().unwrap().join().unwrap()
        }
    }

    /// A complete `TracePath` JSON body for mock-server responses. Progenitor
    /// strictly validates response shapes against the OpenAPI schema, so the
    /// mock has to return every required field even though the CLI only reads
    /// `slug`. Centralized here so tests don't drift when the schema changes.
    fn trace_path_json() -> &'static str {
        r#"{
            "id": "00000000-0000-0000-0000-000000000001",
            "repo_id": "00000000-0000-0000-0000-000000000002",
            "slug": "my-path",
            "toolpath_id": "tp-1",
            "document": {"Step": {}},
            "step_count": 0,
            "is_public": false,
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z"
        }"#
    }

    #[test]
    fn paths_post_wraps_document_with_slug_and_is_public() {
        let server = MockServer::start("HTTP/1.1 201 Created", trace_path_json());
        let created = paths_post(
            &server.base(),
            "tok",
            "alex",
            "pathstash",
            "my-path",
            r#"{"Step":{}}"#,
            false,
        )
        .unwrap();
        assert_eq!(created.slug, "my-path");

        let req = String::from_utf8(server.request()).unwrap();
        assert!(
            req.starts_with("POST /api/v1/repos/alex/pathstash/paths "),
            "got: {req}"
        );
        assert!(
            req.to_lowercase().contains("authorization: bearer tok"),
            "got: {req}"
        );
        assert!(req.contains(r#""slug":"my-path""#), "got: {req}");
        assert!(req.contains(r#""is_public":false"#), "got: {req}");
        assert!(req.contains(r#""document":{"Step":{}}"#), "got: {req}");
    }

    #[test]
    fn paths_post_401_surfaces_relogin_message() {
        let server = MockServer::start("HTTP/1.1 401 Unauthorized", r#"{"error":"bad"}"#);
        let base = server.base();
        let err =
            paths_post(&base, "tok", "alex", "pathstash", "s", "{}", false).unwrap_err();
        let msg = err.to_string();
        // Should name the URL the credentials are being rejected by, point at
        // `path auth login --url`, and offer `--anon` as the bypass.
        assert!(msg.contains(&base), "expected base URL in error: {msg}");
        assert!(msg.contains("path auth login --url"), "expected re-auth hint: {msg}");
        assert!(msg.contains("--anon"), "expected --anon hint: {msg}");
    }

    #[test]
    fn paths_post_5xx_includes_server_message() {
        let server = MockServer::start(
            "HTTP/1.1 500 Internal Server Error",
            r#"{"error":"database is on fire"}"#,
        );
        let err =
            paths_post(&server.base(), "tok", "alex", "pathstash", "s", "{}", false).unwrap_err();
        assert!(err.to_string().contains("database is on fire"), "{err}");
    }

    #[test]
    fn anon_paths_post_wraps_document_and_omits_auth() {
        let server = MockServer::start(
            "HTTP/1.1 201 Created",
            r#"{"id":"abc","url":"https://pathbase.dev/anon/abc"}"#,
        );
        let resp = anon_paths_post(&server.base(), r#"{"Step":{}}"#).unwrap();
        assert_eq!(resp.id, "abc");
        assert_eq!(resp.url, "https://pathbase.dev/anon/abc");

        let req = String::from_utf8(server.request()).unwrap();
        assert!(req.starts_with("POST /api/v1/anon/paths "), "got: {req}");
        assert!(
            !req.to_lowercase().contains("authorization:"),
            "anon must not send auth header: {req}"
        );
        assert!(req.contains(r#""document":{"Step":{}}"#), "got: {req}");
    }

    #[test]
    fn anon_paths_post_413_advises_login() {
        let server = MockServer::start("HTTP/1.1 413 Payload Too Large", "");
        let err = anon_paths_post(&server.base(), "{}").unwrap_err();
        assert!(err.to_string().contains("5 MB"), "{err}");
        assert!(err.to_string().contains("path auth login"), "{err}");
    }

    #[test]
    fn repos_post_treats_409_as_success() {
        let server = MockServer::start("HTTP/1.1 409 Conflict", r#"{"error":"already exists"}"#);
        repos_post(&server.base(), "tok", "pathstash").unwrap();
    }

    #[test]
    fn paths_download_returns_body_byte_for_byte() {
        // Key ordering matters: the server's bytes must come back unmodified.
        // With the round-trip removed (raw blocking GET, no Map decode), this
        // is a straight string equality. If progenitor ever sneaks back in
        // for this endpoint, the BTreeMap-backed Map reorders keys and this
        // assertion catches it.
        let body = r#"{"Step":{"step":{"id":"s1","actor":"human:x","timestamp":"2024-01-01T00:00:00Z"},"change":{}}}"#;
        let server = MockServer::start("HTTP/1.1 200 OK", body);
        let got =
            paths_download(&server.base(), Some("tok"), "alex", "pathstash", "my-path").unwrap();
        assert_eq!(got, body);

        let req = String::from_utf8(server.request()).unwrap();
        assert!(
            req.starts_with("GET /api/v1/repos/alex/pathstash/paths/my-path/download "),
            "got: {req}"
        );
        assert!(
            req.to_lowercase().contains("authorization: bearer tok"),
            "got: {req}"
        );
    }

    #[test]
    fn paths_download_404_says_not_found() {
        let server = MockServer::start("HTTP/1.1 404 Not Found", "");
        let err = paths_download(&server.base(), Some("tok"), "alex", "pathstash", "missing")
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
