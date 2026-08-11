//! Object-storage destinations for share and resume.
//!
//! Transport is [`object_store`], so one code path covers real AWS S3,
//! any S3-compatible endpoint (Cloudflare R2, MinIO, Ceph, Backblaze
//! B2), and a plain local directory via `file://`. A folder is a
//! first-class destination, not a testing affordance: `path target
//! ~/Dropbox/traces` is a complete setup, needing no credentials at
//! all. It is also what the tests round-trip against, so share and
//! resume are exercised end-to-end without a network.
//!
//! The module owns two separable things:
//!
//! 1. [`Destination`] / [`ObjectUri`] / [`ObjectName`] — *where* a
//!    document goes and what it's called. Pure URL parsing and naming;
//!    no credentials involved. Where a document lands is a function of
//!    the destination and the document itself, nothing else.
//! 2. [`S3Settings`] — *how to reach* an `s3://` destination: region,
//!    endpoint, addressing style, and credentials, persisted at
//!    `~/.toolpath/s3.json` by `path auth s3 login`.
//!
//! Keeping those apart is what lets `--to ~/traces` skip the whole
//! credential story, and lets one stored credential serve any number
//! of buckets.
//!
//! Credentials are handed to `object_store` as config options rather
//! than resolved here. When none are configured, the AWS credential
//! chain (env, EC2/ECS instance metadata, web identity) still applies —
//! so an EC2 box with an instance role needs no `path auth s3 login`
//! at all.

use anyhow::{Context, Result, anyhow, bail};
use object_store::{ObjectStore, ObjectStoreExt};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use url::Url;

use crate::config::config_dir;

pub(crate) const S3_CONFIG_FILE: &str = "s3.json";
pub(crate) const DEFAULT_REGION: &str = "us-east-1";

/// URL schemes routed to object storage. Deliberately narrower than
/// what `object_store` can parse: `http`/`https` belong to Pathbase in
/// every command that shares this dispatch, `gs://` / `az://` would
/// need feature flags we don't compile in, and `memory://` is a fresh
/// per-process store — anything "shared" there is gone before the
/// command exits, so accepting it would only waste someone's afternoon.
const SCHEMES: [&str; 3] = ["s3", "s3a", "file"];

// ── S3 connection settings ──────────────────────────────────────────────

/// The blob persisted at `~/.toolpath/s3.json` (0600).
///
/// Connection and credentials only — deliberately *not* a destination.
/// The bucket and prefix live in the share target
/// ([`crate::target`]), so there is exactly one answer to
/// "where does my next share go?", and so one stored credential can
/// serve `--to s3://a/x` and `--to s3://b/y` alike.
///
/// Every field is optional so a partial configuration is legal: a user
/// whose credentials come from the environment (CI, an EC2 instance
/// role) may store only `region`/`endpoint`, or nothing at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct S3Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Custom endpoint (`https://…`) for S3-compatible services such as
    /// Cloudflare R2 or MinIO. Absent means real AWS S3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_access_key: Option<String>,
    /// Temporary-credential token (STS / assumed role).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
    /// Use virtual-hosted addressing (`bucket.host/key`) instead of
    /// path style (`host/bucket/key`). Unset lets `object_store` pick:
    /// path style, which every S3-compatible endpoint accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virtual_hosted_style: Option<bool>,
}

pub(crate) fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join(S3_CONFIG_FILE))
}

pub(crate) fn load_stored(path: &std::path::Path) -> Result<Option<S3Settings>> {
    crate::config::read_private_json(path)
}

pub(crate) fn store(path: &std::path::Path, cfg: &S3Settings) -> Result<()> {
    crate::config::write_private_json(path, cfg)
}

pub(crate) fn clear(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow!("remove {}: {e}", path.display())),
    }
}

/// The stored settings with environment variables filling any gap.
///
/// Precedence is stored-then-env, not env-then-stored: the point of
/// `path auth s3 login` is that what you configured is what you get.
/// Env vars are the fallback for environments that never ran `login`
/// (CI, containers), and they use the conventional AWS names so an
/// already-configured shell just works.
///
/// Recognized: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
/// `AWS_SESSION_TOKEN`, `AWS_REGION` (then `AWS_DEFAULT_REGION`), and
/// `AWS_ENDPOINT_URL_S3` (then `AWS_ENDPOINT_URL`).
pub(crate) fn effective_settings() -> Result<S3Settings> {
    let stored = load_stored(&config_path()?)?.unwrap_or_default();
    Ok(merge_env(stored, |k| std::env::var(k).ok()))
}

/// [`effective_settings`] with the environment injected, so tests don't
/// have to mutate process-global state.
pub(crate) fn merge_env<F: Fn(&str) -> Option<String>>(mut cfg: S3Settings, env: F) -> S3Settings {
    let first = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| env(k).filter(|v| !v.trim().is_empty()))
    };
    cfg.access_key_id = cfg.access_key_id.or_else(|| first(&["AWS_ACCESS_KEY_ID"]));
    cfg.secret_access_key = cfg
        .secret_access_key
        .or_else(|| first(&["AWS_SECRET_ACCESS_KEY"]));
    cfg.session_token = cfg.session_token.or_else(|| first(&["AWS_SESSION_TOKEN"]));
    cfg.region = cfg
        .region
        .or_else(|| first(&["AWS_REGION", "AWS_DEFAULT_REGION"]));
    cfg.endpoint = cfg
        .endpoint
        .or_else(|| first(&["AWS_ENDPOINT_URL_S3", "AWS_ENDPOINT_URL"]));
    cfg
}

/// Settings as `object_store` key/value options. Unrecognized keys are
/// ignored by `parse_url_opts`, so the same list is safe to pass for a
/// `file://` URL as for `s3://`.
fn store_options(cfg: &S3Settings) -> Vec<(&'static str, String)> {
    let mut opts: Vec<(&'static str, String)> = Vec::new();
    let mut push = |k: &'static str, v: &Option<String>| {
        if let Some(v) = v.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            opts.push((k, v.to_string()));
        }
    };
    push("aws_access_key_id", &cfg.access_key_id);
    push("aws_secret_access_key", &cfg.secret_access_key);
    push("aws_session_token", &cfg.session_token);
    push("aws_endpoint", &cfg.endpoint);
    push(
        "aws_region",
        &Some(
            cfg.region
                .clone()
                .unwrap_or_else(|| DEFAULT_REGION.to_string()),
        ),
    );
    if let Some(v) = cfg.virtual_hosted_style {
        opts.push(("aws_virtual_hosted_style_request", v.to_string()));
    }
    // A plaintext endpoint is a deliberate choice (MinIO on localhost,
    // a test fixture); object_store refuses http:// unless told.
    if cfg
        .endpoint
        .as_deref()
        .is_some_and(|e| e.starts_with("http://"))
    {
        opts.push(("aws_allow_http", "true".to_string()));
    }
    opts
}

// ── Locations ───────────────────────────────────────────────────────────

/// A single object in object storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectUri {
    url: Url,
}

impl std::fmt::Display for ObjectUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&friendly(&self.url))
    }
}

/// True for anything `path resume` / `p import` should route to object
/// storage rather than to Pathbase or the local cache.
pub(crate) fn looks_like_object_uri(s: &str) -> bool {
    SCHEMES.iter().any(|p| s.starts_with(&format!("{p}://")))
}

impl ObjectUri {
    /// Parse a full object reference. A container with no key names a
    /// place, not a document, so it's rejected here — the share side
    /// goes through [`Destination`] instead.
    pub(crate) fn parse(raw: &str) -> Result<Self> {
        let url = parse_location(raw)?;
        if url.path().trim_matches('/').is_empty() {
            bail!(
                "`{raw}` names a location but no object key \
                 (expected s3://bucket/path/to/doc.json)"
            );
        }
        Ok(ObjectUri { url })
    }

    /// The cache id a download of this object lands at, e.g.
    /// `s3-my-bucket-traces_claude-abc`.
    pub(crate) fn cache_id(&self) -> String {
        let source = match self.url.scheme() {
            "s3a" => "s3",
            other => other,
        };
        let host = self.url.host_str().unwrap_or_default();
        let key = self.url.path().trim_matches('/');
        let inner = if host.is_empty() {
            key.to_string()
        } else {
            format!("{host}-{key}")
        };
        crate::cache::make_id(source, &inner)
    }

    /// Download the object as UTF-8 text.
    pub(crate) fn get(&self, cfg: &S3Settings) -> Result<String> {
        let (store, path) = open(&self.url, cfg)?;
        let bytes = block_on(async {
            let result = store.get(&path).await?;
            result.bytes().await
        })
        .map_err(|e| explain_location(e, "read", &self.to_string()))?;
        String::from_utf8(bytes.to_vec()).with_context(|| format!("{self} is not valid UTF-8"))
    }

    /// Upload `body` to the object, overwriting any existing one.
    ///
    /// Overwrite is intentional: the object name is a pure function of
    /// the document, so re-sharing a session that has grown replaces
    /// its own object rather than accumulating near-duplicates.
    pub(crate) fn put(&self, cfg: &S3Settings, body: &[u8]) -> Result<()> {
        let (store, path) = open(&self.url, cfg)?;
        let payload = object_store::PutPayload::from(body.to_vec());
        block_on(store.put(&path, payload))
            .map(|_| ())
            .map_err(|e| explain_location(e, "write", &self.to_string()))
    }
}

fn open(url: &Url, cfg: &S3Settings) -> Result<(Box<dyn ObjectStore>, object_store::path::Path)> {
    open_with(url, cfg, Vec::new())
}

fn open_with(
    url: &Url,
    cfg: &S3Settings,
    extra: Vec<(&'static str, String)>,
) -> Result<(Box<dyn ObjectStore>, object_store::path::Path)> {
    let mut opts = store_options(cfg);
    opts.extend(extra);
    object_store::parse_url_opts(url, opts).with_context(|| format!("open {}", friendly(url)))
}

/// Open a store for a preflight check rather than for real work.
///
/// Uploads keep `object_store`'s defaults — a big session over a slow
/// link is not a failure — but a check exists to give a fast answer.
/// Per-request timeouts alone don't bound it: the default ten retries
/// multiply them, so a hanging endpoint could stall a command whose
/// whole job is to record a preference. Cap the retries too.
fn open_for_check(
    url: &Url,
    cfg: &S3Settings,
) -> Result<(Box<dyn ObjectStore>, object_store::path::Path)> {
    let timeouts = vec![
        ("aws_connect_timeout", "5s".to_string()),
        ("aws_timeout", "15s".to_string()),
    ];
    if !matches!(url.scheme(), "s3" | "s3a") {
        // Local has neither retries nor a network to wait on.
        return open_with(url, cfg, timeouts);
    }

    // `parse_url_opts` has no key for retry policy, so build the S3
    // store directly. `with_url` does the same bucket/region parsing
    // `parse_url_opts` would.
    let mut builder = object_store::aws::AmazonS3Builder::new().with_url(url.as_str());
    for (key, value) in store_options(cfg).into_iter().chain(timeouts) {
        if let Ok(parsed) = key.parse() {
            builder = builder.with_config(parsed, value);
        }
    }
    let store = builder
        .with_retry(object_store::RetryConfig {
            max_retries: 2,
            retry_timeout: std::time::Duration::from_secs(20),
            ..Default::default()
        })
        .build()
        .with_context(|| format!("open {}", friendly(url)))?;
    let path = object_store::path::Path::parse(url.path())
        .with_context(|| format!("parse {}", friendly(url)))?;
    Ok((Box::new(store), path))
}

/// Explain a failed [`Destination::verify`] in terms of what the user
/// was trying to do — designate a place to share to — and how to get
/// past it if they know better than we do.
fn explain_verify(err: object_store::Error, dest: &Destination) -> anyhow::Error {
    let detail = match &err {
        e if is_unreachable(e) => {
            "couldn't reach the endpoint. Check the URL and your network.".to_string()
        }
        e if is_missing_container(e) => format!(
            "no such bucket: {}. Check the name, or the endpoint if this isn't AWS.",
            dest.base.host_str().unwrap_or_default()
        ),
        object_store::Error::Unauthenticated { .. } => {
            "S3 rejected the credentials. Run `path auth s3 login` to store working ones."
                .to_string()
        }
        object_store::Error::PermissionDenied { .. } => {
            "the credentials can reach the bucket but aren't allowed to write to it. \
             Check the bucket policy for `s3:PutObject` on this prefix."
                .to_string()
        }
        e => terse(e),
    };
    anyhow!(
        "can't write to {dest}: {detail}\n\
         Pass --no-verify to store it anyway (e.g. the bucket doesn't exist yet, \
         or you're offline)."
    )
}

/// True when the request never reached a server: DNS, refused
/// connection, timeout. Distinguished from an S3-level rejection
/// because the fix is completely different — check the URL, not the
/// policy.
fn is_unreachable(err: &object_store::Error) -> bool {
    let msg = err.to_string();
    msg.contains("error sending request")
        || msg.contains("Connection refused")
        || msg.contains("operation timed out")
        || msg.contains("dns error")
}

/// Strip `object_store`'s internals out of an error message.
///
/// Its transport errors carry a retry epilogue — "after 10 retries,
/// max_retries: 10, retry_timeout: 180s" — plus a `Generic S3 error:`
/// prefix. Neither tells a user anything actionable, and both bury the
/// part that does.
fn terse(err: &object_store::Error) -> String {
    let msg = err.to_string();
    let msg = msg.split(", after ").next().unwrap_or(&msg);
    msg.trim_start_matches("Generic S3 error: ")
        .trim_end_matches([' ', '-'])
        .to_string()
}

/// Where `path share` writes when the target is object storage: a
/// bucket-or-folder base that object keys hang off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Destination {
    base: Url,
}

impl std::fmt::Display for Destination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(friendly(&self.base).trim_end_matches('/'))
    }
}

/// One object found by [`Destination::list`].
#[derive(Debug, Clone)]
pub(crate) struct ObjectEntry {
    pub uri: ObjectUri,
    /// Filename without the `.json` extension — for legible names this
    /// is `<date>-<slug>-<cache-id>`, which is the whole point.
    pub stem: String,
    pub size: u64,
    pub modified: Option<chrono::DateTime<chrono::Utc>>,
}

impl Destination {
    /// Parse a user-supplied destination. See [`parse_location`] for
    /// how a scheme-less value is read.
    pub(crate) fn parse(raw: &str) -> Result<Self> {
        Ok(Destination {
            base: parse_location(raw)?,
        })
    }

    /// The `.json` objects sitting directly under this destination,
    /// newest first.
    ///
    /// Deliberately non-recursive: a destination is a place you share
    /// *to*, so its immediate contents are what a picker should offer.
    /// Nothing is downloaded — legible object names carry enough for a
    /// picker row, which is exactly why they're worth the length.
    pub(crate) fn list(&self, cfg: &S3Settings) -> Result<Vec<ObjectEntry>> {
        let (store, prefix) = open(&self.base, cfg)?;
        let listed = block_on(store.list_with_delimiter(Some(&prefix)))
            .map_err(|e| explain_location(e, "list", &friendly(&self.base)))?;

        let mut out: Vec<ObjectEntry> = listed
            .objects
            .into_iter()
            .filter(|m| m.location.as_ref().ends_with(".json"))
            .map(|m| {
                let name = m
                    .location
                    .filename()
                    .unwrap_or_default()
                    .trim_end_matches(".json")
                    .to_string();
                let mut url = self.base.clone();
                let base_path = url.path().trim_end_matches('/').to_string();
                url.set_path(&format!("{base_path}/{name}.json"));
                ObjectEntry {
                    uri: ObjectUri { url },
                    stem: name,
                    size: m.size,
                    modified: Some(m.last_modified),
                }
            })
            .collect();
        // Newest first: the session you want is nearly always recent.
        out.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.stem.cmp(&b.stem)));
        Ok(out)
    }

    /// Prove a share to this destination will actually work, by doing
    /// what a share does: write a small object, then remove it.
    ///
    /// This is the *configuration-time* check, and it is deliberately a
    /// real write. A listing tells you about `s3:ListBucket`, which is
    /// not the permission a share needs; a credential probe tells you
    /// the keys parse. Only a write tells you the thing the user is
    /// actually asking — "can I send my sessions here?" — and the
    /// moment they designate a destination is the cheapest possible
    /// time to answer it. Anything less is theater that defers the
    /// failure to the middle of a share.
    ///
    /// The probe object is named to be obviously ours, and removed
    /// afterwards. A credential that can write but not delete leaves it
    /// behind; that's reported, not fatal, because writing was the
    /// thing being tested.
    pub(crate) fn verify(&self, cfg: &S3Settings) -> Result<()> {
        const PROBE: &str = ".toolpath-access-check";
        const BODY: &[u8] = b"toolpath access check\n";

        let mut url = self.base.clone();
        let base_path = url.path().trim_end_matches('/').to_string();
        url.set_path(&format!("{base_path}/{PROBE}"));

        // Bound the wait: a wrong endpoint should fail in seconds, not
        // hang a command whose whole job is to record a preference.
        let (store, path) = open_for_check(&url, cfg)?;
        block_on(store.put(&path, object_store::PutPayload::from(BODY.to_vec())))
            .map_err(|e| explain_verify(e, self))?;

        if let Err(e) = block_on(store.delete(&path)) {
            eprintln!(
                "note: wrote and left behind {}/{PROBE} — the credentials can write \
                 but not delete ({e})",
                self
            );
        }
        Ok(())
    }

    /// Cheap reachability check for the moment *before* an upload, when
    /// a real write would be redundant — the upload itself is about to
    /// happen and will report its own failure.
    ///
    /// Only *conclusive* failures are errors. A credential that can
    /// write but not list is normal, and so is an empty or missing
    /// prefix. This exists to catch the one case worth catching before
    /// a derivation: a typo'd bucket or endpoint.
    pub(crate) fn probe(&self, cfg: &S3Settings) -> Result<()> {
        // Local destinations create themselves on write, and a bad path
        // fails with a clear OS error. Nothing to preflight.
        if self.is_local() {
            return Ok(());
        }
        let (store, prefix) = open_for_check(&self.base, cfg)?;
        match block_on(store.list_with_delimiter(Some(&prefix))) {
            Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            // A write-only credential can't list. Not a problem.
            Err(object_store::Error::PermissionDenied { .. }) => Ok(()),
            Err(e) if is_missing_container(&e) => Err(anyhow!(
                "no such bucket: {}. Check the name, or the endpoint if this isn't AWS.",
                self.base.host_str().unwrap_or_default()
            )),
            Err(e @ object_store::Error::Unauthenticated { .. }) => Err(anyhow!(
                "S3 rejected the stored credentials for {self}: {e}. \
                 Run `path auth s3 login` to replace them."
            )),
            // Anything else — a transient network blip, an unusual
            // policy — is not worth blocking a share over. The upload
            // itself will report it properly if it's real.
            Err(_) => Ok(()),
        }
    }

    /// The canonical form to persist: always a URL, never a bare path,
    /// so a stored default can't be re-read relative to a different cwd.
    pub(crate) fn as_url(&self) -> &str {
        self.base.as_str()
    }

    /// True for a plain local folder — the case that needs no
    /// credentials, and so no `path auth s3 login`.
    pub(crate) fn is_local(&self) -> bool {
        self.base.scheme() == "file"
    }

    /// The object a document with this name lands at.
    pub(crate) fn uri_for(&self, name: &ObjectName) -> ObjectUri {
        let mut url = self.base.clone();
        let base_path = url.path().trim_end_matches('/').to_string();
        url.set_path(&format!("{base_path}/{}.json", name.0));
        ObjectUri { url }
    }
}

// ── Naming ──────────────────────────────────────────────────────────────

/// What a shared document is called in the destination.
///
/// `<date>-<slug>-<cache-id>`, e.g.
/// `2026-08-07-add-s3-support-to-share-claude-6f2a1c9e`.
///
/// Two requirements pull in opposite directions and both are load-bearing:
///
/// - **Stable.** Every component is a pure function of the document, so
///   re-sharing a session that has grown overwrites its own object
///   instead of leaving a trail of near-duplicates.
/// - **Legible.** A destination is a folder someone will open, or a
///   bucket someone will page through. `claude-6f2a1c9e.json` tells
///   them nothing; the date sorts chronologically under a plain
///   lexicographic listing, and the slug says which session it is.
///
/// Legibility also buys the picker: `path resume <destination>` builds
/// its rows from names alone, so browsing a hundred shared sessions
/// costs one list request and zero downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectName(String);

impl std::fmt::Display for ObjectName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl ObjectName {
    /// Longest slug we'll put in a name. Long enough to recognize a
    /// session, short enough that the cache id stays visible in a
    /// terminal-width listing.
    const SLUG_MAX: usize = 48;

    pub(crate) fn new(cache_id: &str, date: Option<&str>, title: Option<&str>) -> Self {
        let mut parts: Vec<String> = Vec::new();
        if let Some(d) = date.map(slugify).filter(|d| !d.is_empty()) {
            parts.push(d);
        }
        if let Some(t) = title.map(slugify).filter(|t| !t.is_empty()) {
            parts.push(truncate_slug(&t, Self::SLUG_MAX));
        }
        parts.push(slugify(cache_id));
        ObjectName(parts.join("-"))
    }

    /// The name for a document with no usable metadata — the cache id
    /// alone, which is what the whole scheme degrades to.
    #[cfg(test)]
    pub(crate) fn bare(cache_id: &str) -> Self {
        Self::new(cache_id, None, None)
    }
}

/// Lowercase, ASCII-alphanumeric, single dashes, no leading/trailing
/// dash. Deliberately lossy — this is a filename, not a title.
fn slugify(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out
}

/// Truncate on a dash boundary so a name never ends mid-word.
fn truncate_slug(slug: &str, max: usize) -> String {
    if slug.len() <= max {
        return slug.to_string();
    }
    let cut = &slug[..max];
    match cut.rfind('-') {
        Some(i) if i > 0 => cut[..i].to_string(),
        _ => cut.to_string(),
    }
}

/// Name a document for a destination, reading the date and topic out of
/// the document itself so `share` and `p export object` agree without
/// either of them having to know where the document came from.
pub(crate) fn name_for(doc: &toolpath::v1::Graph, cache_id: &str) -> ObjectName {
    let path = doc.paths.iter().find_map(|p| match p {
        toolpath::v1::PathOrRef::Path(p) => Some(p.as_ref()),
        toolpath::v1::PathOrRef::Ref(_) => None,
    });
    let Some(path) = path else {
        return ObjectName::new(cache_id, None, None);
    };

    // Earliest step wins: a session is dated when it started, so the
    // name doesn't move as the conversation grows.
    let date = path
        .steps
        .iter()
        .map(|s| s.step.timestamp.as_str())
        .min()
        .and_then(|ts| ts.split('T').next())
        .map(str::to_string);

    ObjectName::new(cache_id, date.as_deref(), topic_of(path).as_deref())
}

/// [`name_for`] against the serialized document — the exact bytes about
/// to be uploaded, so the name always describes what actually lands.
/// Degrades to the bare cache id if the body doesn't parse, because a
/// worse name is better than a failed share.
pub(crate) fn name_for_body(body: &str, cache_id: &str) -> ObjectName {
    match toolpath::v1::Graph::from_json(body) {
        Ok(doc) => name_for(&doc, cache_id),
        Err(_) => ObjectName::new(cache_id, None, None),
    }
}

/// The first user prompt, which is what a session is *about*.
///
/// Falls back to `meta.title`, but only when it looks like a real
/// title: `derive_path` synthesizes `"<provider> session: <id>"` when
/// it has nothing better, and repeating the id in the slug would waste
/// the legible half of the name.
fn topic_of(path: &toolpath::v1::Path) -> Option<String> {
    for step in &path.steps {
        for change in path_changes(step) {
            let Some(structural) = &change.structural else {
                continue;
            };
            if structural.change_type != "conversation.append" {
                continue;
            }
            let role = structural.extra.get("role").and_then(|v| v.as_str());
            if role != Some("user") {
                continue;
            }
            if let Some(text) = structural
                .extra
                .get("text")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|t| !t.is_empty())
            {
                return Some(text.to_string());
            }
        }
    }
    path.meta
        .as_ref()
        .and_then(|m| m.title.as_deref())
        .filter(|t| !t.contains(" session: "))
        .map(str::to_string)
}

/// Steps hold their changes in a map; iteration order is arbitrary, so
/// sort by artifact key to keep naming deterministic across runs.
fn path_changes(step: &toolpath::v1::Step) -> Vec<&toolpath::v1::ArtifactChange> {
    let mut keys: Vec<&String> = step.change.keys().collect();
    keys.sort();
    keys.into_iter()
        .filter_map(|k| step.change.get(k))
        .collect()
}

/// Parse a user-supplied location into a URL.
///
/// A value carrying a scheme is taken at its word. A scheme-less value
/// is a **local filesystem path** — `~/traces`, `./out`, `/srv/traces`
/// — expanded, made absolute, and turned into a `file://` URL. Bare
/// strings mean folders rather than buckets because that is what people
/// type when designating a directory; an S3 bucket is named with an
/// explicit `s3://`, which is unambiguous and self-documenting.
///
/// A *bare relative* path (`my-bucket/traces`) is rejected. It is the
/// one shape that is genuinely ambiguous — overwhelmingly a bucket name
/// typed from memory — and silently resolving it against the current
/// directory would create `./my-bucket/traces` and report success.
/// `./my-bucket/traces` says "yes, relative, I meant it".
fn parse_location(raw: &str) -> Result<Url> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("empty location");
    }

    if !raw.contains("://") && is_ambiguously_relative(raw) {
        bail!(
            "`{raw}` is ambiguous: a scheme-less location is a local path, but this \
             one is relative to the current directory.\n  \
             s3://{raw}   — if you meant an S3 bucket\n  \
             ./{raw}      — if you really meant a folder here"
        );
    }

    if raw.contains("://") {
        let url = Url::parse(raw).with_context(|| format!("`{raw}` is not a valid URL"))?;
        if !SCHEMES.contains(&url.scheme()) {
            bail!(
                "unsupported location scheme `{}://` (expected one of: {})",
                url.scheme(),
                SCHEMES
                    .iter()
                    .map(|s| format!("{s}://"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if matches!(url.scheme(), "s3" | "s3a") && url.host_str().unwrap_or_default().is_empty() {
            bail!("`{raw}` has no bucket (expected s3://bucket/prefix)");
        }
        return Ok(url);
    }

    let expanded = expand_tilde(raw);
    let absolute = std::path::absolute(&expanded)
        .with_context(|| format!("resolve `{}` to an absolute path", expanded.display()))?;
    Url::from_directory_path(&absolute).map_err(|()| {
        anyhow!(
            "`{raw}` is neither a URL nor a usable filesystem path \
             (for an S3 bucket, write it as s3://{raw})"
        )
    })
}

/// True for a path that is relative *and* doesn't say so explicitly.
/// `./x` and `../x` are deliberate; `x` and `x/y` are the trap.
fn is_ambiguously_relative(raw: &str) -> bool {
    !(raw.starts_with('/')
        || raw.starts_with('~')
        || raw.starts_with("./")
        || raw.starts_with("../")
        || raw == "."
        || raw == ".."
        // Windows: `C:\…` and `\\server\share`.
        || raw.starts_with('\\')
        || raw.as_bytes().get(1) == Some(&b':'))
}

/// Expand a leading `~/`. Shells normally do this, but a quoted or
/// config-file value arrives literal, and `object_store` would happily
/// create a directory actually named `~`.
fn expand_tilde(raw: &str) -> PathBuf {
    let Some(rest) = raw.strip_prefix("~/") else {
        return PathBuf::from(raw);
    };
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(rest),
        None => PathBuf::from(raw),
    }
}

/// Render a location for humans: a `file://` URL shows as the plain
/// path it names, which is both shorter and directly pasteable into
/// `path resume`. Everything else shows as its URL.
fn friendly(url: &Url) -> String {
    if url.scheme() == "file"
        && let Ok(p) = url.to_file_path()
    {
        return p.to_string_lossy().into_owned();
    }
    url.as_str().to_string()
}

/// Turn an `object_store` error into something a user can act on. Its
/// `NotFound` and `Unauthenticated` variants are the two that matter:
/// the first usually means a typo'd key, the second an unconfigured or
/// stale credential.
fn explain_location(err: object_store::Error, verb: &str, location: &str) -> anyhow::Error {
    match err {
        object_store::Error::NotFound { .. } => anyhow!("{location} not found"),
        object_store::Error::Unauthenticated { .. }
        | object_store::Error::PermissionDenied { .. } => {
            anyhow!(
                "not authorized to {verb} {location}. Run `path auth s3 login` to store \
                 credentials, or check the bucket policy for the ones you have."
            )
        }
        e => anyhow!("failed to {verb} {location}: {}", terse(&e)),
    }
}

/// `object_store` folds "no such bucket" into a generic transport
/// error, so the only handle on it is the message S3 returned. Worth
/// the string match: a typo'd bucket is the single most common way a
/// share target is wrong, and "NoSuchBucket" buried in an XML dump is
/// not an answer.
fn is_missing_container(err: &object_store::Error) -> bool {
    let msg = err.to_string();
    msg.contains("NoSuchBucket") || msg.contains("NoSuchHost") || msg.contains("dns error")
}

/// `object_store` is async; the rest of path-cli is sync. Same tunnel
/// the Pathbase client uses, so both share one runtime.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    crate::cmd_pathbase::block_on(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_s3_uris() {
        let u = ObjectUri::parse("s3://my-bucket/traces/claude-abc.json").unwrap();
        assert_eq!(u.to_string(), "s3://my-bucket/traces/claude-abc.json");
    }

    #[test]
    fn container_without_a_key_is_not_an_object() {
        let err = ObjectUri::parse("s3://my-bucket").unwrap_err().to_string();
        assert!(err.contains("no object key"), "{err}");
    }

    #[test]
    fn looks_like_object_uri_only_matches_known_schemes() {
        assert!(looks_like_object_uri("s3://b/k"));
        assert!(looks_like_object_uri("s3a://b/k"));
        assert!(looks_like_object_uri("file:///tmp/k.json"));
        // https belongs to Pathbase; a bare id belongs to the cache.
        assert!(!looks_like_object_uri("https://pathbase.dev/a/b/c"));
        assert!(!looks_like_object_uri("claude-abc"));
    }

    #[test]
    fn unsupported_scheme_lists_the_supported_ones() {
        let err = ObjectUri::parse("gs://bucket/key.json")
            .unwrap_err()
            .to_string();
        assert!(err.contains("s3://"), "{err}");
    }

    // ── Destinations ─────────────────────────────────────────────────

    #[test]
    fn s3_destination_keys_on_the_cache_id() {
        let d = Destination::parse("s3://bkt/pre/fix").unwrap();
        assert!(!d.is_local());
        assert_eq!(
            d.uri_for(&ObjectName::bare("claude-abc")).to_string(),
            "s3://bkt/pre/fix/claude-abc.json"
        );
    }

    #[test]
    fn s3_destination_without_a_prefix_writes_at_the_bucket_root() {
        let d = Destination::parse("s3://bkt").unwrap();
        assert_eq!(
            d.uri_for(&ObjectName::bare("claude-abc")).to_string(),
            "s3://bkt/claude-abc.json"
        );
    }

    #[test]
    fn a_bare_path_is_a_local_folder_not_a_bucket() {
        let d = Destination::parse("/srv/traces").unwrap();
        assert!(d.is_local());
        assert_eq!(d.to_string(), "/srv/traces");
        assert_eq!(d.as_url(), "file:///srv/traces/");
        assert_eq!(
            d.uri_for(&ObjectName::bare("claude-abc")).to_string(),
            "/srv/traces/claude-abc.json"
        );
    }

    #[test]
    fn an_explicitly_relative_path_is_made_absolute_so_a_stored_default_is_stable() {
        let d = Destination::parse("./out").unwrap();
        let expected = std::path::absolute("./out").unwrap();
        assert_eq!(d.to_string(), expected.to_string_lossy());
    }

    #[test]
    fn a_bare_relative_path_is_rejected_as_ambiguous() {
        // The trap this guards: someone types a bucket name from memory
        // and gets ./my-bucket/traces created under their cwd, with the
        // share reporting success.
        let err = Destination::parse("my-bucket/traces")
            .unwrap_err()
            .to_string();
        assert!(err.contains("s3://my-bucket/traces"), "{err}");
        assert!(err.contains("./my-bucket/traces"), "{err}");

        // A single bare word is the same mistake.
        assert!(Destination::parse("my-bucket").is_err());
        // Saying "relative, I meant it" is accepted.
        assert!(Destination::parse("./my-bucket/traces").is_ok());
        assert!(Destination::parse("../sibling").is_ok());
    }

    #[test]
    fn memory_urls_are_rejected() {
        // A fresh store per process: anything "shared" there is gone
        // before the command exits.
        let err = Destination::parse("memory:///x").unwrap_err().to_string();
        assert!(err.contains("unsupported location scheme"), "{err}");
    }

    #[test]
    fn a_tilde_path_expands_against_home() {
        let home = std::env::var("HOME").unwrap();
        let d = Destination::parse("~/traces").unwrap();
        assert_eq!(d.to_string(), format!("{home}/traces"));
    }

    #[test]
    fn a_file_url_and_the_equivalent_bare_path_agree() {
        let from_path = Destination::parse("/srv/traces").unwrap();
        let from_url = Destination::parse("file:///srv/traces").unwrap();
        assert_eq!(
            from_path.uri_for(&ObjectName::bare("x")).to_string(),
            from_url.uri_for(&ObjectName::bare("x")).to_string()
        );
    }

    #[test]
    fn local_destinations_display_as_plain_paths() {
        // The printed form is what a user pastes into `path resume`,
        // and `path resume /abs/path.json` already works.
        let uri = Destination::parse("/srv/traces")
            .unwrap()
            .uri_for(&ObjectName::bare("claude-abc"));
        assert_eq!(uri.to_string(), "/srv/traces/claude-abc.json");
    }

    #[test]
    fn cache_id_flattens_the_key() {
        let uri = ObjectUri::parse("s3://bkt/traces/claude-abc.json").unwrap();
        assert_eq!(uri.cache_id(), "s3-bkt-traces_claude-abc");
        // s3a is the same store under a different scheme spelling, so
        // it must not fork the cache.
        let alias = ObjectUri::parse("s3a://bkt/traces/claude-abc.json").unwrap();
        assert_eq!(alias.cache_id(), uri.cache_id());
    }

    // ── S3 settings ──────────────────────────────────────────────────

    #[test]
    fn env_fills_only_the_gaps() {
        let stored = S3Settings {
            region: Some("eu-west-1".to_string()),
            ..Default::default()
        };
        let merged = merge_env(stored, |k| match k {
            "AWS_ACCESS_KEY_ID" => Some("AK".to_string()),
            "AWS_SECRET_ACCESS_KEY" => Some("SK".to_string()),
            "AWS_REGION" => Some("us-west-2".to_string()),
            _ => None,
        });
        assert_eq!(merged.access_key_id.as_deref(), Some("AK"));
        // Stored wins over env.
        assert_eq!(merged.region.as_deref(), Some("eu-west-1"));
    }

    #[test]
    fn blank_env_values_are_ignored() {
        let merged = merge_env(S3Settings::default(), |k| match k {
            "AWS_ACCESS_KEY_ID" => Some("   ".to_string()),
            _ => None,
        });
        assert!(merged.access_key_id.is_none());
    }

    #[test]
    fn store_options_carry_credentials_and_endpoint() {
        let opts = store_options(&S3Settings {
            access_key_id: Some("AK".to_string()),
            secret_access_key: Some("SK".to_string()),
            endpoint: Some("http://127.0.0.1:9000".to_string()),
            ..Default::default()
        });
        let get = |k: &str| {
            opts.iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("aws_access_key_id"), Some("AK"));
        assert_eq!(get("aws_secret_access_key"), Some("SK"));
        assert_eq!(get("aws_endpoint"), Some("http://127.0.0.1:9000"));
        // Plaintext endpoints have to be opted into explicitly.
        assert_eq!(get("aws_allow_http"), Some("true"));
        assert_eq!(get("aws_region"), Some(DEFAULT_REGION));
    }

    #[test]
    fn https_endpoint_does_not_allow_http() {
        let opts = store_options(&S3Settings {
            endpoint: Some("https://minio.example".to_string()),
            ..Default::default()
        });
        assert!(!opts.iter().any(|(k, _)| *k == "aws_allow_http"));
    }

    #[test]
    fn stored_settings_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s3.json");
        let cfg = S3Settings {
            region: Some("us-east-2".to_string()),
            access_key_id: Some("AK".to_string()),
            secret_access_key: Some("SK".to_string()),
            ..Default::default()
        };
        store(&path, &cfg).unwrap();
        assert_eq!(load_stored(&path).unwrap().unwrap(), cfg);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        clear(&path).unwrap();
        assert!(load_stored(&path).unwrap().is_none());
        // Clearing settings that aren't there is not an error.
        clear(&path).unwrap();
    }

    // ── Round trips against a local folder ───────────────────────────

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let dest = Destination::parse(&dir.path().to_string_lossy()).unwrap();
        let cfg = S3Settings::default();
        let uri = dest.uri_for(&ObjectName::bare("claude-abc"));
        let body = br#"{"graph":{"id":"g"},"paths":[]}"#;

        uri.put(&cfg, body).unwrap();
        assert_eq!(uri.get(&cfg).unwrap(), String::from_utf8_lossy(body));
        assert!(dir.path().join("claude-abc.json").is_file());
    }

    #[test]
    fn put_overwrites_an_existing_object() {
        let dir = tempfile::tempdir().unwrap();
        let dest = Destination::parse(&dir.path().to_string_lossy()).unwrap();
        let cfg = S3Settings::default();
        let uri = dest.uri_for(&ObjectName::bare("claude-abc"));

        uri.put(&cfg, b"{\"v\":1}").unwrap();
        uri.put(&cfg, b"{\"v\":2}").unwrap();
        assert_eq!(uri.get(&cfg).unwrap(), "{\"v\":2}");
    }

    #[test]
    fn nested_prefixes_are_created_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let dest = Destination::parse(&format!("{}/a/b/c", dir.path().display())).unwrap();
        dest.uri_for(&ObjectName::bare("claude-abc"))
            .put(&S3Settings::default(), b"{}")
            .unwrap();
        assert!(dir.path().join("a/b/c/claude-abc.json").is_file());
    }

    #[test]
    fn missing_object_says_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let uri = Destination::parse(&dir.path().to_string_lossy())
            .unwrap()
            .uri_for(&ObjectName::bare("nope"));
        let err = uri.get(&S3Settings::default()).unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
        assert!(err.contains("nope.json"), "{err}");
    }

    // ── Naming ───────────────────────────────────────────────────────

    /// A one-step agent document whose first user turn is `prompt`.
    fn doc_with(prompt: &str, timestamp: &str) -> toolpath::v1::Graph {
        let body = serde_json::json!({
            "graph": { "id": "g1" },
            "paths": [{
                "path": { "id": "p1", "head": "s1" },
                "steps": [{
                    "step": {
                        "id": "s1", "parents": [],
                        "actor": "agent:claude-code",
                        "timestamp": timestamp
                    },
                    "change": { "claude-code://s": { "structural": {
                        "type": "conversation.append",
                        "role": "user",
                        "text": prompt
                    }}}
                }]
            }]
        });
        toolpath::v1::Graph::from_json(&body.to_string()).unwrap()
    }

    #[test]
    fn a_name_leads_with_the_date_and_topic() {
        let doc = doc_with("Add S3 support to share", "2026-08-07T09:15:00Z");
        assert_eq!(
            name_for(&doc, "claude-abc123").to_string(),
            "2026-08-07-add-s3-support-to-share-claude-abc123"
        );
    }

    #[test]
    fn a_name_is_stable_as_the_session_grows() {
        // The date comes from the *earliest* step, so appending turns
        // can't move the object and leave a duplicate behind.
        let short = doc_with("Fix the parser", "2026-08-07T09:15:00Z");
        let name = name_for(&short, "claude-abc");

        let mut grown = short.clone();
        if let toolpath::v1::PathOrRef::Path(p) = &mut grown.paths[0] {
            let mut later = p.steps[0].clone();
            later.step.id = "s2".to_string();
            later.step.timestamp = "2026-08-09T18:00:00Z".to_string();
            p.steps.push(later);
        }
        assert_eq!(name_for(&grown, "claude-abc"), name);
    }

    #[test]
    fn a_long_prompt_is_truncated_on_a_word_boundary() {
        let doc = doc_with(
            "Add support to share and resume to and from S3 and a way to configure credentials",
            "2026-08-07T00:00:00Z",
        );
        let name = name_for(&doc, "claude-abc").to_string();
        assert!(
            name.starts_with("2026-08-07-add-support-to-share"),
            "{name}"
        );
        assert!(name.ends_with("-claude-abc"), "{name}");
        assert!(!name.contains("--"), "no empty slug segments: {name}");
    }

    #[test]
    fn a_prompt_of_pure_punctuation_degrades_to_date_and_id() {
        let doc = doc_with("!!! ???", "2026-08-07T00:00:00Z");
        assert_eq!(
            name_for(&doc, "claude-abc").to_string(),
            "2026-08-07-claude-abc"
        );
    }

    #[test]
    fn a_synthesized_title_is_not_worth_slugging() {
        // `derive_path` writes "claude-code session: abc" when it has
        // nothing better; repeating the id would waste the legible half
        // of the name.
        let body = serde_json::json!({
            "graph": { "id": "g1" },
            "paths": [{
                "path": { "id": "p1", "head": "s1" },
                "meta": { "title": "claude-code session: abc123" },
                "steps": [{
                    "step": { "id": "s1", "parents": [], "actor": "agent:claude-code",
                              "timestamp": "2026-08-07T00:00:00Z" },
                    "change": { "f": { "structural": { "type": "file.edit" } } }
                }]
            }]
        });
        let doc = toolpath::v1::Graph::from_json(&body.to_string()).unwrap();
        assert_eq!(
            name_for(&doc, "claude-abc").to_string(),
            "2026-08-07-claude-abc"
        );
    }

    #[test]
    fn an_unparseable_body_still_gets_a_name() {
        // A worse name beats a failed share.
        assert_eq!(
            name_for_body("not json", "claude-abc").to_string(),
            "claude-abc"
        );
    }

    // ── Listing ──────────────────────────────────────────────────────

    #[test]
    fn listing_returns_shared_documents_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let dest = Destination::parse(&dir.path().to_string_lossy()).unwrap();
        let cfg = S3Settings::default();

        for name in ["2026-08-01-older-claude-a", "2026-08-09-newer-claude-b"] {
            dest.uri_for(&ObjectName::bare(name))
                .put(&cfg, b"{}")
                .unwrap();
            // Distinct mtimes; the local backend stamps on write.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Noise that isn't a shared document.
        std::fs::write(dir.path().join("README.txt"), "hi").unwrap();

        let entries = dest.list(&cfg).unwrap();
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert_eq!(entries[0].stem, "2026-08-09-newer-claude-b");
        assert_eq!(entries[1].stem, "2026-08-01-older-claude-a");
        assert!(entries[0].uri.to_string().ends_with(".json"));
        assert!(entries[0].size > 0);
    }

    #[test]
    fn listing_does_not_recurse_into_sub_prefixes() {
        // A destination is a place you share *to*; its immediate
        // contents are what a picker should offer.
        let dir = tempfile::tempdir().unwrap();
        let dest = Destination::parse(&dir.path().to_string_lossy()).unwrap();
        let cfg = S3Settings::default();
        dest.uri_for(&ObjectName::bare("here"))
            .put(&cfg, b"{}")
            .unwrap();

        let nested = Destination::parse(&format!("{}/deeper", dir.path().display())).unwrap();
        nested
            .uri_for(&ObjectName::bare("there"))
            .put(&cfg, b"{}")
            .unwrap();

        let entries = dest.list(&cfg).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].stem, "here");
    }

    #[test]
    fn listing_an_empty_destination_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let dest = Destination::parse(&dir.path().to_string_lossy()).unwrap();
        assert!(dest.list(&S3Settings::default()).unwrap().is_empty());
    }

    #[test]
    fn probing_a_local_destination_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        // Not created yet: a local destination makes itself on write,
        // so there's nothing to preflight.
        let dest = Destination::parse(&format!("{}/not/yet", dir.path().display())).unwrap();
        dest.probe(&S3Settings::default()).unwrap();
    }
}
