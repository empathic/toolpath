//! Shared Pathbase client helpers.
//!
//! Wraps the typed `pathbase-client` (generated from
//! `crates/pathbase-client/openapi.json` — refresh via
//! `scripts/refresh-pathbase-openapi.sh`) plus session-storage logic
//! used by `cmd_auth`, `cmd_import`, `cmd_export`, and `cmd_share`.
//! Every Pathbase HTTP call goes through the typed client except the
//! streamed-upload batch routes (`graphs_post_streamed`), which are not
//! in the spec yet and use reqwest directly. Config-dir resolution lives
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
// — runs on a single reqwest version (0.13), the auth flow included:
// `cli_redeem` is in the spec, so `api_redeem` calls the generated
// method like any other operation.

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

/// Build a `pathbase_client::Client` over [`http_client`]. Progenitor
/// doesn't expose a bearer-token setter, so the header is pre-baked into
/// the http client and handed over via `Client::new_with_client`.
fn pathbase_client(base_url: &str, token: Option<&str>) -> Result<pathbase_client::Client> {
    Ok(pathbase_client::Client::new_with_client(
        base_url,
        http_client(token)?,
    ))
}

/// A reqwest client with a 30 s per-request timeout and, when a token is
/// supplied, a default `Authorization: Bearer <token>` header.
fn http_client(token: Option<&str>) -> Result<reqwest::Client> {
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
    builder.build().context("build pathbase http client")
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
                bail!(relogin_message(base_url))
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

fn relogin_message(base_url: &str) -> String {
    format!(
        "{base_url} rejected your stored credentials (HTTP 401). \
         Run `path auth login --url {base_url}` to authenticate against this server, \
         or pass `--anon` to upload anonymously."
    )
}

// ── Streamed upload ─────────────────────────────────────────────────────

/// Largest request body the streamed upload sends, and the document size
/// above which an authed upload is streamed at all.
pub(crate) const BATCH_BUDGET: usize = 4 * 1024 * 1024;

const BATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const BATCH_RETRIES: u32 = 3;
#[cfg(not(test))]
const RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(1);
#[cfg(test)]
const RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(5);

/// One request body of a streamed path upload.
#[derive(Debug, Default, PartialEq)]
struct Batch {
    body: String,
    /// Id and line size of the largest `Step` line, for the 413 message.
    largest_step: Option<(String, usize)>,
}

fn head_line(step_id: &str) -> String {
    let line = toolpath::v1::jsonl::JsonlLine::Head(toolpath::v1::jsonl::HeadBody {
        step_id: step_id.to_string(),
    });
    let mut s = serde_json::to_string(&line).expect("serialize Head line");
    s.push('\n');
    s
}

/// Split the output of `Path::to_jsonl_writer` into request bodies of at
/// most `budget` bytes, cut at line boundaries. `step_ids` are the ids of
/// the `Step` lines in order.
///
/// A step and its `Signature` lines are never separated. Every non-final
/// batch ends with an added `Head` line naming its last step, so the server
/// holds a valid path after each request. A batch is only closed once it
/// contains a step, which keeps `PathOpen` and the `ActorDef` lines with
/// the first step. A step larger than the budget is sent in a batch of its
/// own, over budget.
fn pack_batches(jsonl: &str, step_ids: &[&str], budget: usize) -> Vec<Batch> {
    const STEP: &str = r#"{"Step":"#;
    const STEP_SIGNATURE: &str = r#"{"Signature":{"target":"step:"#;
    let line_end = |s: &str, from: usize| s[from..].find('\n').map_or(s.len(), |i| from + i + 1);

    let mut batches = Vec::new();
    let mut cur = Batch::default();
    let mut last_step: Option<&str> = None;
    let mut ids = step_ids.iter().copied();
    let mut rest = jsonl;
    while !rest.is_empty() {
        let mut end = line_end(rest, 0);
        let step = if rest.starts_with(STEP) {
            ids.next().map(|id| (id, end))
        } else {
            None
        };
        if step.is_some() {
            while rest[end..].starts_with(STEP_SIGNATURE) {
                end = line_end(rest, end);
            }
        }
        let (unit, tail) = rest.split_at(end);
        rest = tail;

        if let Some(last) = last_step {
            let head = head_line(step.map_or(last, |(id, _)| id));
            if cur.body.len() + unit.len() + head.len() > budget {
                cur.body.push_str(&head_line(last));
                batches.push(std::mem::take(&mut cur));
                last_step = None;
            }
        }
        cur.body.push_str(unit);
        if let Some((id, len)) = step {
            last_step = Some(id);
            if cur.largest_step.as_ref().is_none_or(|(_, l)| len > *l) {
                cur.largest_step = Some((id.to_string(), len));
            }
        }
    }
    if !cur.body.is_empty() {
        batches.push(cur);
    }
    batches
}

/// Step indices in a stable parents-first order, or `None` when the steps
/// already are. Parents outside the path are ignored.
fn parents_first_order(steps: &[toolpath::v1::Step]) -> Option<Vec<usize>> {
    use std::collections::{HashMap, HashSet};
    let index: HashMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.step.id.as_str(), i))
        .collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let ordered = steps.iter().all(|s| {
        let ok = s
            .step
            .parents
            .iter()
            .all(|p| !index.contains_key(p.as_str()) || seen.contains(p.as_str()));
        seen.insert(s.step.id.as_str());
        ok
    });
    if ordered {
        return None;
    }

    #[derive(Clone, Copy, PartialEq)]
    enum State {
        New,
        Open,
        Done,
    }
    let mut state = vec![State::New; steps.len()];
    let mut order = Vec::with_capacity(steps.len());
    for root in 0..steps.len() {
        let mut stack = vec![root];
        while let Some(&i) = stack.last() {
            if state[i] == State::Done {
                stack.pop();
                continue;
            }
            state[i] = State::Open;
            let pending = steps[i]
                .step
                .parents
                .iter()
                .filter_map(|p| index.get(p.as_str()).copied())
                .find(|&p| state[p] == State::New);
            match pending {
                Some(p) => stack.push(p),
                None => {
                    state[i] = State::Done;
                    order.push(i);
                    stack.pop();
                }
            }
        }
    }
    Some(order)
}

enum BatchFailure {
    Transport(reqwest::Error),
    Status(u16, String),
}

impl BatchFailure {
    fn retryable(&self) -> bool {
        match self {
            BatchFailure::Transport(_) => true,
            BatchFailure::Status(code, _) => *code >= 500,
        }
    }

    fn describe(&self) -> String {
        match self {
            BatchFailure::Transport(e) if e.is_timeout() => {
                format!("request timed out after {}s", BATCH_TIMEOUT.as_secs())
            }
            BatchFailure::Transport(e) => reqwest_hint(e),
            BatchFailure::Status(code, msg) if msg.is_empty() => format!("HTTP {code}"),
            BatchFailure::Status(code, msg) => format!("HTTP {code}: {msg}"),
        }
    }
}

fn post_batch_once(
    http: &reqwest::Client,
    url: &str,
    body: &str,
) -> std::result::Result<serde_json::Value, BatchFailure> {
    block_on(async {
        let resp = http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/x-ndjson")
            .timeout(BATCH_TIMEOUT)
            .body(body.to_owned())
            .send()
            .await
            .map_err(BatchFailure::Transport)?;
        let status = resp.status();
        let text = resp.text().await.map_err(BatchFailure::Transport)?;
        if status.is_success() {
            Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
        } else {
            let msg = error_message(&text).unwrap_or_else(|| short_body(&text));
            Err(BatchFailure::Status(status.as_u16(), msg))
        }
    })
}

/// POST one batch, retrying transport errors and 5xx responses. Both batch
/// routes are idempotent for a replayed body, so a retry after a lost
/// response is safe.
fn post_batch(
    http: &reqwest::Client,
    url: &str,
    body: &str,
) -> std::result::Result<serde_json::Value, BatchFailure> {
    let mut retries = 0;
    loop {
        match post_batch_once(http, url, body) {
            Err(f) if f.retryable() && retries < BATCH_RETRIES => {
                retries += 1;
                eprintln!(
                    "  batch failed ({}); retry {retries}/{BATCH_RETRIES}",
                    f.describe()
                );
                std::thread::sleep(RETRY_BACKOFF * 2u32.pow(retries - 1));
            }
            other => return other,
        }
    }
}

/// Upload a graph too large for one request: create it with `paths: []`,
/// then send each inline path as batches of RFC-jsonl lines of at most
/// `budget` bytes. The first batch of a path opens it
/// (`POST …/graphs/{id}/paths`); the rest append to it
/// (`POST …/graphs/{id}/paths/{path_id}/steps`).
///
/// If any batch fails, the partly uploaded graph is deleted (best effort)
/// before the error is returned. A `404` or `405` on a path's first batch
/// means the server lacks the batch routes; the whole document is then
/// sent with [`graphs_post`] instead. `$ref` path entries are skipped; callers
/// route documents containing them to [`graphs_post`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn graphs_post_streamed(
    base_url: &str,
    token: &str,
    owner: &str,
    repo: &str,
    name: Option<&str>,
    doc: &toolpath::v1::Graph,
    public: bool,
    budget: usize,
) -> Result<CreatedGraph> {
    let shell = toolpath::v1::Graph {
        graph: doc.graph.clone(),
        paths: Vec::new(),
        meta: doc.meta.clone(),
    };
    let shell_json = serde_json::to_string(&shell).context("serialize graph")?;
    let created = graphs_post(base_url, token, owner, repo, name, &shell_json, public)?;

    let http = http_client(Some(token))?;
    let mut graph_url = reqwest::Url::parse(base_url).context("parse pathbase url")?;
    graph_url
        .path_segments_mut()
        .map_err(|_| anyhow!("pathbase url cannot be a base: {base_url}"))?
        .pop_if_empty()
        .extend([
            "api",
            "v1",
            "u",
            owner,
            "repos",
            repo,
            "graphs",
            &created.id,
        ]);
    let graph_url = graph_url.as_str();
    match stream_paths(&http, graph_url, doc, budget) {
        Ok(()) => Ok(created),
        Err(e) => {
            let _ = block_on(async { http.delete(graph_url).send().await });
            match (&e.failure, e.largest_step) {
                (BatchFailure::Status(404 | 405, _), _) if e.opening_path => {
                    eprintln!(
                        "note: {base_url} does not support streamed upload; \
                         sending the document in one request"
                    );
                    let json = serde_json::to_string(doc).context("serialize graph")?;
                    graphs_post(base_url, token, owner, repo, name, &json, public)
                }
                (BatchFailure::Status(401, _), _) => bail!(relogin_message(base_url)),
                (BatchFailure::Status(413, _), Some((id, len))) => bail!(
                    "upload to {owner}/{repo} failed (HTTP 413): step {id} is {len} bytes, \
                     larger than the server accepts in one request"
                ),
                _ => bail!("upload to {owner}/{repo} failed: {}", e.failure.describe()),
            }
        }
    }
}

struct StreamError {
    failure: BatchFailure,
    /// Largest step of the failing batch.
    largest_step: Option<(String, usize)>,
    /// The failing request was a path's first batch (`POST …/paths`).
    opening_path: bool,
}

fn stream_paths(
    http: &reqwest::Client,
    graph_url: &str,
    doc: &toolpath::v1::Graph,
    budget: usize,
) -> std::result::Result<(), StreamError> {
    use toolpath::v1::PathOrRef;

    let paths: Vec<&toolpath::v1::Path> = doc
        .paths
        .iter()
        .filter_map(|p| match p {
            PathOrRef::Path(p) => Some(p.as_ref()),
            PathOrRef::Ref(_) => None,
        })
        .collect();
    for (pi, path) in paths.iter().enumerate() {
        let reordered;
        let path = match parents_first_order(&path.steps) {
            Some(order) => {
                reordered = toolpath::v1::Path {
                    path: path.path.clone(),
                    steps: order.iter().map(|&i| path.steps[i].clone()).collect(),
                    meta: path.meta.clone(),
                };
                &reordered
            }
            None => *path,
        };
        let jsonl = path.to_jsonl_string().expect("write jsonl to memory");
        let step_ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
        let batches = pack_batches(&jsonl, &step_ids, budget);

        let mut steps_url = String::new();
        for (bi, batch) in batches.iter().enumerate() {
            eprintln!(
                "Uploading path {}/{}, batch {}/{} ({} bytes)",
                pi + 1,
                paths.len(),
                bi + 1,
                batches.len(),
                batch.body.len()
            );
            let url = if bi == 0 {
                format!("{graph_url}/paths")
            } else {
                steps_url.clone()
            };
            let resp = post_batch(http, &url, &batch.body).map_err(|failure| StreamError {
                failure,
                largest_step: batch.largest_step.clone(),
                opening_path: bi == 0,
            })?;
            if bi == 0 {
                let Some(path_id) = resp.get("path_id").and_then(|v| v.as_str()) else {
                    return Err(StreamError {
                        failure: BatchFailure::Status(
                            200,
                            "server response to the first batch has no path_id".to_string(),
                        ),
                        largest_step: None,
                        opening_path: false,
                    });
                };
                steps_url = format!("{graph_url}/paths/{path_id}/steps");
            }
        }
    }
    Ok(())
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
            401 => bail!(relogin_message(base_url)),
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
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // Open already restricted to 0600 so there is never a moment
        // where the token exists world/group-readable under a permissive
        // umask. `truncate` rather than `create_new`: the file is
        // rewritten in place on every login.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("open {}", path.display()))?;
        f.write_all(payload.as_bytes())
            .with_context(|| format!("write {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, payload).with_context(|| format!("write {}", path.display()))?;
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

    /// A canned HTTP/1.1 responder. Binds to 127.0.0.1 on a free port and
    /// serves one connection per scripted response: reads one request
    /// (headers + body), writes the response, closes.
    pub(crate) struct MockServer {
        port: u16,
        thread: Option<std::thread::JoinHandle<Vec<Vec<u8>>>>,
    }

    impl MockServer {
        pub(crate) fn start(status_line: &'static str, body: &'static str) -> Self {
            Self::start_sequence(vec![(status_line, body.to_string())])
        }

        /// Serve `responses` in order, one connection each. An empty status
        /// line closes that connection without responding.
        pub(crate) fn start_sequence(responses: Vec<(&'static str, String)>) -> Self {
            use std::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let thread = std::thread::spawn(move || {
                responses
                    .into_iter()
                    .map(|(status_line, body)| {
                        let (stream, _addr) = listener.accept().unwrap();
                        Self::serve(stream, status_line, &body)
                    })
                    .collect()
            });
            MockServer {
                port,
                thread: Some(thread),
            }
        }

        fn serve(mut stream: std::net::TcpStream, status_line: &str, body: &str) -> Vec<u8> {
            use std::io::{BufRead, BufReader, Write};
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

            if !status_line.is_empty() {
                let response = format!(
                    "{status_line}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
            req
        }

        pub(crate) fn base(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        pub(crate) fn request(self) -> Vec<u8> {
            self.requests().remove(0)
        }

        pub(crate) fn requests(mut self) -> Vec<Vec<u8>> {
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
    pub(crate) fn graph_document_json() -> String {
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
    // ── Streamed upload ──────────────────────────────────────────────

    fn signature(sig: &str) -> toolpath::v1::Signature {
        toolpath::v1::Signature {
            signer: "human:alex".to_string(),
            key: "key-1".to_string(),
            scope: "author".to_string(),
            sig: sig.to_string(),
            timestamp: None,
        }
    }

    /// A linear path `s0 → s1 → …` whose step lines are roughly
    /// `step_bytes` long. `s1` carries two signatures and the path one.
    fn stream_path(steps: usize, step_bytes: usize) -> toolpath::v1::Path {
        use toolpath::v1::{Path, PathIdentity, PathMeta, Step, StepMeta};
        let steps: Vec<Step> = (0..steps)
            .map(|i| {
                let mut step = Step::new(format!("s{i}"), "human:alex", "2024-01-01T00:00:00Z")
                    .with_raw_change("src/main.rs", "x".repeat(step_bytes));
                if i > 0 {
                    step = step.with_parent(format!("s{}", i - 1));
                }
                if i == 1 {
                    step.meta = Some(StepMeta {
                        signatures: vec![signature("step-a"), signature("step-b")],
                        ..Default::default()
                    });
                }
                step
            })
            .collect();
        Path {
            path: PathIdentity {
                id: "p".to_string(),
                base: None,
                head: steps.last().unwrap().step.id.clone(),
                graph_ref: None,
            },
            steps,
            meta: Some(PathMeta {
                title: Some("streamed".to_string()),
                signatures: vec![signature("path-a")],
                ..Default::default()
            }),
        }
    }

    fn pack(path: &toolpath::v1::Path, budget: usize) -> Vec<Batch> {
        let ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
        pack_batches(&path.to_jsonl_string().unwrap(), &ids, budget)
    }

    fn step_lines(batch: &Batch) -> usize {
        batch
            .body
            .lines()
            .filter(|l| l.starts_with(r#"{"Step":"#))
            .count()
    }

    #[test]
    fn pack_batches_concatenation_reads_back_to_the_path() {
        let path = stream_path(12, 200);
        let batches = pack(&path, 1000);
        assert!(batches.len() > 3, "got {} batches", batches.len());
        for b in &batches {
            assert!(b.body.len() <= 1000, "batch is {} bytes", b.body.len());
        }

        let all: String = batches.iter().map(|b| b.body.as_str()).collect();
        let read = toolpath::v1::Path::from_jsonl_str(&all).unwrap();
        assert_eq!(
            serde_json::to_value(&read).unwrap(),
            serde_json::to_value(&path).unwrap()
        );
    }

    #[test]
    fn pack_batches_every_prefix_has_a_stored_head() {
        let path = stream_path(12, 200);
        let batches = pack(&path, 1000);
        let mut prefix = String::new();
        for b in &batches {
            prefix.push_str(&b.body);
            let read = toolpath::v1::Path::from_jsonl_str(&prefix).unwrap();
            assert_eq!(
                read.path.head,
                read.steps.last().unwrap().step.id,
                "head should name the last step sent so far"
            );
        }
    }

    #[test]
    fn pack_batches_sends_an_oversized_step_alone() {
        let mut path = stream_path(6, 200);
        path.steps[3] = toolpath::v1::Step::new("s3", "human:alex", "2024-01-01T00:00:00Z")
            .with_parent("s2")
            .with_raw_change("src/main.rs", "y".repeat(5000));
        let batches = pack(&path, 1000);

        let big = batches
            .iter()
            .find(|b| b.body.contains(r#""id":"s3""#))
            .unwrap();
        assert_eq!(step_lines(big), 1);
        assert!(big.body.len() > 1000);
        let (id, len) = big.largest_step.clone().unwrap();
        assert_eq!(id, "s3");
        assert_eq!(len, big.body.lines().next().unwrap().len() + 1);
        assert!(big.body.ends_with(&head_line("s3")));

        let all: String = batches.iter().map(|b| b.body.as_str()).collect();
        let read = toolpath::v1::Path::from_jsonl_str(&all).unwrap();
        assert_eq!(read.steps.len(), 6);
    }

    #[test]
    fn pack_batches_keeps_signatures_with_their_step() {
        let path = stream_path(4, 200);
        let line_len = |needle: &str| {
            let jsonl = path.to_jsonl_string().unwrap();
            jsonl.lines().find(|l| l.contains(needle)).unwrap().len() + 1
        };
        // Room for s1's step line and one signature, but not both signatures.
        let budget = line_len(r#""id":"s1""#) + line_len("step-a") + head_line("s1").len() + 10;
        let batches = pack(&path, budget);

        let with_s1 = batches
            .iter()
            .find(|b| b.body.contains(r#""id":"s1""#))
            .unwrap();
        assert!(with_s1.body.contains("step-a"));
        assert!(with_s1.body.contains("step-b"));
        assert_eq!(step_lines(with_s1), 1);
    }

    #[test]
    fn pack_batches_keeps_path_open_with_the_first_step() {
        let path = stream_path(3, 200);
        let batches = pack(&path, 50);
        assert!(batches[0].body.starts_with(r#"{"PathOpen":"#));
        assert_eq!(step_lines(&batches[0]), 1);
    }

    #[test]
    fn parents_first_order_leaves_ordered_steps_alone() {
        let path = stream_path(5, 10);
        assert!(parents_first_order(&path.steps).is_none());

        let external = vec![
            toolpath::v1::Step::new("a", "human:alex", "2024-01-01T00:00:00Z")
                .with_parent("not-in-this-path"),
        ];
        assert!(parents_first_order(&external).is_none());
    }

    #[test]
    fn parents_first_order_moves_only_what_it_must() {
        let step = |id: &str, parent: Option<&str>| {
            let s = toolpath::v1::Step::new(id, "human:alex", "2024-01-01T00:00:00Z");
            match parent {
                Some(p) => s.with_parent(p),
                None => s,
            }
        };
        let steps = vec![
            step("a", None),
            step("c", Some("b")),
            step("b", Some("a")),
            step("d", Some("a")),
        ];
        let order = parents_first_order(&steps).unwrap();
        let ids: Vec<&str> = order.iter().map(|&i| steps[i].step.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "d"]);
    }

    fn request_line(req: &[u8]) -> String {
        let text = String::from_utf8_lossy(req);
        text.lines().next().unwrap_or_default().to_string()
    }

    fn request_body(req: &[u8]) -> String {
        let text = String::from_utf8_lossy(req);
        text.split_once("\r\n\r\n").unwrap().1.to_string()
    }

    const PATH_OPENED: &str = r#"{"path_id":"11111111-1111-1111-1111-111111111111","inserted":1,"head":"s0","generation":1}"#;
    const STEPS_APPENDED: &str = r#"{"inserted":1,"head":"s1","generation":2}"#;
    const GRAPH_ROUTE: &str =
        "/api/v1/u/alex/repos/pathstash/graphs/fe94b6f9-b0af-4cdd-b9ca-3c9a2a697537";

    fn post_streamed(
        server: &MockServer,
        path: &toolpath::v1::Path,
        budget: usize,
    ) -> Result<CreatedGraph> {
        let doc = toolpath::v1::Graph::from_path(path.clone());
        graphs_post_streamed(
            &server.base(),
            "tok",
            "alex",
            "pathstash",
            Some("big"),
            &doc,
            false,
            budget,
        )
    }

    #[test]
    fn graphs_post_streamed_sends_graph_then_batches() {
        let path = stream_path(8, 200);
        let batches = pack(&path, 1000);
        assert!(batches.len() >= 3);

        let mut responses = vec![
            ("HTTP/1.1 201 Created", graph_document_json()),
            ("HTTP/1.1 201 Created", PATH_OPENED.to_string()),
        ];
        responses.extend((2..=batches.len()).map(|_| ("HTTP/1.1 200 OK", STEPS_APPENDED.into())));
        let server = MockServer::start_sequence(responses);

        let created = post_streamed(&server, &path, 1000).unwrap();
        assert_eq!(created.id, TEST_UUID);

        let reqs = server.requests();
        assert_eq!(reqs.len(), batches.len() + 1);

        assert_eq!(
            request_line(&reqs[0]),
            "POST /api/v1/u/alex/repos/pathstash/graphs HTTP/1.1"
        );
        let create: serde_json::Value = serde_json::from_str(&request_body(&reqs[0])).unwrap();
        assert_eq!(create["name"], "big");
        assert_eq!(create["visibility"], "unlisted");
        assert_eq!(create["document"]["paths"], serde_json::json!([]));
        assert_eq!(create["document"]["graph"]["id"], "p");

        assert_eq!(
            request_line(&reqs[1]),
            format!("POST {GRAPH_ROUTE}/paths HTTP/1.1")
        );
        for (req, batch) in reqs[1..].iter().zip(&batches) {
            let head = String::from_utf8_lossy(req).to_lowercase();
            assert!(
                head.contains("content-type: application/x-ndjson"),
                "{head}"
            );
            assert!(head.contains("authorization: bearer tok"), "{head}");
            assert_eq!(request_body(req), batch.body);
        }
        for req in &reqs[2..] {
            assert_eq!(
                request_line(req),
                format!(
                    "POST {GRAPH_ROUTE}/paths/11111111-1111-1111-1111-111111111111/steps HTTP/1.1"
                )
            );
        }
    }

    #[test]
    fn graphs_post_streamed_reorders_children_sent_before_parents() {
        let mut path = stream_path(3, 50);
        path.steps.swap(1, 2);
        let server = MockServer::start_sequence(vec![
            ("HTTP/1.1 201 Created", graph_document_json()),
            ("HTTP/1.1 201 Created", PATH_OPENED.to_string()),
        ]);
        post_streamed(&server, &path, BATCH_BUDGET).unwrap();

        let body = request_body(&server.requests()[1]);
        let s1 = body.find(r#""id":"s1""#).unwrap();
        let s2 = body.find(r#""id":"s2""#).unwrap();
        assert!(s1 < s2, "{body}");
    }

    #[test]
    fn graphs_post_streamed_retries_dropped_response_and_5xx() {
        let path = stream_path(2, 50);
        let server = MockServer::start_sequence(vec![
            ("HTTP/1.1 201 Created", graph_document_json()),
            ("", String::new()),
            (
                "HTTP/1.1 503 Service Unavailable",
                r#"{"error":"busy"}"#.into(),
            ),
            ("HTTP/1.1 200 OK", PATH_OPENED.to_string()),
        ]);
        post_streamed(&server, &path, BATCH_BUDGET).unwrap();

        let reqs = server.requests();
        assert_eq!(reqs.len(), 4);
        for req in &reqs[1..] {
            assert_eq!(
                request_line(req),
                format!("POST {GRAPH_ROUTE}/paths HTTP/1.1")
            );
            assert_eq!(request_body(req), request_body(&reqs[1]));
        }
    }

    #[test]
    fn graphs_post_streamed_gives_up_after_three_retries_and_deletes() {
        let path = stream_path(2, 50);
        let busy = || {
            (
                "HTTP/1.1 503 Service Unavailable",
                r#"{"error":"busy"}"#.to_string(),
            )
        };
        let server = MockServer::start_sequence(vec![
            ("HTTP/1.1 201 Created", graph_document_json()),
            busy(),
            busy(),
            busy(),
            busy(),
            ("HTTP/1.1 204 No Content", String::new()),
        ]);
        let err = post_streamed(&server, &path, BATCH_BUDGET).unwrap_err();
        assert!(err.to_string().contains("HTTP 503: busy"), "{err}");

        let reqs = server.requests();
        assert_eq!(
            request_line(&reqs[5]),
            format!("DELETE {GRAPH_ROUTE} HTTP/1.1")
        );
    }

    #[test]
    fn graphs_post_streamed_deletes_graph_after_a_400() {
        let path = stream_path(8, 200);
        let server = MockServer::start_sequence(vec![
            ("HTTP/1.1 201 Created", graph_document_json()),
            ("HTTP/1.1 201 Created", PATH_OPENED.to_string()),
            (
                "HTTP/1.1 400 Bad Request",
                r#"{"code":"bad_request","error":"line 2: malformed Step"}"#.into(),
            ),
            ("HTTP/1.1 204 No Content", String::new()),
        ]);
        let err = post_streamed(&server, &path, 1000).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("alex/pathstash"), "{msg}");
        assert!(msg.contains("HTTP 400: line 2: malformed Step"), "{msg}");

        let reqs = server.requests();
        assert_eq!(reqs.len(), 4);
        assert_eq!(
            request_line(&reqs[3]),
            format!("DELETE {GRAPH_ROUTE} HTTP/1.1")
        );
        assert!(
            String::from_utf8_lossy(&reqs[3])
                .to_lowercase()
                .contains("authorization: bearer tok")
        );
    }

    #[test]
    fn graphs_post_streamed_413_names_the_step_and_its_size() {
        let mut path = stream_path(3, 50);
        path.steps[2] = toolpath::v1::Step::new("s2", "human:alex", "2024-01-01T00:00:00Z")
            .with_parent("s1")
            .with_raw_change("src/main.rs", "y".repeat(5000));
        let size = path
            .to_jsonl_string()
            .unwrap()
            .lines()
            .find(|l| l.contains(r#""id":"s2""#))
            .unwrap()
            .len()
            + 1;
        let server = MockServer::start_sequence(vec![
            ("HTTP/1.1 201 Created", graph_document_json()),
            ("HTTP/1.1 413 Payload Too Large", String::new()),
            ("HTTP/1.1 204 No Content", String::new()),
        ]);
        let err = post_streamed(&server, &path, BATCH_BUDGET).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("HTTP 413"), "{msg}");
        assert!(msg.contains(&format!("step s2 is {size} bytes")), "{msg}");
        assert_eq!(server.requests().len(), 3);
    }
    #[test]
    fn graphs_post_streamed_falls_back_when_the_open_route_is_missing() {
        let path = stream_path(8, 200);
        let server = MockServer::start_sequence(vec![
            ("HTTP/1.1 201 Created", graph_document_json()),
            ("HTTP/1.1 405 Method Not Allowed", String::new()),
            ("HTTP/1.1 204 No Content", String::new()),
            ("HTTP/1.1 201 Created", graph_document_json()),
        ]);
        let created = post_streamed(&server, &path, 1000).unwrap();
        assert_eq!(created.id, TEST_UUID);

        let reqs = server.requests();
        assert_eq!(reqs.len(), 4);
        assert_eq!(
            request_line(&reqs[2]),
            format!("DELETE {GRAPH_ROUTE} HTTP/1.1")
        );
        assert_eq!(
            request_line(&reqs[3]),
            "POST /api/v1/u/alex/repos/pathstash/graphs HTTP/1.1"
        );
        let full: serde_json::Value = serde_json::from_str(&request_body(&reqs[3])).unwrap();
        assert_eq!(full["name"], "big");
        assert_eq!(
            full["document"],
            serde_json::to_value(toolpath::v1::Graph::from_path(path)).unwrap()
        );
    }

    #[test]
    fn graphs_post_streamed_does_not_fall_back_on_a_400_from_open() {
        let path = stream_path(8, 200);
        let server = MockServer::start_sequence(vec![
            ("HTTP/1.1 201 Created", graph_document_json()),
            (
                "HTTP/1.1 400 Bad Request",
                r#"{"code":"bad_request","error":"line 1: not a PathOpen"}"#.into(),
            ),
            ("HTTP/1.1 204 No Content", String::new()),
        ]);
        let err = post_streamed(&server, &path, 1000).unwrap_err();
        assert!(err.to_string().contains("line 1: not a PathOpen"), "{err}");

        let reqs = server.requests();
        assert_eq!(reqs.len(), 3);
        assert_eq!(
            request_line(&reqs[2]),
            format!("DELETE {GRAPH_ROUTE} HTTP/1.1")
        );
    }

    #[test]
    fn graphs_post_streamed_does_not_fall_back_on_a_404_from_a_later_batch() {
        let path = stream_path(8, 200);
        let server = MockServer::start_sequence(vec![
            ("HTTP/1.1 201 Created", graph_document_json()),
            ("HTTP/1.1 201 Created", PATH_OPENED.to_string()),
            ("HTTP/1.1 404 Not Found", String::new()),
            ("HTTP/1.1 204 No Content", String::new()),
        ]);
        let err = post_streamed(&server, &path, 1000).unwrap_err();
        assert!(err.to_string().contains("HTTP 404"), "{err}");
        assert_eq!(server.requests().len(), 4);
    }

    #[test]
    fn graphs_post_streamed_percent_encodes_owner_and_repo() {
        let path = stream_path(2, 50);
        let server = MockServer::start_sequence(vec![
            ("HTTP/1.1 201 Created", graph_document_json()),
            ("HTTP/1.1 201 Created", PATH_OPENED.to_string()),
        ]);
        let doc = toolpath::v1::Graph::from_path(path);
        graphs_post_streamed(
            &server.base(),
            "tok",
            "al ex",
            "path/stash",
            None,
            &doc,
            false,
            BATCH_BUDGET,
        )
        .unwrap();
        assert_eq!(
            request_line(&server.requests()[1]),
            format!("POST /api/v1/u/al%20ex/repos/path%2Fstash/graphs/{TEST_UUID}/paths HTTP/1.1")
        );
    }
}
