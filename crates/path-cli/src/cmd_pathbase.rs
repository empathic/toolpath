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

/// Reference returned after uploading a trace.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TraceRef {
    pub id: String,
    pub url: String,
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

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        bail!("stored session is no longer valid — run `path auth login` again");
    }
    if !resp.status().is_success() {
        bail!("server returned {}", resp.status());
    }
    let user: User = resp.json().context("parsing /auth/me response")?;
    Ok(user)
}

/// `POST /api/v1/traces` — upload a toolpath document.
pub(crate) fn traces_post(base_url: &str, token: &str, body: &str) -> Result<TraceRef> {
    let client = http_client()?;
    let resp = client
        .post(format!("{base_url}/api/v1/traces"))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .with_context(|| format!("connect to {base_url}"))?;

    let status = resp.status();
    let text = resp.text().unwrap_or_default();

    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!("stored session is no longer valid — run `path auth login` again");
    }
    if !status.is_success() {
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
            .unwrap_or_else(|| text.clone());
        bail!("upload failed ({status}): {msg}");
    }

    serde_json::from_str(&text).with_context(|| format!("parsing upload response: {text}"))
}

/// `GET /api/v1/traces/{id}` — download a toolpath document body (JSON).
pub(crate) fn traces_get(base_url: &str, token: &str, id: &str) -> Result<String> {
    let client = http_client()?;
    let resp = client
        .get(format!("{base_url}/api/v1/traces/{id}"))
        .bearer_auth(token)
        .send()
        .with_context(|| format!("connect to {base_url}"))?;

    let status = resp.status();
    let text = resp.text().unwrap_or_default();

    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!("stored session is no longer valid — run `path auth login` again");
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        bail!("trace {id} not found on {base_url}");
    }
    if !status.is_success() {
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
            .unwrap_or_else(|| text.clone());
        bail!("download failed ({status}): {msg}");
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

/// Load the stored session or bail with a helpful message.
pub(crate) fn require_session() -> Result<StoredSession> {
    let path = credentials_path()?;
    load_session(&path)?.ok_or_else(|| anyhow!("Not logged in. Run `path auth login`."))
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

    #[test]
    fn traces_post_returns_id_and_url_on_success() {
        let server = MockServer::start(
            "HTTP/1.1 200 OK",
            r#"{"id":"trc_01H","url":"https://pathbase.dev/traces/trc_01H"}"#,
        );
        let trace = traces_post(&server.base(), "tok", r#"{"Step":{}}"#).unwrap();
        assert_eq!(trace.id, "trc_01H");
        assert_eq!(trace.url, "https://pathbase.dev/traces/trc_01H");

        let req = String::from_utf8(server.request()).unwrap();
        assert!(req.starts_with("POST /api/v1/traces "), "got: {req}");
        assert!(
            req.to_lowercase().contains("authorization: bearer tok"),
            "got: {req}"
        );
        assert!(req.contains(r#"{"Step":{}}"#));
    }

    #[test]
    fn traces_post_401_surfaces_relogin_message() {
        let server = MockServer::start("HTTP/1.1 401 Unauthorized", r#"{"error":"bad"}"#);
        let err = traces_post(&server.base(), "tok", "{}").unwrap_err();
        assert!(err.to_string().contains("run `path auth login`"));
    }

    #[test]
    fn traces_post_5xx_includes_server_message() {
        let server = MockServer::start(
            "HTTP/1.1 500 Internal Server Error",
            r#"{"error":"database is on fire"}"#,
        );
        let err = traces_post(&server.base(), "tok", "{}").unwrap_err();
        assert!(err.to_string().contains("database is on fire"), "{err}");
    }

    #[test]
    fn traces_get_returns_body_on_success() {
        let body = r#"{"Step":{"step":{"id":"s1","actor":"human:x","timestamp":"2024-01-01T00:00:00Z"},"change":{}}}"#;
        let server = MockServer::start("HTTP/1.1 200 OK", body);
        let got = traces_get(&server.base(), "tok", "trc_01H").unwrap();
        assert_eq!(got, body);

        let req = String::from_utf8(server.request()).unwrap();
        assert!(req.starts_with("GET /api/v1/traces/trc_01H "), "got: {req}");
        assert!(
            req.to_lowercase().contains("authorization: bearer tok"),
            "got: {req}"
        );
    }

    #[test]
    fn traces_get_404_says_not_found() {
        let server = MockServer::start("HTTP/1.1 404 Not Found", "");
        let err = traces_get(&server.base(), "tok", "trc_nope").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
