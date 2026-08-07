//! Object-storage destinations for share and resume.
//!
//! Transport is [`object_store`], so one code path covers real AWS S3,
//! any S3-compatible endpoint (Cloudflare R2, MinIO, Ceph, Backblaze
//! B2), and a plain local directory via `file://`. A folder is a
//! first-class destination, not a testing affordance: `path auth
//! default ~/Dropbox/traces` is a complete setup, needing no
//! credentials at all. It is also what the tests round-trip against,
//! so share and resume are exercised end-to-end without a network.
//!
//! The module owns two separable things:
//!
//! 1. [`Destination`] / [`ObjectUri`] — *where* a document goes. Pure
//!    URL parsing and key layout; no credentials involved. Where a
//!    document lands is a function of the destination and the
//!    document's cache id, nothing else.
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
/// every command that shares this dispatch, and `gs://` / `az://` would
/// need feature flags we don't compile in.
const SCHEMES: [&str; 4] = ["s3", "s3a", "file", "memory"];

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

    fn open(&self, cfg: &S3Settings) -> Result<(Box<dyn ObjectStore>, object_store::path::Path)> {
        object_store::parse_url_opts(&self.url, store_options(cfg))
            .with_context(|| format!("open {self}"))
    }

    /// Download the object as UTF-8 text.
    pub(crate) fn get(&self, cfg: &S3Settings) -> Result<String> {
        let (store, path) = self.open(cfg)?;
        let bytes = block_on(async {
            let result = store.get(&path).await?;
            result.bytes().await
        })
        .map_err(|e| explain(e, "read", self))?;
        String::from_utf8(bytes.to_vec()).with_context(|| format!("{self} is not valid UTF-8"))
    }

    /// Upload `body` to the object, overwriting any existing one.
    pub(crate) fn put(&self, cfg: &S3Settings, body: &[u8]) -> Result<()> {
        let (store, path) = self.open(cfg)?;
        let payload = object_store::PutPayload::from(body.to_vec());
        block_on(store.put(&path, payload))
            .map(|_| ())
            .map_err(|e| explain(e, "write", self))
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

impl Destination {
    /// Parse a user-supplied destination. See [`parse_location`] for
    /// how a scheme-less value is read.
    pub(crate) fn parse(raw: &str) -> Result<Self> {
        Ok(Destination {
            base: parse_location(raw)?,
        })
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

    /// The object a document with this cache id lands at.
    pub(crate) fn uri_for(&self, cache_id: &str) -> ObjectUri {
        let mut url = self.base.clone();
        let base_path = url.path().trim_end_matches('/').to_string();
        url.set_path(&format!("{base_path}/{cache_id}.json"));
        ObjectUri { url }
    }
}

/// Parse a user-supplied location into a URL.
///
/// A value carrying a scheme is taken at its word. A scheme-less value
/// is a **local filesystem path** — `~/traces`, `./out`, `/srv/traces`
/// — expanded, made absolute, and turned into a `file://` URL. Bare
/// strings mean folders rather than buckets because that is what people
/// type when designating a directory; an S3 bucket is named with an
/// explicit `s3://`, which is unambiguous and self-documenting.
fn parse_location(raw: &str) -> Result<Url> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("empty location");
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
fn explain(err: object_store::Error, verb: &str, uri: &ObjectUri) -> anyhow::Error {
    match err {
        object_store::Error::NotFound { .. } => anyhow!("{uri} not found"),
        object_store::Error::Unauthenticated { .. }
        | object_store::Error::PermissionDenied { .. } => {
            anyhow!(
                "not authorized to {verb} {uri}. Run `path auth s3 login` to store \
                 credentials, or check the bucket policy for the ones you have."
            )
        }
        e => anyhow!(e).context(format!("failed to {verb} {uri}")),
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
        assert!(!d.is_local());
        assert_eq!(
            d.uri_for("claude-abc").to_string(),
            "s3://bkt/pre/fix/claude-abc.json"
        );
    }

    #[test]
    fn s3_destination_without_a_prefix_writes_at_the_bucket_root() {
        let d = Destination::parse("s3://bkt").unwrap();
        assert_eq!(
            d.uri_for("claude-abc").to_string(),
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
            d.uri_for("claude-abc").to_string(),
            "/srv/traces/claude-abc.json"
        );
    }

    #[test]
    fn a_relative_path_is_made_absolute_so_a_stored_default_is_stable() {
        let d = Destination::parse("./out").unwrap();
        let expected = std::path::absolute("./out").unwrap();
        assert_eq!(d.to_string(), expected.to_string_lossy());
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
            from_path.uri_for("x").to_string(),
            from_url.uri_for("x").to_string()
        );
    }

    #[test]
    fn local_destinations_display_as_plain_paths() {
        // The printed form is what a user pastes into `path resume`,
        // and `path resume /abs/path.json` already works.
        let uri = Destination::parse("/srv/traces")
            .unwrap()
            .uri_for("claude-abc");
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
        let uri = dest.uri_for("claude-abc");
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
        let uri = dest.uri_for("claude-abc");

        uri.put(&cfg, b"{\"v\":1}").unwrap();
        uri.put(&cfg, b"{\"v\":2}").unwrap();
        assert_eq!(uri.get(&cfg).unwrap(), "{\"v\":2}");
    }

    #[test]
    fn nested_prefixes_are_created_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let dest = Destination::parse(&format!("{}/a/b/c", dir.path().display())).unwrap();
        dest.uri_for("claude-abc")
            .put(&S3Settings::default(), b"{}")
            .unwrap();
        assert!(dir.path().join("a/b/c/claude-abc.json").is_file());
    }

    #[test]
    fn missing_object_says_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let uri = Destination::parse(&dir.path().to_string_lossy())
            .unwrap()
            .uri_for("nope");
        let err = uri.get(&S3Settings::default()).unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
        assert!(err.contains("nope.json"), "{err}");
    }
}
