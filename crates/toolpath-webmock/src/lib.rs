//! Record/replay HTTP mock for the model APIs and Pathbase endpoints the harness cells call.
//!
//! One loopback server, two modes:
//!
//! * **record** — forward every request to an upstream base URL, stream the response back
//!   byte-for-byte, then write a cassette: the request (headers minus credentials, body), the
//!   response (status, headers, the stream as an ordered list of chunks), and the normalized form
//!   the key hashes.
//! * **replay** — never open an upstream connection. Look the normalized request up, serve the
//!   stored status/headers and the stored chunks in order (so SSE framing is what the agent saw).
//!   A miss is a `599` whose JSON body names the key, plus `miss-<ts>.json` next to the cassettes.
//!
//! The request key is `sha256({method, path+query, normalized body})`. Normalization drops the
//! body fields in [`VOLATILE`] (ids and timestamps agents mint per call) and replaces ISO dates in
//! strings. A looser key `{method, path, model, last user text}` is tried on an exact miss.
//!
//! The Bun prototype this is ported from (and the per-agent base-URL overrides, the proofs and the
//! limits) is described in the scratchpad review `webmock.md`; this crate is the committed,
//! regenerable form of it: cassettes live under `cassettes/<agent>/` and the recorder is the same
//! binary (`WEBMOCK_MODE=record`), so every fixture can be re-recorded from the repo.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{HeaderMap, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// Headers never written to disk (request or response).
pub const SECRET_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    // Gemini's API key travels in this header, not in `authorization`. The first gemini
    // recording wrote the real key to disk before this entry existed.
    "x-goog-api-key",
    "api-key",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "chatgpt-account-id",
    "openai-organization",
    "x-github-token",
    // The Gemini API answers with the account id behind the key. An identifier, not a credential,
    // but it ties a public cassette to one Google account; the first gemini recordings kept it.
    "x-gemini-api-privileged-user-id",
];

/// A header is a secret when it is listed above or when its name ends in `-key`, `-token`
/// or `-secret`: a provider's new credential header must never reach a cassette by default.
pub fn is_secret_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SECRET_HEADERS.contains(&n.as_str())
        || n.ends_with("-key")
        || n.ends_with("-token")
        || n.ends_with("-secret")
}

/// Hop-by-hop / framing headers never forwarded or stored. Request `content-encoding` is NOT
/// here: codex zstd-compresses its request bodies and forwarding the bytes without that header
/// gets a 400 from chatgpt.com. Responses are requested as identity, so it never appears there.
pub const HOP_HEADERS: &[&str] = &[
    "host",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "content-length",
    "accept-encoding",
    "upgrade",
    "te",
    "trailer",
    "proxy-connection",
];

/// Body keys dropped before hashing. Each entry is an id or clock an agent mints per call; see
/// the review doc for which agent needed which.
pub const VOLATILE: &[&str] = &[
    "request_id",
    "client_request_id",
    "idempotency_key",
    "nonce",
    "timestamp",
    "created_at",
    "created",
    "trace_id",
    "span_id",
    "turn_id",
    "message_id",
    "device_id",
    // codex 0.162 Responses body
    "client_metadata",
    "x-codex-turn-metadata",
    "root_turn_id",
    "context_window_id",
    "turn_started_at_unix_ms",
    "id",
    "internal_chat_message_metadata_passthrough",
    "create_time",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub t_ms: u64,
    pub b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedRequest {
    pub method: String,
    /// path + query, as the agent sent it
    pub url: String,
    pub upstream: String,
    pub headers: HashMap<String, String>,
    /// decoded (identity) body text
    pub body: String,
    #[serde(default)]
    pub body_encoding: String,
    pub normalized: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub chunks: Vec<Chunk>,
    pub bytes: usize,
    pub duration_ms: u64,
    pub sse: bool,
    pub text_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cassette {
    pub agent: String,
    pub recorded_at: String,
    pub key: String,
    pub loose_key: String,
    #[serde(default)]
    pub model: String,
    pub request: RecordedRequest,
    pub response: RecordedResponse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keys {
    pub exact: String,
    pub loose: String,
    pub normalized: Value,
    pub model: String,
}

fn sha_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// `2026-10-15`, `2026-10-15T19:21:09.962Z`, `2026-10-15 19:21` → `<DATE>`.
fn replace_dates(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let is_d = |c: u8| c.is_ascii_digit();
    while i < b.len() {
        // 20dd-dd-dd
        if i + 10 <= b.len()
            && b[i] == b'2'
            && b[i + 1] == b'0'
            && is_d(b[i + 2])
            && is_d(b[i + 3])
            && b[i + 4] == b'-'
            && is_d(b[i + 5])
            && is_d(b[i + 6])
            && b[i + 7] == b'-'
            && is_d(b[i + 8])
            && is_d(b[i + 9])
            && (i == 0 || !b[i - 1].is_ascii_alphanumeric())
        {
            let mut j = i + 10;
            // optional time: [T ]hh:mm[:ss[.fff]][Z|±hh[:]mm]
            if j + 6 <= b.len()
                && (b[j] == b'T' || b[j] == b' ')
                && is_d(b[j + 1])
                && is_d(b[j + 2])
                && b[j + 3] == b':'
                && is_d(b[j + 4])
                && is_d(b[j + 5])
            {
                j += 6;
                if j + 3 <= b.len() && b[j] == b':' && is_d(b[j + 1]) && is_d(b[j + 2]) {
                    j += 3;
                    if j < b.len() && b[j] == b'.' {
                        j += 1;
                        while j < b.len() && is_d(b[j]) {
                            j += 1;
                        }
                    }
                }
                if j < b.len() && b[j] == b'Z' {
                    j += 1;
                } else if j + 3 <= b.len()
                    && (b[j] == b'+' || b[j] == b'-')
                    && is_d(b[j + 1])
                    && is_d(b[j + 2])
                {
                    j += 3;
                    if j < b.len() && b[j] == b':' {
                        j += 1;
                    }
                    if j + 2 <= b.len() && is_d(b[j]) && is_d(b[j + 1]) {
                        j += 2;
                    }
                }
            }
            if j >= b.len() || !b[j].is_ascii_alphanumeric() {
                out.push_str("<DATE>");
                i = j;
                continue;
            }
        }
        // push one UTF-8 char
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Sorted keys, [`VOLATILE`] dropped, dates in strings replaced.
pub fn normalize(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(normalize).collect()),
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for k in keys {
                if VOLATILE.contains(&k.as_str()) {
                    continue;
                }
                out.insert(k.clone(), normalize(&o[k]));
            }
            Value::Object(out)
        }
        Value::String(s) => Value::String(replace_dates(s)),
        other => other.clone(),
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(text_of).collect::<Vec<_>>().join("\n"),
        Value::Object(o) => {
            if let Some(Value::String(t)) = o.get("text") {
                t.clone()
            } else if let Some(c) = o.get("content") {
                text_of(c)
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

/// The last user-authored text in a Messages (`messages`), Responses (`input`) or Gemini
/// (`contents`) body.
pub fn last_user_text(body: &Value) -> String {
    let list = body
        .get("messages")
        .or_else(|| body.get("input"))
        .or_else(|| body.get("contents"))
        .and_then(Value::as_array);
    let Some(list) = list else {
        return String::new();
    };
    for m in list.iter().rev() {
        if m.get("role").and_then(Value::as_str) == Some("user") {
            return text_of(
                m.get("content")
                    .or_else(|| m.get("parts"))
                    .or_else(|| m.get("text"))
                    .unwrap_or(&Value::Null),
            );
        }
    }
    String::new()
}

/// Exact and loose keys for one request.
pub fn keys(method: &str, path_and_query: &str, body_text: &str) -> Keys {
    let parsed: Option<Value> = if body_text.is_empty() {
        None
    } else {
        serde_json::from_str(body_text).ok()
    };
    let normalized = match &parsed {
        Some(v) => normalize(v),
        None => Value::String(body_text.to_string()),
    };
    let model = parsed
        .as_ref()
        .and_then(|v| v.get("model"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let path = path_and_query.split('?').next().unwrap_or("");
    let exact = sha_hex(
        &serde_json::json!({"method": method, "path": path_and_query, "body": normalized})
            .to_string(),
    );
    let loose = sha_hex(
        &serde_json::json!({"method": method, "path": path, "model": model, "last_user": parsed.as_ref().map(last_user_text).unwrap_or_default()}).to_string(),
    );
    Keys {
        exact,
        loose,
        normalized,
        model,
    }
}

/// The body as text whatever its `content-encoding` was (zstd only with the `zstd-bodies` feature).
pub fn decode_body(body: &[u8], encoding: Option<&str>) -> (String, String) {
    let enc = encoding.unwrap_or("").trim().to_ascii_lowercase();
    let decoded: Option<Vec<u8>> = match enc.as_str() {
        "gzip" => {
            use std::io::Read;
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(body)
                .read_to_end(&mut out)
                .ok()
                .map(|_| out)
        }
        "deflate" => {
            use std::io::Read;
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(body)
                .read_to_end(&mut out)
                .ok()
                .map(|_| out)
        }
        #[cfg(feature = "zstd-bodies")]
        "zstd" => zstd::stream::decode_all(body).ok(),
        _ => None,
    };
    match decoded {
        Some(d) => (String::from_utf8_lossy(&d).into_owned(), enc),
        None => (
            String::from_utf8_lossy(body).into_owned(),
            if enc.is_empty() {
                "identity".into()
            } else {
                enc
            },
        ),
    }
}

fn storable_headers(h: &HeaderMap) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (k, v) in h {
        let k = k.as_str();
        if HOP_HEADERS.contains(&k) {
            continue;
        }
        let val = if is_secret_header(k) {
            "<redacted>".to_string()
        } else {
            String::from_utf8_lossy(v.as_bytes()).into_owned()
        };
        out.insert(k.to_string(), val);
    }
    out
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// On-disk cassette directory for one agent, with the replay indexes (keys recomputed with the
/// current normalization, so tightening [`VOLATILE`] after a recording still yields exact hits).
pub struct Store {
    pub dir: PathBuf,
    exact: Mutex<HashMap<String, PathBuf>>,
    loose: Mutex<HashMap<String, PathBuf>>,
}

impl Store {
    pub fn open(dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let mut exact = HashMap::new();
        let mut loose = HashMap::new();
        for entry in std::fs::read_dir(&dir)? {
            let p = entry?.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.ends_with(".json")
                || name.starts_with("miss-")
                || name.starts_with("replayed-raw-")
            {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(c) = serde_json::from_str::<Cassette>(&text) else {
                continue;
            };
            let k = keys(&c.request.method, &c.request.url, &c.request.body);
            exact.entry(k.exact).or_insert_with(|| p.clone());
            loose.entry(k.loose).or_insert(p);
        }
        Ok(Self {
            dir,
            exact: Mutex::new(exact),
            loose: Mutex::new(loose),
        })
    }

    pub fn len(&self) -> usize {
        self.exact.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `(cassette, "EXACT" | "LOOSE")`
    pub fn lookup(&self, k: &Keys) -> Option<(Cassette, &'static str)> {
        let path = self
            .exact
            .lock()
            .unwrap()
            .get(&k.exact)
            .cloned()
            .map(|p| (p, "EXACT"))
            .or_else(|| {
                self.loose
                    .lock()
                    .unwrap()
                    .get(&k.loose)
                    .cloned()
                    .map(|p| (p, "LOOSE"))
            });
        let (p, how) = path?;
        let c: Cassette = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
        Some((c, how))
    }

    pub fn write(&self, c: &Cassette) -> anyhow::Result<PathBuf> {
        let p = self.dir.join(format!("{}.json", c.key));
        std::fs::write(&p, serde_json::to_string_pretty(c)?)?;
        let line = serde_json::json!({
            "at": c.recorded_at, "method": c.request.method, "url": c.request.url, "status": c.response.status,
            "bytes": c.response.bytes, "chunks": c.response.chunks.len(), "sse": c.response.sse, "model": c.model,
            "key": c.key, "loose_key": c.loose_key,
        });
        use std::io::Write;
        let mut idx = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("index.jsonl"))?;
        writeln!(idx, "{line}")?;
        self.exact.lock().unwrap().insert(c.key.clone(), p.clone());
        self.loose
            .lock()
            .unwrap()
            .entry(c.loose_key.clone())
            .or_insert_with(|| p.clone());
        Ok(p)
    }

    pub fn write_miss(&self, method: &str, url: &str, k: &Keys) -> PathBuf {
        let p = self.dir.join(format!("miss-{}.json", now_ms()));
        let doc = serde_json::json!({"method": method, "url": url, "key": k.exact, "loose_key": k.loose, "model": k.model, "normalized": k.normalized});
        let _ = std::fs::write(&p, serde_json::to_string_pretty(&doc).unwrap_or_default());
        p
    }
}

#[derive(Debug, Clone)]
pub enum Mode {
    /// Forward to this upstream base URL (scheme + host\[:port\], no trailing slash) and record.
    Record { upstream: String },
    /// Serve cassettes only; never open an upstream connection.
    Replay,
}

pub struct Config {
    pub agent: String,
    pub mode: Mode,
    pub store: Arc<Store>,
    /// A line per request, e.g. `eprintln!`-backed. Keep it cheap; it runs on the request path.
    pub log: Arc<dyn Fn(&str) + Send + Sync>,
}

type BoxBody = http_body_util::combinators::BoxBody<Bytes, std::io::Error>;

fn plain(status: StatusCode, body: String, ctype: &str) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .header("content-type", ctype)
        .body(Full::new(Bytes::from(body)).map_err(|e| match e {}).boxed())
        .unwrap()
}

fn chunk_stream(chunks: Vec<Bytes>) -> BoxBody {
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, std::io::Error>>(8);
    tokio::spawn(async move {
        for c in chunks {
            if tx.send(Ok(Frame::data(c))).await.is_err() {
                break;
            }
        }
    });
    StreamBody::new(tokio_stream_from(rx)).boxed()
}

/// A `Stream` over an mpsc receiver, so the crate needs neither `futures` nor `tokio-stream`.
struct RecvStream<T>(mpsc::Receiver<T>);
impl<T> futures_core::Stream for RecvStream<T> {
    type Item = T;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<T>> {
        self.0.poll_recv(cx)
    }
}
fn tokio_stream_from<T>(rx: mpsc::Receiver<T>) -> RecvStream<T> {
    RecvStream(rx)
}

async fn handle(cfg: Arc<Config>, req: Request<Incoming>) -> Response<BoxBody> {
    let method = req.method().to_string();
    let pathq = req
        .uri()
        .path_and_query()
        .map(|p| p.to_string())
        .unwrap_or_else(|| "/".into());
    if req
        .headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
    {
        (cfg.log)(&format!(
            "refusing websocket upgrade {pathq} (client should fall back to SSE)"
        ));
        return plain(
            StatusCode::UPGRADE_REQUIRED,
            "webmock: no websocket".into(),
            "text/plain",
        );
    }
    if pathq == "/__webmock/health" {
        let up = match &cfg.mode {
            Mode::Record { upstream } => Value::String(upstream.clone()),
            Mode::Replay => Value::Null,
        };
        let mode = match cfg.mode {
            Mode::Record { .. } => "record",
            Mode::Replay => "replay",
        };
        return plain(StatusCode::OK, serde_json::json!({"mode": mode, "agent": cfg.agent, "upstream": up, "cassettes": cfg.store.dir}).to_string(), "application/json");
    }
    let (parts, body) = req.into_parts();
    let raw = match body.collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => {
            return plain(
                StatusCode::BAD_REQUEST,
                format!("webmock: unreadable request body: {e}"),
                "text/plain",
            );
        }
    };
    let encoding = parts
        .headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let (text, enc) = decode_body(&raw, encoding.as_deref());
    let k = keys(&method, &pathq, &text);
    match &cfg.mode {
        Mode::Record { upstream } => {
            record(
                cfg.clone(),
                upstream,
                &method,
                &pathq,
                &parts.headers,
                raw,
                text,
                enc,
                k,
            )
            .await
        }
        Mode::Replay => replay(cfg.clone(), &method, &pathq, &text, k),
    }
}

#[allow(clippy::too_many_arguments)]
async fn record(
    cfg: Arc<Config>,
    upstream: &str,
    method: &str,
    pathq: &str,
    req_headers: &HeaderMap,
    raw: Bytes,
    text: String,
    enc: String,
    k: Keys,
) -> Response<BoxBody> {
    let url = format!("{upstream}{pathq}");
    let client = reqwest::Client::builder()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let mut rb = client.request(method.parse().unwrap_or(reqwest::Method::GET), &url);
    for (name, v) in req_headers {
        if HOP_HEADERS.contains(&name.as_str()) {
            continue;
        }
        rb = rb.header(name, v);
    }
    rb = rb.header("accept-encoding", "identity");
    if !raw.is_empty() {
        rb = rb.body(raw.clone());
    }
    let t0 = now_ms();
    let mut up = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            (cfg.log)(&format!("upstream error {method} {pathq}: {e}"));
            return plain(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({"webmock": "upstream error", "error": e.to_string()})
                    .to_string(),
                "application/json",
            );
        }
    };
    let status = up.status().as_u16();
    let resp_headers_stored = storable_headers(up.headers());
    let resp_headers_for_client = resp_headers_stored.clone();
    let ctype = up
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, std::io::Error>>(8);
    let req_headers_stored = storable_headers(req_headers);
    let store = cfg.store.clone();
    let log = cfg.log.clone();
    let (agent, upstream_s, method_s, pathq_s) = (
        cfg.agent.clone(),
        upstream.to_string(),
        method.to_string(),
        pathq.to_string(),
    );
    tokio::spawn(async move {
        let mut chunks: Vec<Chunk> = Vec::new();
        let mut all: Vec<u8> = Vec::new();
        let mut client_gone = false;
        loop {
            match up.chunk().await {
                Ok(Some(c)) => {
                    chunks.push(Chunk {
                        t_ms: now_ms().saturating_sub(t0),
                        b64: base64::engine::general_purpose::STANDARD.encode(&c),
                    });
                    all.extend_from_slice(&c);
                    if !client_gone && tx.send(Ok(Frame::data(c))).await.is_err() {
                        client_gone = true;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    log(&format!("upstream stream error {method_s} {pathq_s}: {e}"));
                    break;
                }
            }
        }
        let text_all = String::from_utf8_lossy(&all);
        let sse = ctype.contains("text/event-stream")
            || text_all.starts_with("event:")
            || text_all.starts_with("data:");
        let c = Cassette {
            agent,
            recorded_at: chrono_like(t0),
            key: k.exact.clone(),
            loose_key: k.loose.clone(),
            model: k.model.clone(),
            request: RecordedRequest {
                method: method_s.clone(),
                url: pathq_s.clone(),
                upstream: upstream_s,
                headers: req_headers_stored,
                body: text,
                body_encoding: enc,
                normalized: k.normalized.clone(),
            },
            response: RecordedResponse {
                status,
                headers: resp_headers_stored,
                bytes: all.len(),
                duration_ms: now_ms().saturating_sub(t0),
                sse,
                text_preview: text_all.chars().take(400).collect(),
                chunks,
            },
        };
        match store.write(&c) {
            Ok(_) => log(&format!(
                "recorded {method_s} {pathq_s} -> {status} {}B/{} chunks{} key={}",
                c.response.bytes,
                c.response.chunks.len(),
                if sse { " (sse)" } else { "" },
                &c.key[..12]
            )),
            Err(e) => log(&format!("could not write cassette: {e}")),
        }
    });
    let mut resp = Response::builder().status(status);
    for (k2, v) in up_headers_for_client(&resp_headers_for_client) {
        resp = resp.header(k2, v);
    }
    resp.body(StreamBody::new(tokio_stream_from(rx)).boxed())
        .unwrap()
}

fn up_headers_for_client(stored: &HashMap<String, String>) -> Vec<(String, String)> {
    stored
        .iter()
        .filter(|(k, _)| !HOP_HEADERS.contains(&k.as_str()) && !is_secret_header(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn replay(
    cfg: Arc<Config>,
    method: &str,
    pathq: &str,
    raw_text: &str,
    k: Keys,
) -> Response<BoxBody> {
    let Some((c, how)) = cfg.store.lookup(&k) else {
        let miss = cfg.store.write_miss(method, pathq, &k);
        (cfg.log)(&format!(
            "MISS {method} {pathq} key={} loose={} -> {}",
            &k.exact[..12],
            &k.loose[..12],
            miss.display()
        ));
        return plain(
            StatusCode::from_u16(599).unwrap(),
            serde_json::json!({"webmock": "cassette miss", "agent": cfg.agent, "method": method, "url": pathq, "key": k.exact, "loose_key": k.loose, "wrote": miss}).to_string(),
            "application/json",
        );
    };
    let raw_same = raw_text == c.request.body;
    if !raw_same {
        let _ = std::fs::write(
            cfg.store
                .dir
                .join(format!("replayed-raw-{}.json", &k.exact[..12])),
            raw_text,
        );
    }
    (cfg.log)(&format!(
        "{how} {method} {pathq} -> {} {}B/{} chunks key={} raw-identical={raw_same}",
        c.response.status,
        c.response.bytes,
        c.response.chunks.len(),
        &c.key[..12]
    ));
    let chunks: Vec<Bytes> = c
        .response
        .chunks
        .iter()
        .filter_map(|ch| {
            base64::engine::general_purpose::STANDARD
                .decode(&ch.b64)
                .ok()
        })
        .map(Bytes::from)
        .collect();
    let mut resp = Response::builder().status(c.response.status);
    for (k2, v) in up_headers_for_client(&c.response.headers) {
        resp = resp.header(k2, v);
    }
    resp.body(chunk_stream(chunks)).unwrap()
}

fn chrono_like(ms: u64) -> String {
    // RFC 3339 without pulling chrono into a dev crate.
    let secs = ms / 1000;
    let days = secs / 86400;
    let (h, m, s) = ((secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    // civil-from-days (Howard Hinnant)
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{:03}Z",
        ms % 1000
    )
}

/// Bind on loopback and serve until the returned handle is aborted. `port` 0 picks a free port.
pub async fn serve(
    cfg: Config,
    port: u16,
) -> anyhow::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    let addr = listener.local_addr()?;
    let cfg = Arc::new(cfg);
    (cfg.log)(&format!(
        "listening on http://{addr} mode={} cassettes={}",
        match cfg.mode {
            Mode::Record { .. } => "record",
            Mode::Replay => "replay",
        },
        cfg.store.dir.display()
    ));
    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let cfg = cfg.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let cfg = cfg.clone();
                    async move { Ok::<_, std::convert::Infallible>(handle(cfg, req).await) }
                });
                let _ = http1::Builder::new()
                    .keep_alive(true)
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    Ok((addr, handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_replaced() {
        assert_eq!(replace_dates("today is 2026-10-15."), "today is <DATE>.");
        assert_eq!(
            replace_dates("at 2026-10-15T19:21:09.962Z end"),
            "at <DATE> end"
        );
        assert_eq!(replace_dates("x2026-10-15"), "x2026-10-15");
    }

    #[test]
    fn volatile_fields_do_not_change_the_key() {
        let a = keys(
            "POST",
            "/v1/messages?beta=true",
            r#"{"model":"m","messages":[{"role":"user","content":"hi"}],"request_id":"r1"}"#,
        );
        let b = keys(
            "POST",
            "/v1/messages?beta=true",
            r#"{"request_id":"r2","messages":[{"role":"user","content":"hi"}],"model":"m"}"#,
        );
        assert_eq!(a.exact, b.exact);
        assert_eq!(a.loose, b.loose);
        let c = keys(
            "POST",
            "/v1/messages?beta=true",
            r#"{"model":"m","messages":[{"role":"user","content":"bye"}]}"#,
        );
        assert_ne!(a.exact, c.exact);
        assert_ne!(a.loose, c.loose);
    }

    #[test]
    fn loose_key_ignores_history_but_not_the_last_user_turn() {
        let a = keys(
            "POST",
            "/x",
            r#"{"model":"m","input":[{"role":"user","content":[{"type":"input_text","text":"first"}]},{"role":"assistant","content":"a"},{"role":"user","content":[{"type":"input_text","text":"last"}]}]}"#,
        );
        let b = keys(
            "POST",
            "/x",
            r#"{"model":"m","input":[{"role":"user","content":[{"type":"input_text","text":"last"}]}]}"#,
        );
        assert_ne!(a.exact, b.exact);
        assert_eq!(a.loose, b.loose);
    }
}
