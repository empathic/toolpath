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
/// A destination is named per call (`--to s3://bucket/prefix`), so one
/// stored credential serves any number of buckets, and a folder
/// destination needs no credentials at all.
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
    /// AWS profile to resolve credentials from, when you don't want the
    /// `AWS_PROFILE` / `[default]` answer. Storing a profile name is
    /// very different from storing a key: it's a pointer to credentials
    /// the AWS tooling already manages, so it can't go stale and it
    /// costs nothing at rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Set by [`merge_env`] when the access key came from
    /// `AWS_ACCESS_KEY_ID` rather than the stored file, so the resolver
    /// can report the source honestly. Never persisted.
    #[serde(skip)]
    pub credentials_from_env: bool,
}

/// The credential resolution this settings blob implies.
///
/// `profile` is threaded through so `--profile` on a command reaches
/// the resolver; everything else comes from the ambient AWS setup.
impl S3Settings {
    /// Resolve against the real environment, propagating the error. The
    /// resolver already answers "nothing configured" with the instance
    /// chain, so an error here is an explicit failure (a named profile
    /// that doesn't exist, an expired SSO session with nobody to ask) —
    /// callers that want to *report* a failure, rather than silently
    /// fall through to the instance chain, need the reason.
    pub(crate) fn resolve_real(&self) -> Result<crate::aws_creds::Resolved> {
        self.resolve_with(&crate::aws_creds::Env {
            home: std::env::var_os("HOME").map(PathBuf::from),
            var: &|k| std::env::var(k).ok(),
            aws_cli: &crate::aws_creds::run_aws_cli,
            sso_login: &crate::aws_creds::run_sso_login,
            confirm: &crate::aws_creds::confirm_on_tty,
        })
    }

    /// [`resolve_real`](Self::resolve_real) against an injected
    /// environment, so tests don't have to mutate process-global state.
    pub(crate) fn resolve_with(
        &self,
        env: &crate::aws_creds::Env<'_>,
    ) -> Result<crate::aws_creds::Resolved> {
        let stored = match (&self.access_key_id, &self.secret_access_key) {
            (Some(k), Some(s)) => Some(crate::aws_creds::Credentials {
                access_key_id: k.clone(),
                secret_access_key: s.clone(),
                session_token: self.session_token.clone(),
            }),
            _ => None,
        };
        let mut resolved = crate::aws_creds::resolve(stored, self.profile.as_deref(), env)?;
        if self.credentials_from_env && resolved.source == crate::aws_creds::Source::Stored {
            resolved.source = crate::aws_creds::Source::Environment;
        }
        Ok(resolved)
    }
}

pub(crate) fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join(crate::config::S3_SETTINGS_FILE_NAME))
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
    if cfg.access_key_id.is_none()
        && let (Some(key), Some(secret)) = (
            first(&["AWS_ACCESS_KEY_ID"]),
            first(&["AWS_SECRET_ACCESS_KEY"]),
        )
    {
        cfg.access_key_id = Some(key);
        cfg.secret_access_key = Some(secret);
        cfg.credentials_from_env = true;
    }
    cfg.session_token = cfg.session_token.or_else(|| first(&["AWS_SESSION_TOKEN"]));
    cfg.region = cfg
        .region
        .or_else(|| first(&["AWS_REGION", "AWS_DEFAULT_REGION"]));
    cfg.endpoint = cfg
        .endpoint
        .or_else(|| first(&["AWS_ENDPOINT_URL_S3", "AWS_ENDPOINT_URL"]));
    cfg
}

/// Settings as `object_store` key/value options, plus which credential
/// source won.
///
/// Only an `s3`/`s3a` URL resolves credentials at all. A folder needs
/// none, so for `file` this returns nothing and never touches `~/.aws`
/// or spawns the AWS CLI — which also means a folder export can never
/// trip an SSO login prompt.
///
/// A resolution *error* propagates. The resolver already answers
/// "nothing configured" with the instance chain, so an error here is
/// an explicit failure (a named profile that doesn't exist, an expired
/// SSO session with nobody to ask), and silently falling through to
/// instance metadata would write under whatever principal the machine
/// happens to have.
#[allow(clippy::type_complexity)]
fn store_options(
    cfg: &S3Settings,
    scheme: &str,
) -> Result<(
    Vec<(&'static str, String)>,
    Option<crate::aws_creds::Source>,
)> {
    fn push(opts: &mut Vec<(&'static str, String)>, k: &'static str, v: &Option<String>) {
        if let Some(v) = v.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            opts.push((k, v.to_string()));
        }
    }

    if !matches!(scheme, "s3" | "s3a") {
        return Ok((Vec::new(), None));
    }

    let mut opts: Vec<(&'static str, String)> = Vec::new();
    let resolved = cfg.resolve_real()?;
    if let Some(c) = &resolved.credentials {
        opts.push(("aws_access_key_id", c.access_key_id.clone()));
        opts.push(("aws_secret_access_key", c.secret_access_key.clone()));
        if let Some(t) = &c.session_token {
            opts.push(("aws_session_token", t.clone()));
        }
    }

    push(&mut opts, "aws_endpoint", &cfg.endpoint);
    let region = cfg
        .region
        .clone()
        .or_else(|| resolved.region.clone())
        .unwrap_or_else(|| DEFAULT_REGION.to_string());
    opts.push(("aws_region", region));

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
    Ok((opts, Some(resolved.source)))
}

// ── Locations ───────────────────────────────────────────────────────────

/// A single object in object storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectUri {
    url: Url,
}

/// What [`ObjectUri::put`] actually did: whether the write replaced an
/// object that was already there, and how many ancestor directories (for
/// a folder destination) it had to create to get there.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PutOutcome {
    pub replaced: bool,
    pub created_dirs: usize,
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

    /// The cache ID a download of this object lands at: `object-<id>`,
    /// where the ID is read from the object name (see [`ObjectName::id_of`]).
    /// A function of the URI alone, so a cache hit costs no request; a
    /// function of the *name* rather than the whole URI, so the same
    /// document fetched from two prefixes is one cache entry and a
    /// re-export of it names itself the same way.
    pub(crate) fn cache_id(&self) -> String {
        let stem = self
            .url
            .path()
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_end_matches(".json");
        let id = slugify(ObjectName::id_of(stem));
        let id = if id.len() > 100 {
            truncate_slug(&id, 100)
        } else {
            id
        };
        crate::cache::make_id("object", &id)
    }

    /// The URI's exact wire form, always carrying an explicit scheme.
    /// Unlike the friendly `Display` form (a bare path for `file://`),
    /// this round-trips through [`ObjectUri::parse`] without falling
    /// into its scheme-less/directory branch, which unconditionally
    /// appends a trailing slash (`Url::from_directory_path`) — the
    /// right behavior for a destination, but wrong for a single
    /// object. Use this, not `to_string()`, whenever a URI needs to be
    /// handed to something that re-parses it.
    pub(crate) fn as_str(&self) -> &str {
        self.url.as_str()
    }

    /// Download the object as UTF-8 text.
    pub(crate) fn get(&self, cfg: &S3Settings) -> Result<String> {
        let opened = open(&self.url, cfg)?;
        let bytes = block_on(async {
            let result = opened.store.get(&opened.path).await?;
            result.bytes().await
        })
        .map_err(|e| explain_location(e, "read", &self.to_string(), opened.source.as_ref()))?;
        String::from_utf8(bytes.to_vec()).with_context(|| format!("{self} is not valid UTF-8"))
    }

    /// Upload `body` to the object, overwriting any existing one.
    ///
    /// Overwrite is intentional: the object name is a pure function of
    /// the document, so re-sharing a session that has grown replaces
    /// its own object rather than accumulating near-duplicates.
    pub(crate) fn put(&self, cfg: &S3Settings, body: &[u8]) -> Result<PutOutcome> {
        // For a folder, count the directories that don't exist yet so the
        // caller can say when this write created the destination.
        let created_dirs = if self.url.scheme() == "file" {
            self.url
                .to_file_path()
                .ok()
                .map(|target| {
                    target
                        .ancestors()
                        .skip(1)
                        .take_while(|d| !d.exists())
                        .count()
                })
                .unwrap_or(0)
        } else {
            0
        };

        let opened = open(&self.url, cfg)?;

        // Whether this write replaces an existing object, decided before
        // the put so the answer reflects the state the caller is about to
        // change. Any head error other than `NotFound` (a transient read
        // failure, a backend that doesn't support head) is treated as
        // "new": it's informational only, and guessing wrong here must
        // never block the upload itself.
        let replaced = block_on(opened.store.head(&opened.path)).is_ok();

        let payload = object_store::PutPayload::from(body.to_vec());
        block_on(opened.store.put(&opened.path, payload))
            .map(|_| PutOutcome {
                replaced,
                created_dirs,
            })
            .map_err(|e| explain_location(e, "write", &self.to_string(), opened.source.as_ref()))
    }
}

/// An open store plus the path inside it, and which credential source
/// was used (`None` for a folder).
struct Opened {
    store: Box<dyn ObjectStore>,
    path: object_store::path::Path,
    source: Option<crate::aws_creds::Source>,
}

fn open(url: &Url, cfg: &S3Settings) -> Result<Opened> {
    let (opts, source) = store_options(cfg, url.scheme())?;
    let (store, path) = object_store::parse_url_opts(url, opts)
        .with_context(|| format!("open {}", friendly(url)))?;
    Ok(Opened {
        store,
        path,
        source,
    })
}

/// Strip `object_store`'s internals out of an error message and keep
/// the cause.
///
/// Its transport errors carry a retry epilogue — "after 10 retries,
/// max_retries: 10, retry_timeout: 180s" — plus a `Generic S3 error:`
/// or `Generic LocalFileSystem error:` prefix. Neither tells a user
/// anything actionable. The *innermost* source ("connection refused",
/// "File name too long") is the actionable part and lives at the
/// bottom of the chain, so it is appended when the head doesn't
/// already say it.
fn terse(err: &object_store::Error) -> String {
    let top = err.to_string();
    let head = top
        .split(", after ")
        .next()
        .unwrap_or(&top)
        .trim_start_matches("Generic S3 error: ")
        .trim_start_matches("Generic LocalFileSystem error: ")
        .trim_end_matches([' ', '-'])
        .to_string();

    let mut cause: Option<String> = None;
    let mut cur: &dyn std::error::Error = err;
    while let Some(next) = cur.source() {
        cause = Some(next.to_string());
        cur = next;
    }
    match cause {
        Some(c) if !c.is_empty() && !head.contains(&c) => format!("{head}: {c}"),
        _ => head,
    }
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

    /// Error clearly for a `file://` destination whose directory does not
    /// exist, rather than silently listing nothing: the local backend
    /// treats an absent directory the same as an empty one. A no-op for
    /// `s3`/`s3a`, where "doesn't exist yet" and "empty" are genuinely
    /// indistinguishable (and both fine).
    pub(crate) fn ensure_local_dir_exists(&self) -> Result<()> {
        if self.base.scheme() == "file"
            && let Ok(path) = self.base.to_file_path()
            && !path.exists()
        {
            bail!("{self} does not exist");
        }
        Ok(())
    }

    /// The `.json` objects sitting directly under this destination,
    /// newest first.
    ///
    /// Deliberately non-recursive: a destination is a place you share
    /// *to*, so its immediate contents are what a picker should offer.
    /// Nothing is downloaded — legible object names carry enough for a
    /// picker row, which is exactly why they're worth the length.
    pub(crate) fn list(&self, cfg: &S3Settings) -> Result<Vec<ObjectEntry>> {
        let opened = open(&self.base, cfg)?;
        let listed =
            block_on(opened.store.list_with_delimiter(Some(&opened.path))).map_err(|e| {
                explain_location(e, "list", &friendly(&self.base), opened.source.as_ref())
            })?;

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

    pub(crate) fn uri_for(&self, name: &ObjectName) -> ObjectUri {
        let mut url = self.base.clone();
        let base_path = url.path().trim_end_matches('/').to_string();
        url.set_path(&format!("{base_path}/{}.json", name.0));
        ObjectUri { url }
    }

    pub(crate) fn scheme(&self) -> &str {
        self.base.scheme()
    }
}

// ── Naming ──────────────────────────────────────────────────────────────

/// What a shared document is called in the destination.
///
/// `<date>-<topic>--<id>`, e.g.
/// `2026-08-07-add-s3-support-to-share--claude-code-de09d54b-b91f-4be7-a757-3ff3d004fb35`.
///
/// Two requirements pull in opposite directions and both are load-bearing:
///
/// - **Stable and unique.** Every component is a pure function of the
///   document, never of the input filename, so re-sharing a session that
///   has grown overwrites its own object instead of leaving a trail of
///   near-duplicates. The ID half is the session's own conversation
///   artifact key (see [`session_key`]), so two sessions collide only if
///   the harness issued one identifier twice. A document with no
///   conversation artifact falls back to `graph.id`, which is unique
///   within a document but says nothing across a shared store: two git
///   documents from different repositories on the same branch do collide,
///   and `--no-overwrite` is the guard for that case.
/// - **Legible.** A destination is a folder someone will open, or a
///   bucket someone will page through. The bare ID tells them nothing;
///   the date sorts chronologically under a plain lexicographic
///   listing, and the topic says which session it is.
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

/// The three pieces of an object name, recovered from its stem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameParts {
    pub date: Option<String>,
    pub topic: Option<String>,
    pub id: String,
}

impl ObjectName {
    /// Longest slug we'll put in a name. Long enough to recognize a
    /// session, short enough that the ID stays visible in a
    /// terminal-width listing.
    const SLUG_MAX: usize = 48;
    /// Longest ID we'll put in a name verbatim. Derived IDs are ~40
    /// chars; anything longer is a hand-written document, and a name
    /// must stay under filesystem limits however long that ID is.
    const ID_MAX: usize = 64;
    /// Reserved: the slugger collapses dash runs, so neither the date
    /// nor the topic can contain it, and automation splits on the last
    /// occurrence to get the ID.
    pub(crate) const ID_SEPARATOR: &'static str = "--";

    pub(crate) fn new(id: &str, date: Option<&str>, title: Option<&str>) -> Self {
        let mut prefix: Vec<String> = Vec::new();
        if let Some(d) = date.map(slugify).filter(|d| !d.is_empty()) {
            prefix.push(d);
        }
        if let Some(t) = title.map(slugify).filter(|t| !t.is_empty()) {
            prefix.push(truncate_slug(&t, Self::SLUG_MAX));
        }
        let id = bounded_id(id);
        if prefix.is_empty() {
            ObjectName(id)
        } else {
            ObjectName(format!("{}{}{id}", prefix.join("-"), Self::ID_SEPARATOR))
        }
    }

    /// The name for a document with no usable metadata — the ID
    /// alone, which is what the whole scheme degrades to.
    #[cfg(test)]
    pub(crate) fn bare(id: &str) -> Self {
        Self::new(id, None, None)
    }

    /// The ID half of a name stem: everything after the last `--`. A
    /// stem with no separator (a name from before the separator
    /// existed, or a bare ID) is taken whole.
    pub(crate) fn id_of(stem: &str) -> &str {
        stem.rsplit_once(Self::ID_SEPARATOR)
            .map(|(_, id)| id)
            .unwrap_or(stem)
    }

    /// Split a stem into date, topic, and ID. The date is recognized
    /// only as a leading `YYYY-MM-DD`; everything else before the
    /// separator is the topic.
    pub(crate) fn parse(stem: &str) -> NameParts {
        let (prefix, id) = match stem.rsplit_once(Self::ID_SEPARATOR) {
            Some((p, id)) => (Some(p), id),
            None => (None, stem),
        };
        let mut date = None;
        let mut topic = None;
        if let Some(prefix) = prefix {
            let looks_like_date = prefix.len() >= 10
                && prefix.as_bytes()[..10].iter().enumerate().all(|(i, b)| {
                    if i == 4 || i == 7 {
                        *b == b'-'
                    } else {
                        b.is_ascii_digit()
                    }
                })
                && (prefix.len() == 10 || prefix.as_bytes()[10] == b'-');
            if looks_like_date {
                date = Some(prefix[..10].to_string());
                let rest = prefix[10..].trim_start_matches('-');
                if !rest.is_empty() {
                    topic = Some(rest.to_string());
                }
            } else if !prefix.is_empty() {
                topic = Some(prefix.to_string());
            }
        }
        NameParts {
            date,
            topic,
            id: id.to_string(),
        }
    }
}

/// Slug of an ID, bounded: past `ID_MAX` the slug is cut on a dash
/// boundary at 48 and suffixed with 8 hex characters of the raw ID's
/// SHA-256, so two long IDs that share a prefix still get distinct names.
fn bounded_id(raw: &str) -> String {
    use sha2::Digest;
    let slug = slugify(raw);
    if slug.len() <= ObjectName::ID_MAX {
        return slug;
    }
    let digest = hex::encode(sha2::Sha256::digest(raw.as_bytes()));
    format!("{}-{}", truncate_slug(&slug, 48), &digest[..8])
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

/// Name a document for a destination: date and topic from the document
/// itself, identity from its conversation artifact key (see
/// [`session_key`]) or, for a document that has none, from `graph.id`.
/// Nothing about the input path is consulted, so `share` and
/// `p export object` agree, and two different documents that happen to
/// share a filename land on two keys.
pub(crate) fn name_for(doc: &toolpath::v1::Graph) -> ObjectName {
    let path = doc.paths.iter().find_map(|p| match p {
        toolpath::v1::PathOrRef::Path(p) => Some(p.as_ref()),
        toolpath::v1::PathOrRef::Ref(_) => None,
    });
    let Some(path) = path else {
        return ObjectName::new(&doc.graph.id, None, None);
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

    let id = session_key(path).unwrap_or(doc.graph.id.as_str());
    ObjectName::new(id, date.as_deref(), topic_of(path).as_deref())
}

/// The session a path describes, as its conversation artifact key.
///
/// The agent-coding-session kind specifies that key as
/// `<source>://<conversation-id>` on the `conversation.append` entry, so
/// it already carries the provider and the harness's own session ID.
/// That pair is the identity a store shared between machines needs, and
/// it is unique because the harness that issued it says so — not because
/// we truncated an ID and hoped. Documents with no conversation artifact
/// (git-derived, hand-written) have no session identity, so callers fall
/// back to `graph.id`.
///
/// Keys are visited in sorted order so a path whose steps carry several
/// conversation artifacts still names the same one every run.
fn session_key(path: &toolpath::v1::Path) -> Option<&str> {
    for step in &path.steps {
        let mut keys: Vec<&String> = step.change.keys().collect();
        keys.sort();
        for key in keys {
            let is_conversation = step
                .change
                .get(key)
                .and_then(|c| c.structural.as_ref())
                .is_some_and(|s| s.change_type == "conversation.append");
            if is_conversation && key.contains("://") {
                return Some(key.as_str());
            }
        }
    }
    None
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
/// stale credential. A request that ended up at instance metadata
/// because nothing local resolved gets the real explanation instead of
/// a link-local IP.
fn explain_location(
    err: object_store::Error,
    verb: &str,
    location: &str,
    source: Option<&crate::aws_creds::Source>,
) -> anyhow::Error {
    match err {
        object_store::Error::NotFound { .. } => anyhow!("{location} not found"),
        object_store::Error::Unauthenticated { .. }
        | object_store::Error::PermissionDenied { .. } => {
            anyhow!(
                "not authorized to {verb} {location}. Run `path auth s3 login` to store \
                 credentials, or check the bucket policy for the ones you have."
            )
        }
        e => {
            let msg = terse(&e);
            if matches!(source, Some(crate::aws_creds::Source::InstanceChain))
                && msg.contains("169.254.169.254")
            {
                anyhow!(
                    "failed to {verb} {location}: no credentials found (tried ~/.aws, the \
                     environment, and the EC2/ECS/EKS chain). Run `path auth s3 login` or set \
                     AWS_PROFILE."
                )
            } else {
                anyhow!("failed to {verb} {location}: {msg}")
            }
        }
    }
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
        assert_eq!(d.to_string(), "/srv/traces");
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
    fn cache_id_is_the_document_id_from_the_object_name() {
        let uri = ObjectUri::parse("s3://bkt/traces/2026-01-01-hello--path-claude-code-abc.json")
            .unwrap();
        assert_eq!(uri.cache_id(), "object-path-claude-code-abc");
        // s3a is the same store under a different scheme spelling, so
        // it must not fork the cache; neither must the container.
        let alias =
            ObjectUri::parse("s3a://other/prefix/2026-01-01-hello--path-claude-code-abc.json")
                .unwrap();
        assert_eq!(alias.cache_id(), uri.cache_id());
        let local =
            ObjectUri::parse("file:///srv/traces/2026-01-01-hello--path-claude-code-abc.json")
                .unwrap();
        assert_eq!(local.cache_id(), uri.cache_id());
    }

    #[test]
    fn cache_id_of_a_legacy_name_is_the_whole_stem_bounded() {
        let uri = ObjectUri::parse("s3://bkt/traces/2026-01-01-hello-doc.json").unwrap();
        assert_eq!(uri.cache_id(), "object-2026-01-01-hello-doc");

        let long = format!("s3://bkt/{}.json", "k".repeat(300));
        let id = ObjectUri::parse(&long).unwrap().cache_id();
        assert!(id.len() <= "object-".len() + 100, "{}", id.len());
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
    fn env_supplied_keys_resolve_as_the_environment_source() {
        let merged = merge_env(S3Settings::default(), |k| match k {
            "AWS_ACCESS_KEY_ID" => Some("AKIAENV".to_string()),
            "AWS_SECRET_ACCESS_KEY" => Some("SK".to_string()),
            _ => None,
        });
        assert!(merged.credentials_from_env);
        let resolved = merged
            .resolve_with(&crate::aws_creds::Env {
                home: None,
                var: &|_: &str| None,
                aws_cli: &|_: &str| anyhow::bail!("unused"),
                sso_login: &|_: &str| Ok(()),
                confirm: &|_: &str| false,
            })
            .unwrap();
        assert_eq!(resolved.source, crate::aws_creds::Source::Environment);
        assert_eq!(resolved.credentials.unwrap().access_key_id, "AKIAENV");

        // Stored keys stay "stored".
        let stored = S3Settings {
            access_key_id: Some("AKIASTORED".to_string()),
            secret_access_key: Some("SK".to_string()),
            ..Default::default()
        };
        assert!(!merge_env(stored.clone(), |_| None).credentials_from_env);
    }

    #[test]
    fn store_options_carry_credentials_and_endpoint() {
        let (opts, source) = store_options(
            &S3Settings {
                access_key_id: Some("AK".to_string()),
                secret_access_key: Some("SK".to_string()),
                endpoint: Some("http://127.0.0.1:9000".to_string()),
                ..Default::default()
            },
            "s3",
        )
        .unwrap();
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
        assert_eq!(source, Some(crate::aws_creds::Source::Stored));
    }

    #[test]
    fn https_endpoint_does_not_allow_http() {
        let (opts, _) = store_options(
            &S3Settings {
                endpoint: Some("https://minio.example".to_string()),
                ..Default::default()
            },
            "s3",
        )
        .unwrap();
        assert!(!opts.iter().any(|(k, _)| *k == "aws_allow_http"));
    }

    #[test]
    fn a_folder_never_resolves_credentials() {
        // A stored profile that does not exist would make resolution
        // fail — and must not even be attempted for a folder.
        let cfg = S3Settings {
            profile: Some("definitely-not-a-profile".to_string()),
            ..Default::default()
        };
        let (opts, source) = store_options(&cfg, "file").unwrap();
        assert!(opts.is_empty());
        assert_eq!(source, None);
        let err = store_options(&cfg, "s3").unwrap_err().to_string();
        assert!(err.contains("definitely-not-a-profile"), "{err}");
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
    fn a_name_leads_with_the_date_and_topic_and_ends_with_the_document_id() {
        let doc = doc_with("Add S3 support to share", "2026-08-07T09:15:00Z");
        assert_eq!(
            name_for(&doc).to_string(),
            "2026-08-07-add-s3-support-to-share--claude-code-s"
        );
    }

    #[test]
    fn a_name_is_stable_as_the_session_grows() {
        // The date comes from the *earliest* step, so appending turns
        // can't move the object and leave a duplicate behind.
        let short = doc_with("Fix the parser", "2026-08-07T09:15:00Z");
        let name = name_for(&short);

        let mut grown = short.clone();
        if let toolpath::v1::PathOrRef::Path(p) = &mut grown.paths[0] {
            let mut later = p.steps[0].clone();
            later.step.id = "s2".to_string();
            later.step.timestamp = "2026-08-09T18:00:00Z".to_string();
            p.steps.push(later);
        }
        assert_eq!(name_for(&grown), name);
    }

    #[test]
    fn a_long_prompt_is_truncated_on_a_word_boundary() {
        let doc = doc_with(
            "Add support to share and resume to and from S3 and a way to configure credentials",
            "2026-08-07T00:00:00Z",
        );
        let name = name_for(&doc).to_string();
        assert!(
            name.starts_with("2026-08-07-add-support-to-share"),
            "{name}"
        );
        assert!(name.ends_with("--claude-code-s"), "{name}");
        // The separator appears exactly once: the slugger collapses dash runs.
        assert_eq!(name.matches("--").count(), 1, "{name}");
    }

    #[test]
    fn the_id_half_is_the_session_not_the_document_id() {
        // Two documents from the same session but with different
        // `graph.id`s (a re-derive that renamed the graph, say) are the
        // same session and must land on the same key.
        let a = doc_with("Fix the parser", "2026-08-07T09:15:00Z");
        let mut b = toolpath::v1::Graph::from_json(&a.to_json().unwrap()).unwrap();
        b.graph.id = "some-other-graph-id".to_string();
        assert_eq!(name_for(&a), name_for(&b));
        assert!(
            name_for(&a).to_string().ends_with("--claude-code-s"),
            "{}",
            name_for(&a)
        );
    }

    #[test]
    fn two_sessions_take_two_keys_even_with_the_same_graph_id() {
        // The inverse: one `graph.id`, two harness sessions. Keying on
        // the session is what keeps the second from replacing the first.
        let a = doc_with("Fix the parser", "2026-08-07T09:15:00Z");
        let raw = a
            .to_json()
            .unwrap()
            .replace("claude-code://s", "claude-code://other");
        let b = toolpath::v1::Graph::from_json(&raw).unwrap();
        assert_eq!(b.graph.id, a.graph.id);
        assert_ne!(name_for(&a), name_for(&b));
        assert!(name_for(&b).to_string().ends_with("--claude-code-other"));
    }

    #[test]
    fn a_document_with_no_conversation_artifact_falls_back_to_the_graph_id() {
        // Git-derived and hand-written documents have no session identity.
        let body = serde_json::json!({
            "graph": { "id": "path-main" },
            "paths": [{
                "path": { "id": "p1", "head": "s1" },
                "steps": [{
                    "step": { "id": "s1", "parents": [], "actor": "human:alex",
                              "timestamp": "2026-08-07T00:00:00Z" },
                    "change": { "src/main.rs": { "raw": "@@ -1 +1 @@\n-a\n+b" } }
                }]
            }]
        });
        let doc = toolpath::v1::Graph::from_json(&body.to_string()).unwrap();
        assert_eq!(name_for(&doc).to_string(), "2026-08-07--path-main");
    }

    #[test]
    fn a_prompt_of_pure_punctuation_degrades_to_date_and_id() {
        let doc = doc_with("!!! ???", "2026-08-07T00:00:00Z");
        assert_eq!(name_for(&doc).to_string(), "2026-08-07--claude-code-s");
    }

    #[test]
    fn a_synthesized_title_is_not_worth_slugging() {
        // `derive_path` writes "claude-code session: abc" when it has
        // nothing better; repeating the ID would waste the legible half
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
        assert_eq!(name_for(&doc).to_string(), "2026-08-07--g1");
    }

    #[test]
    fn a_document_with_no_date_or_topic_is_named_by_its_id_alone() {
        let doc =
            toolpath::v1::Graph::from_json(r#"{"graph":{"id":"path-claude-code-abc"},"paths":[]}"#)
                .unwrap();
        assert_eq!(name_for(&doc).to_string(), "path-claude-code-abc");
    }

    #[test]
    fn the_id_is_read_back_from_after_the_last_separator() {
        assert_eq!(
            ObjectName::id_of("2026-08-07-fix-the-parser--path-claude-code-abc"),
            "path-claude-code-abc"
        );
        // Legacy names without a separator: the whole stem is the ID.
        assert_eq!(
            ObjectName::id_of("2026-08-07-fix-the-parser-doc"),
            "2026-08-07-fix-the-parser-doc"
        );
        assert_eq!(
            ObjectName::id_of("path-claude-code-abc"),
            "path-claude-code-abc"
        );
    }

    #[test]
    fn parse_splits_date_topic_and_id() {
        let p = ObjectName::parse("2026-08-07-fix-the-parser--path-claude-code-abc");
        assert_eq!(p.date.as_deref(), Some("2026-08-07"));
        assert_eq!(p.topic.as_deref(), Some("fix-the-parser"));
        assert_eq!(p.id, "path-claude-code-abc");

        let p = ObjectName::parse("2026-08-07--g1");
        assert_eq!(p.date.as_deref(), Some("2026-08-07"));
        assert_eq!(p.topic, None);
        assert_eq!(p.id, "g1");

        let p = ObjectName::parse("g1");
        assert_eq!((p.date, p.topic, p.id.as_str()), (None, None, "g1"));

        // A topic that happens to start with digits is not a date.
        let p = ObjectName::parse("2026-fixes--g1");
        assert_eq!(p.date, None);
        assert_eq!(p.topic.as_deref(), Some("2026-fixes"));
    }

    #[test]
    fn an_overlong_id_is_bounded_with_a_hash_suffix() {
        let long = "x".repeat(200);
        let name = ObjectName::new(&long, None, None).to_string();
        assert!(name.len() <= 64, "{}", name.len());
        assert!(name.starts_with(&"x".repeat(48)), "{name}");
        // 48 x's, a dash, 8 hex chars.
        assert_eq!(name.len(), 48 + 1 + 8, "{name}");
        // Two different overlong IDs get different names.
        let other = format!("{}y", "x".repeat(199));
        assert_ne!(
            ObjectName::new(&other, None, None),
            ObjectName::new(&long, None, None)
        );
    }

    #[test]
    fn an_id_never_contains_the_separator() {
        // slugify collapses dash runs, so `--` in a raw ID can't leak
        // into the name and confuse `id_of`.
        let name = ObjectName::new("weird--id", Some("2026-01-01"), Some("topic")).to_string();
        assert_eq!(name, "2026-01-01-topic--weird-id");
        assert_eq!(ObjectName::id_of(&name), "weird-id");
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
    fn terse_keeps_the_innermost_cause() {
        let inner =
            std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused");
        let err = object_store::Error::Generic {
            store: "S3",
            source: Box::new(inner),
        };
        let msg = terse(&err);
        assert!(msg.contains("connection refused"), "{msg}");
        assert!(!msg.starts_with("Generic S3 error"), "{msg}");
    }

    #[test]
    fn terse_strips_the_local_filesystem_prefix() {
        let inner = std::io::Error::other("File name too long (os error 63)");
        let err = object_store::Error::Generic {
            store: "LocalFileSystem",
            source: Box::new(inner),
        };
        let msg = terse(&err);
        assert!(!msg.contains("Generic LocalFileSystem error"), "{msg}");
        assert!(msg.contains("File name too long"), "{msg}");
    }

    #[test]
    fn an_imds_failure_with_no_credentials_explains_where_it_looked() {
        let inner = std::io::Error::other(
            "Error performing PUT http://169.254.169.254/latest/api/token in 1.5s",
        );
        let err = object_store::Error::Generic {
            store: "S3",
            source: Box::new(inner),
        };
        let msg = explain_location(
            err,
            "write",
            "s3://b/k.json",
            Some(&crate::aws_creds::Source::InstanceChain),
        )
        .to_string();
        assert!(msg.contains("no credentials found"), "{msg}");
        assert!(msg.contains("~/.aws"), "{msg}");
        assert!(!msg.contains("169.254.169.254"), "{msg}");
    }
}
