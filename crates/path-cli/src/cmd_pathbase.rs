//! Shared Pathbase client helpers.
//!
//! Wraps the typed `pathbase-client` (generated from
//! `crates/pathbase-client/openapi.json` — refresh via
//! `scripts/refresh-pathbase-openapi.sh`) plus session-storage logic
//! used by `cmd_auth`, `cmd_import`, `cmd_export`, and `cmd_share`.
//! Every Pathbase HTTP call now goes through the typed client; no
//! hand-rolled reqwest left in this module. Config-dir resolution lives
//! in the sibling `config` module so `cmd_cache` (which doesn't depend
//! on reqwest and must build on emscripten) can reuse it.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) use crate::config::PATHBASE_URL_ENV;
use crate::config::config_dir;

pub(crate) const DEFAULT_URL: &str = "https://pathbase.dev";

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
}

/// Response from `POST /api/v1/u/anon/repos/pathstash/graphs`.
/// `id` is the graph UUID; `url` is the server-rendered share URL.
#[derive(Debug, Clone)]
pub(crate) struct AnonGraphResponse {
    pub id: String,
    pub url: String,
}

/// Response from `POST /api/v1/u/{owner}/repos/{repo}/graphs`.
///
/// `id` is the graph's UUID (the share token for `Unlisted` and `Public`
/// graphs). `url` is the server-rendered canonical URL clients should
/// link to. `visibility` is what the server actually applied — may
/// diverge from what the caller requested if server-side policy clamped
/// it, so callers should render based on this value rather than the
/// request.
#[derive(Debug, Clone)]
pub(crate) struct CreatedGraph {
    pub id: String,
    pub url: String,
    pub visibility: pathbase_client::types::Visibility,
}

// ── URL + prompt helpers ────────────────────────────────────────────────

pub(crate) fn resolve_url(cli_url: Option<String>) -> String {
    let raw = cli_url
        .or_else(|| std::env::var(PATHBASE_URL_ENV).ok())
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    raw.trim_end_matches('/').to_string()
}

/// Extract `scheme://host[:port]` from a URL, dropping any path/query.
/// Returns the input unchanged if it doesn't look like a URL. Used to
/// compare a stored session's host against the upload target so we can
/// warn / fall back when the two don't agree.
pub(crate) fn host_of(url: &str) -> &str {
    let after_scheme = match url.find("://") {
        Some(i) => i + 3,
        None => return url,
    };
    match url[after_scheme..].find('/') {
        Some(off) => &url[..after_scheme + off],
        None => url,
    }
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

pub(crate) fn api_redeem(base_url: &str, code: &str) -> Result<(String, User)> {
    let body = pathbase_client::types::RedeemBody {
        code: code.to_string(),
    };
    let client = pathbase_client(base_url, None)?;
    match block_on(client.cli_redeem(&body)) {
        Ok(resp) => {
            let inner = resp.into_inner();
            let u = inner.user;
            Ok((
                inner.token,
                User {
                    id: u.id.to_string(),
                    username: u.username,
                    email: u.email,
                    display_name: u.display_name,
                },
            ))
        }
        Err(pathbase_client::Error::ErrorResponse(resp)) => match resp.status().as_u16() {
            401 => bail!("code is invalid, already used, or expired — generate a new one"),
            400 => bail!("invalid code format"),
            code => bail!("redeem failed (HTTP {code})"),
        },
        Err(pathbase_client::Error::UnexpectedResponse(resp)) => {
            let status = resp.status();
            let body = block_on(resp.text()).unwrap_or_default();
            let msg = error_message(&body).unwrap_or_else(|| short_body(&body));
            if msg.is_empty() {
                bail!("redeem failed ({status})")
            } else {
                bail!("redeem failed ({status}): {msg}")
            }
        }
        Err(pathbase_client::Error::CommunicationError(e)) => {
            bail!("connect to {base_url}: {}", reqwest_hint(&e))
        }
        Err(e) => Err(anyhow!("redeem failed: {}", full_chain(&e))),
    }
}

/// Revoke the current bearer-token session on the server, then return.
///
/// Pathbase 1.1 dropped the dedicated `/logout` endpoint in favor of a
/// uniform sessions surface. There's no "revoke whoever made this
/// request" endpoint; the CLI has to list its own sessions, find the
/// one flagged `is_current`, and `DELETE` it by id.
///
/// Returns `Ok(())` even when the current session can't be located in
/// the response — callers always proceed to clear local credentials, so
/// failure here just means the token will rot server-side until expiry.
pub(crate) fn api_logout(base_url: &str, token: &str) -> Result<()> {
    let client = pathbase_client(base_url, Some(token))?;
    let sessions = match block_on(client.list_sessions()) {
        Ok(resp) => resp.into_inner(),
        Err(pathbase_client::Error::ErrorResponse(resp)) => {
            bail!("server returned {}", resp.status())
        }
        Err(pathbase_client::Error::UnexpectedResponse(resp)) => {
            bail!("server returned {}", resp.status())
        }
        Err(pathbase_client::Error::CommunicationError(e)) => {
            bail!("connect to {base_url}: {}", reqwest_hint(&e))
        }
        Err(e) => bail!("connect to {base_url}: {}", full_chain(&e)),
    };
    let current = sessions
        .iter()
        .find(|s| s.is_current)
        .map(|s| s.id.as_str());
    let Some(id_str) = current else {
        // No session marked current — server accepted the token but
        // can't tell us which session it belongs to. Nothing to revoke
        // server-side; local credentials still get cleared by the caller.
        return Ok(());
    };
    let id: uuid::Uuid = id_str
        .parse()
        .with_context(|| format!("server returned a non-UUID session id: {id_str}"))?;
    match block_on(client.revoke_session(&id)) {
        Ok(_) => Ok(()),
        Err(pathbase_client::Error::ErrorResponse(resp)) => {
            bail!("server returned {}", resp.status())
        }
        Err(pathbase_client::Error::UnexpectedResponse(resp)) => {
            bail!("server returned {}", resp.status())
        }
        Err(pathbase_client::Error::CommunicationError(e)) => {
            bail!("connect to {base_url}: {}", reqwest_hint(&e))
        }
        Err(e) => Err(anyhow!("connect to {base_url}: {}", full_chain(&e))),
    }
}

/// Errors are intentionally terse one-liners — callers compose them
/// into either a fallback notice ("note: <err>; falling back to
/// anonymous") or a propagated error with actionable next-step hints.
/// Don't bake the hints in here; otherwise the fallback notice gets
/// telephone-pole long.
pub(crate) fn api_me(base_url: &str, token: &str) -> Result<User> {
    let client = pathbase_client(base_url, Some(token))?;
    match block_on(client.get_me()) {
        Ok(resp) => {
            let u = resp.into_inner();
            Ok(User {
                id: u.id.to_string(),
                username: u.username,
                email: u.email,
                display_name: u.display_name,
            })
        }
        Err(pathbase_client::Error::ErrorResponse(resp)) => {
            let status = resp.status();
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                bail!("{base_url} rejected the stored credentials ({status})")
            } else {
                bail!("{base_url} returned {status} on /api/v1/users/me")
            }
        }
        Err(pathbase_client::Error::UnexpectedResponse(resp)) => {
            bail!("{base_url} returned {} on /api/v1/users/me", resp.status())
        }
        Err(pathbase_client::Error::InvalidResponsePayload(_, _)) => {
            bail!("{base_url} isn't a Pathbase deployment (non-JSON /api/v1/users/me response)")
        }
        Err(pathbase_client::Error::CommunicationError(e)) => {
            bail!("connect to {base_url}: {}", reqwest_hint(&e))
        }
        Err(e) => Err(anyhow!("connect to {base_url}: {}", full_chain(&e))),
    }
}

/// Pre-resolved upload mode. Produced by [`preflight_auth`] before any
/// expensive work (session pickers, cache writes, derive passes) so that
/// callers can fail fast or fall back to anonymous mode without making
/// the user select a session and *then* discover the credentials are bad.
#[derive(Debug)]
pub(crate) enum AuthMode {
    /// Use the public anonymous endpoint. No credentials required;
    /// 5 MB cap and rate-limited.
    Anon,
    /// Use the authenticated endpoint. Credentials have already been
    /// validated against the target server via `api_me`.
    Authed { token: String, username: String },
}

/// Probe credentials and decide whether the upload should go authed or
/// anonymous, *before* any picker/derive/cache work. Behavior:
///
/// - `--anon` → `Anon`, no credentials check.
/// - No stored credentials and no auth-requiring flags → `Anon` with the
///   "not logged in — uploading anonymously" notice.
/// - Stored credentials present → call `api_me` against the target URL.
///   - On success → `Authed { token, username }`.
///   - On failure with no auth-requiring flags (`--repo`/`--public`/`--slug`)
///     → fall back to `Anon` with a stderr notice explaining why.
///   - On failure with auth-requiring flags → propagate the error so the
///     user knows their explicit request can't be satisfied.
///
/// `host_of(base_url) != host_of(stored.url)` triggers an advisory warning
/// before the credentials probe so the user sees the mismatch even if
/// `api_me` happens to succeed.
pub(crate) fn preflight_auth(base_url: &str, anon: bool, needs_auth: bool) -> Result<AuthMode> {
    if anon {
        return Ok(AuthMode::Anon);
    }
    let stored = load_session(&credentials_path()?)?;

    let go_anon = stored.is_none() && !needs_auth;
    if go_anon {
        eprintln!(
            "note: not logged in — uploading anonymously (not listable). \
             Run `path auth login --url {base_url}` for a listable upload."
        );
        return Ok(AuthMode::Anon);
    }

    let session = match stored {
        Some(s) => s,
        None => bail!("Not logged in. Run `path auth login` or pass `--anon`."),
    };

    if host_of(base_url) != host_of(&session.url) {
        eprintln!(
            "warning: stored credentials are for {}, but you're uploading to {}.",
            session.url, base_url
        );
    }

    match api_me(base_url, &session.token) {
        Ok(user) => Ok(AuthMode::Authed {
            token: session.token,
            username: user.username,
        }),
        Err(e) if needs_auth => Err(e.context(format!(
            "--repo / --public / --slug require an authenticated upload. \
             Run `path auth login --url {base_url}` to authenticate against this \
             server, or drop those flags to upload anonymously."
        ))),
        Err(e) => {
            eprintln!("note: {e}; falling back to anonymous upload.");
            Ok(AuthMode::Anon)
        }
    }
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

/// Decode a toolpath JSON string into the typed `ToolpathDocument` the
/// generated upload bodies expect. A toolpath `Graph` document is a
/// `{graph, paths, meta?}` object — the parse can't fail on a
/// well-formed Graph.
fn parse_document(json: &str) -> Result<pathbase_client::types::ToolpathDocument> {
    serde_json::from_str(json).context("parse toolpath document")
}

/// Map the CLI's boolean `--public` flag to the wire-level visibility
/// the upload body expects. Public when `true`; `Unlisted` when `false`
/// — the historical "secret" semantic where the graph is addressed only
/// by its UUID share-link.
fn visibility_from_public_flag(public: bool) -> pathbase_client::types::Visibility {
    use pathbase_client::types::Visibility;
    if public {
        Visibility::Public
    } else {
        Visibility::Unlisted
    }
}

/// `POST /api/v1/u/anon/repos/pathstash/graphs` — public, rate-limited.
/// No auth. Anon graphs are always `Unlisted` (URL-addressable but
/// unlistable); the trade-off is that they aren't listable from any
/// user account. For listable uploads use [`graphs_post`] against an
/// authenticated session.
pub(crate) fn anon_graphs_post(base_url: &str, document_json: &str) -> Result<AnonGraphResponse> {
    let body = pathbase_client::types::UploadGraphBody {
        document: parse_document(document_json)?,
        name: None,
        visibility: None,
    };
    let client = pathbase_client(base_url, None)?;
    match block_on(client.create_anon_graph(&body)) {
        Ok(resp) => {
            let inner = resp.into_inner();
            Ok(AnonGraphResponse {
                id: inner.id.to_string(),
                url: inner.url,
            })
        }
        Err(pathbase_client::Error::ErrorResponse(resp)) => {
            let code = resp.status().as_u16();
            match code {
                413 => bail!(
                    "anon upload exceeds the size cap — log in (`path auth login`) for a listable upload without that limit"
                ),
                429 => bail!("anon upload rate-limited; retry shortly or log in"),
                _ => {
                    // Surface the server's `ApiErrorResponse.error` (e.g. the
                    // 400 naming duplicate step IDs) instead of just the code.
                    let msg = resp.into_inner().error;
                    if msg.is_empty() {
                        bail!("anon upload failed (HTTP {code})")
                    } else {
                        bail!("anon upload failed (HTTP {code}): {msg}")
                    }
                }
            }
        }
        Err(pathbase_client::Error::UnexpectedResponse(resp)) => {
            let status = resp.status();
            let body = block_on(resp.text()).unwrap_or_default();
            let msg = error_message(&body).unwrap_or_else(|| short_body(&body));
            if msg.is_empty() {
                bail!("anon upload failed ({status})")
            } else {
                bail!("anon upload failed ({status}): {msg}")
            }
        }
        Err(pathbase_client::Error::CommunicationError(e)) => {
            bail!("anon upload failed: {}", reqwest_hint(&e))
        }
        Err(e) => Err(anyhow!("anon upload failed: {}", full_chain(&e))),
    }
}

/// `POST /api/v1/u/{owner}/repos/{repo}/graphs` — listable upload to
/// a repo the authenticated user owns.
///
/// `name` is a free-form display label; it is **not** addressable —
/// the server addresses graphs by UUID. Pass `None` to let the server
/// default it.
///
/// `public=false` writes a secret/unlisted graph; the graph stays
/// addressable via its UUID share-link but doesn't appear in any
/// listing. `public=true` writes a `Public` graph that appears in
/// owner and public listings. The wire spec also exposes `Private`
/// (owner-only); the CLI flag is boolean so we don't surface it here.
pub(crate) fn graphs_post(
    base_url: &str,
    token: &str,
    owner: &str,
    repo: &str,
    name: Option<&str>,
    document_json: &str,
    public: bool,
) -> Result<CreatedGraph> {
    let body = pathbase_client::types::UploadGraphBody {
        document: parse_document(document_json)?,
        name: name.map(|s| s.to_string()),
        visibility: Some(visibility_from_public_flag(public)),
    };
    let client = pathbase_client(base_url, Some(token))?;
    match block_on(client.create_graph(owner, repo, &body)) {
        Ok(resp) => {
            let inner = resp.into_inner();
            Ok(CreatedGraph {
                id: inner.id.to_string(),
                url: inner.url,
                visibility: inner.visibility,
            })
        }
        Err(pathbase_client::Error::ErrorResponse(resp)) => {
            let code = resp.status().as_u16();
            if code == 401 {
                bail!(
                    "{base_url} rejected your stored credentials (HTTP 401). \
                     Run `path auth login --url {base_url}` to authenticate against this server, \
                     or pass `--anon` to upload anonymously."
                )
            }
            // Declared error responses (e.g. the 400 from duplicate step IDs)
            // carry an `ApiErrorResponse { code, error }` body — surface the
            // human-readable `error` rather than just the status code.
            let msg = resp.into_inner().error;
            if msg.is_empty() {
                bail!("upload to {owner}/{repo} failed (HTTP {code})")
            } else {
                bail!("upload to {owner}/{repo} failed (HTTP {code}): {msg}")
            }
        }
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
        Err(pathbase_client::Error::CommunicationError(e)) => {
            bail!("upload to {owner}/{repo} failed: {}", reqwest_hint(&e))
        }
        Err(e) => Err(anyhow!(
            "upload to {owner}/{repo} failed: {}",
            full_chain(&e)
        )),
    }
}

fn error_message(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
}

/// Walk an error's `source()` chain and join each link's `Display`
/// with `: `. progenitor's `CommunicationError(reqwest::Error)`
/// renders as "error sending request" by default — the actually-useful
/// detail (timeout / connection refused / TLS handshake) sits two or
/// three levels down in `source()`. This surfaces it.
fn full_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut s = err.to_string();
    let mut cur = err.source();
    while let Some(c) = cur {
        s.push_str(": ");
        s.push_str(&c.to_string());
        cur = c.source();
    }
    s
}

/// Classify a `reqwest::Error` into a short hint so users can tell
/// "took too long" from "couldn't connect" from "server hung up." Falls
/// back to the full source chain when no specific hint applies.
fn reqwest_hint(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        return "request timed out after 30s — try again, or shrink the upload".to_string();
    }
    if err.is_connect() {
        return format!("couldn't connect to server: {}", full_chain(err));
    }
    if err.is_body() {
        return format!("body error: {}", full_chain(err));
    }
    if err.is_decode() {
        return format!("response decode error: {}", full_chain(err));
    }
    full_chain(err)
}

/// `POST /api/v1/u/{owner}/repos` — create a repo owned by the
/// authenticated user. Owner must match the caller's username.
/// Visibility defaults to `Public` per server spec; we don't override
/// it here because the only documented caller (`run_pathbase_inner`)
/// uses this to ensure the user's `pathstash` exists, and `pathstash`
/// is system-pinned to `Unlisted` by direct DB seeding regardless of
/// what this endpoint asks for.
///
/// Treats 409 (already exists) as success so callers can use this
/// idempotently to ensure a repo exists before uploading to it.
pub(crate) fn repos_post(base_url: &str, token: &str, owner: &str, name: &str) -> Result<()> {
    let body = pathbase_client::types::CreateRepoBody {
        name: name.to_string(),
        description: None,
        visibility: None,
    };
    let client = pathbase_client(base_url, Some(token))?;
    match block_on(client.create_repo(owner, &body)) {
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
            409 => Ok(()),
            code => bail!("creating repo {name} returned unexpected status: HTTP {code}"),
        },
        Err(pathbase_client::Error::CommunicationError(e)) => {
            bail!("creating repo {name} failed: {}", reqwest_hint(&e))
        }
        Err(e) => Err(anyhow!("creating repo {name} failed: {}", full_chain(&e))),
    }
}

/// `GET /api/v1/u/{owner}/repos/{repo}/graphs/{id}/download` — fetch
/// the reconstructed Graph document by UUID.
///
/// `id` must be a graph UUID (parsed before the wire call). Pathbase 1.1
/// addresses graphs by UUID only; the old slug-style references aren't
/// resolvable here. `Private` graphs 404 unless the caller is
/// owner-authenticated; `Public` and `Unlisted` graphs are readable by
/// anyone with the UUID.
///
/// Returns a serialized JSON string. The generated client decodes into
/// `serde_json::Map`, which we re-serialize on the way out — keys may
/// be reordered relative to the server's bytes, but the consumer parses
/// to `Graph` and re-serializes anyway, so byte-fidelity isn't a real
/// requirement.
pub(crate) fn graphs_download(
    base_url: &str,
    token: Option<&str>,
    owner: &str,
    repo: &str,
    id: &str,
) -> Result<String> {
    let uuid: uuid::Uuid = id
        .parse()
        .with_context(|| format!("not a valid graph UUID: {id}"))?;
    let client = pathbase_client(base_url, token)?;
    match block_on(client.download_graph(owner, repo, &uuid)) {
        Ok(resp) => {
            let map = resp.into_inner();
            serde_json::to_string(&map).context("re-serializing downloaded graph")
        }
        Err(pathbase_client::Error::ErrorResponse(resp)) => match resp.status() {
            reqwest::StatusCode::NOT_FOUND => bail!(
                "{owner}/{repo}/{id} not found on {base_url} (or it's a private graph \
                 and you're not the owner — run `path auth login --url {base_url}`)"
            ),
            status => bail!("download of {owner}/{repo}/{id} failed ({status})"),
        },
        Err(pathbase_client::Error::UnexpectedResponse(resp)) => {
            let status = resp.status();
            let body = block_on(resp.text()).unwrap_or_default();
            let msg = error_message(&body).unwrap_or_else(|| short_body(&body));
            bail!("download of {owner}/{repo}/{id} failed ({status}): {msg}")
        }
        Err(pathbase_client::Error::CommunicationError(e)) => bail!(
            "download of {owner}/{repo}/{id} failed: {}",
            reqwest_hint(&e)
        ),
        Err(e) => Err(anyhow!(
            "download of {owner}/{repo}/{id} failed: {}",
            full_chain(&e)
        )),
    }
}

// ── File storage ────────────────────────────────────────────────────────

pub(crate) fn credentials_path() -> Result<PathBuf> {
    Ok(config_dir()?.join(crate::config::CREDENTIALS_FILE_NAME))
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
pub(crate) mod tests {
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
            },
        }
    }

    #[test]
    fn resolve_url_prefers_cli_flag() {
        let got = resolve_url(Some("https://example.com/".into()));
        assert_eq!(got, "https://example.com");
    }

    #[test]
    fn host_of_strips_path() {
        assert_eq!(host_of("https://pathbase.dev"), "https://pathbase.dev");
        assert_eq!(host_of("https://pathbase.dev/"), "https://pathbase.dev");
        assert_eq!(
            host_of("https://pathbase.dev/api/v1/traces"),
            "https://pathbase.dev"
        );
        assert_eq!(
            host_of("http://127.0.0.1:9000/foo"),
            "http://127.0.0.1:9000"
        );
        assert_eq!(host_of("not-a-url"), "not-a-url");
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
    pub(crate) struct MockServer {
        port: u16,
        thread: Option<std::thread::JoinHandle<Vec<u8>>>,
    }

    impl MockServer {
        pub(crate) fn start(status_line: &'static str, body: &'static str) -> Self {
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

        pub(crate) fn base(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        fn request(mut self) -> Vec<u8> {
            self.thread.take().unwrap().join().unwrap()
        }
    }

    const TEST_UUID: &str = "fe94b6f9-b0af-4cdd-b9ca-3c9a2a697537";
    const TEST_REPO_UUID: &str = "00000000-0000-0000-0000-000000000002";

    /// A `GraphDocumentResponse` body for mock-server responses. Progenitor
    /// strictly validates response shapes against the OpenAPI schema, so the
    /// mock has to return every required field even though the CLI only reads
    /// a few. A bare-minimum toolpath document parses cleanly as
    /// `ToolpathDocument` (`{graph, paths}`).
    fn graph_document_json() -> String {
        format!(
            r#"{{
                "id": "{TEST_UUID}",
                "repo_id": "{TEST_REPO_UUID}",
                "toolpath_id": "tp-1",
                "document": {{"graph": {{"id":"g"}}, "paths": []}},
                "path_count": 0,
                "url": "https://pathbase.dev/u/alex/repos/pathstash/graphs/{TEST_UUID}",
                "visibility": "unlisted",
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z"
            }}"#
        )
    }

    #[test]
    fn graphs_post_wraps_document_with_name_and_visibility() {
        let server = MockServer::start(
            "HTTP/1.1 201 Created",
            Box::leak(graph_document_json().into_boxed_str()),
        );
        let created = graphs_post(
            &server.base(),
            "tok",
            "alex",
            "pathstash",
            Some("my-graph"),
            r#"{"graph":{"id":"g"},"paths":[]}"#,
            false,
        )
        .unwrap();
        assert_eq!(created.id, TEST_UUID);
        assert_eq!(
            created.visibility,
            pathbase_client::types::Visibility::Unlisted
        );

        let req = String::from_utf8(server.request()).unwrap();
        assert!(
            req.starts_with("POST /api/v1/u/alex/repos/pathstash/graphs "),
            "got: {req}"
        );
        assert!(
            req.to_lowercase().contains("authorization: bearer tok"),
            "got: {req}"
        );
        assert!(req.contains(r#""name":"my-graph""#), "got: {req}");
        assert!(req.contains(r#""visibility":"unlisted""#), "got: {req}");
        assert!(
            req.contains(r#""document":{"graph":{"id":"g"},"paths":[]}"#),
            "got: {req}"
        );
    }

    #[test]
    fn graphs_post_401_surfaces_relogin_message() {
        let server = MockServer::start(
            "HTTP/1.1 401 Unauthorized",
            r#"{"code":"unauthorized","error":"bad"}"#,
        );
        let base = server.base();
        let err = graphs_post(
            &base,
            "tok",
            "alex",
            "pathstash",
            None,
            r#"{"graph":{"id":"g"},"paths":[]}"#,
            false,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(&base), "expected base URL in error: {msg}");
        assert!(
            msg.contains("path auth login --url"),
            "expected re-auth hint: {msg}"
        );
        assert!(msg.contains("--anon"), "expected --anon hint: {msg}");
    }

    #[test]
    fn graphs_post_5xx_includes_server_message() {
        let server = MockServer::start(
            "HTTP/1.1 500 Internal Server Error",
            r#"{"error":"database is on fire"}"#,
        );
        let err = graphs_post(
            &server.base(),
            "tok",
            "alex",
            "pathstash",
            None,
            r#"{"graph":{"id":"g"},"paths":[]}"#,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("database is on fire"), "{err}");
    }

    /// Anon upload returns a `GraphDocumentResponse` whose `url` field
    /// is the canonical share link. We expose that field to callers.
    #[test]
    fn anon_graphs_post_wraps_document_and_omits_auth() {
        let server = MockServer::start(
            "HTTP/1.1 201 Created",
            Box::leak(graph_document_json().into_boxed_str()),
        );
        let resp = anon_graphs_post(&server.base(), r#"{"graph":{"id":"g"},"paths":[]}"#).unwrap();
        assert_eq!(resp.id, TEST_UUID);
        assert!(resp.url.ends_with(TEST_UUID));

        let req = String::from_utf8(server.request()).unwrap();
        assert!(
            req.starts_with("POST /api/v1/u/anon/repos/pathstash/graphs "),
            "got: {req}"
        );
        assert!(
            !req.to_lowercase().contains("authorization:"),
            "anon must not send auth header: {req}"
        );
        assert!(
            req.contains(r#""document":{"graph":{"id":"g"},"paths":[]}"#),
            "got: {req}"
        );
    }

    #[test]
    fn anon_graphs_post_413_advises_login() {
        let server = MockServer::start(
            "HTTP/1.1 413 Payload Too Large",
            r#"{"code":"bad_request","error":"body too large"}"#,
        );
        let err =
            anon_graphs_post(&server.base(), r#"{"graph":{"id":"g"},"paths":[]}"#).unwrap_err();
        assert!(err.to_string().contains("size cap"), "{err}");
        assert!(err.to_string().contains("path auth login"), "{err}");
    }

    #[test]
    fn repos_post_treats_409_as_success() {
        let server = MockServer::start(
            "HTTP/1.1 409 Conflict",
            r#"{"code":"conflict","error":"already exists"}"#,
        );
        repos_post(&server.base(), "tok", "alex", "pathstash").unwrap();
    }

    /// Download decodes through `serde_json::Map` and re-serializes, so
    /// keys may be reordered relative to the server's bytes. The
    /// downstream cache writer (`write_cached`) round-trips through
    /// `Graph` and writes pretty-printed JSON anyway, so the only
    /// invariant we care about is "the JSON parses to the same value".
    #[test]
    fn graphs_download_returns_body_as_json() {
        let body = r#"{"graph":{"id":"g"},"paths":[{"path":{"id":"p1","head":"s1"},"steps":[]}]}"#;
        let server = MockServer::start("HTTP/1.1 200 OK", body);
        let got =
            graphs_download(&server.base(), Some("tok"), "alex", "pathstash", TEST_UUID).unwrap();
        let got_v: serde_json::Value = serde_json::from_str(&got).unwrap();
        let want_v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            got_v, want_v,
            "downloaded body should parse to the same value"
        );

        let req = String::from_utf8(server.request()).unwrap();
        let expected_path =
            format!("GET /api/v1/u/alex/repos/pathstash/graphs/{TEST_UUID}/download ");
        assert!(req.starts_with(&expected_path), "got: {req}");
        assert!(
            req.to_lowercase().contains("authorization: bearer tok"),
            "got: {req}"
        );
    }

    #[test]
    fn graphs_download_404_says_not_found() {
        let server = MockServer::start(
            "HTTP/1.1 404 Not Found",
            r#"{"code":"not_found","error":"graph not found"}"#,
        );
        let err = graphs_download(&server.base(), Some("tok"), "alex", "pathstash", TEST_UUID)
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn graphs_download_rejects_non_uuid_id() {
        // Don't even make the network call when the id obviously can't
        // be a graph id under the 1.1+ wire scheme.
        let err = graphs_download(
            "http://127.0.0.1:1",
            Some("tok"),
            "alex",
            "pathstash",
            "my-old-slug",
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a valid graph UUID"), "{err}");
    }

    // ── preflight_auth ────────────────────────────────────────────────
    //
    // The preflight is the gate that decides authed-vs-anon BEFORE the
    // share picker runs, so a credential rejection shouldn't make the
    // user pick a session and *then* fail. These tests use
    // TOOLPATH_CONFIG_DIR + a tempdir-credentials file to drive the
    // logged-in path through the same MockServer used elsewhere.

    fn write_credentials(dir: &std::path::Path, url: &str) {
        let creds = StoredSession {
            url: url.to_string(),
            token: "tok".into(),
            user: User {
                id: "u1".into(),
                username: "alice".into(),
                email: None,
                display_name: None,
            },
        };
        store_session(&dir.join(crate::config::CREDENTIALS_FILE_NAME), &creds).unwrap();
    }

    fn me_response_body(username: &str) -> String {
        // The generated User type requires id (uuid), username, created_at,
        // updated_at. Mock the bare minimum that parses cleanly.
        format!(
            r#"{{"id":"00000000-0000-0000-0000-000000000001","username":"{username}","created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z"}}"#
        )
    }

    /// Cleared TOOLPATH_CONFIG_DIR + no `--anon` + no auth-requiring flags
    /// → preflight returns Anon with the "not logged in" notice.
    #[test]
    fn preflight_anon_when_logged_out_and_no_auth_flags() {
        let cfg = tempfile::tempdir().unwrap();
        let _g = EnvGuard::set("TOOLPATH_CONFIG_DIR", cfg.path().to_str().unwrap());
        let mode = preflight_auth("https://pathbase.dev", false, false).unwrap();
        assert!(matches!(mode, AuthMode::Anon));
    }

    /// Stored credentials AND host matches AND api_me succeeds → Authed.
    #[test]
    fn preflight_authed_when_credentials_validate() {
        let server = MockServer::start(
            "HTTP/1.1 200 OK",
            Box::leak(me_response_body("alice").into_boxed_str()),
        );
        let cfg = tempfile::tempdir().unwrap();
        let _g = EnvGuard::set("TOOLPATH_CONFIG_DIR", cfg.path().to_str().unwrap());
        write_credentials(cfg.path(), &server.base());
        let base = server.base();
        let mode = preflight_auth(&base, false, false).unwrap();
        match mode {
            AuthMode::Authed { username, .. } => assert_eq!(username, "alice"),
            AuthMode::Anon => panic!("expected Authed, got Anon"),
        }
    }

    /// Stored credentials but api_me rejects with 401 + no auth-requiring
    /// flags → fall back to Anon (don't error).
    #[test]
    fn preflight_falls_back_to_anon_on_401_without_auth_flags() {
        let server = MockServer::start("HTTP/1.1 401 Unauthorized", "{}");
        let cfg = tempfile::tempdir().unwrap();
        let _g = EnvGuard::set("TOOLPATH_CONFIG_DIR", cfg.path().to_str().unwrap());
        write_credentials(cfg.path(), &server.base());
        let base = server.base();
        let mode = preflight_auth(&base, false, false).unwrap();
        assert!(matches!(mode, AuthMode::Anon));
    }

    /// Stored credentials but api_me rejects + needs_auth=true → propagate
    /// the error so the user knows --repo/--public/--slug can't be honored.
    #[test]
    fn preflight_propagates_401_when_auth_required() {
        let server = MockServer::start("HTTP/1.1 401 Unauthorized", "{}");
        let cfg = tempfile::tempdir().unwrap();
        let _g = EnvGuard::set("TOOLPATH_CONFIG_DIR", cfg.path().to_str().unwrap());
        write_credentials(cfg.path(), &server.base());
        let base = server.base();
        let err = preflight_auth(&base, false, true).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--repo"), "expected mention of --repo: {msg}");
    }

    /// `--anon` short-circuits past every check.
    #[test]
    fn preflight_anon_flag_skips_credentials_check() {
        // Even with valid credentials in place, --anon returns Anon without
        // calling api_me (no MockServer needed — would 404).
        let cfg = tempfile::tempdir().unwrap();
        let _g = EnvGuard::set("TOOLPATH_CONFIG_DIR", cfg.path().to_str().unwrap());
        write_credentials(cfg.path(), "https://pathbase.dev");
        let mode = preflight_auth("https://pathbase.dev", true, false).unwrap();
        assert!(matches!(mode, AuthMode::Anon));
    }

    /// Test-helper guard for `std::env::set_var`. Process env is shared
    /// across all `cargo test` threads, so concurrent tests that mutate or
    /// read *any* env var would race — `std::env::set_var`/`var_os` are not
    /// thread-safe. `EnvGuard` serializes against every other env-touching
    /// test in the crate via the *shared* [`crate::config::TEST_ENV_LOCK`]
    /// (held for the guard's lifetime), not a private lock: these tests set
    /// `TOOLPATH_CONFIG_DIR`, which `cmd_resume`/`cmd_cache`/`cmd_export`
    /// also read/write under that same lock. A separate mutex here would
    /// only exclude EnvGuard users from each other while still racing those
    /// modules. Drop restores the prior value.
    struct EnvGuard {
        key: String,
        prior: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl EnvGuard {
        fn set(key: &str, val: &str) -> Self {
            // PoisonError on a previously-panicked test still gives us a
            // valid lock — recover the inner guard and proceed.
            let lock = crate::config::TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let prior = std::env::var_os(key);
            // SAFETY: TEST_ENV_LOCK serializes this against every other
            // env-touching test in the crate, so no concurrent
            // set_var/var_os on the shared environ can occur.
            unsafe {
                std::env::set_var(key, val);
            }
            Self {
                key: key.to_string(),
                prior,
                _lock: lock,
            }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var(&self.key, v),
                    None => std::env::remove_var(&self.key),
                }
            }
        }
    }
}
