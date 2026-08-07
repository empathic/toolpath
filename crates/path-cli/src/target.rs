//! Where `path share` uploads to, and how that gets configured once.
//!
//! A share target is either Pathbase — the hosted service, with its own
//! auth, repos, and visibility — or an object-storage
//! [`Destination`](crate::store::Destination): an S3 bucket, an
//! S3-compatible endpoint, or a plain local folder.
//!
//! The default lives in **one** place, `~/.toolpath/config.json`, and
//! not inside either credential file. "Where does my next share go?"
//! must have a single answer; if each backend's own config could claim
//! to be the default, that answer becomes a precedence puzzle the day a
//! third backend appears.
//!
//! Resolution order, highest first:
//!
//! 1. `--to <target>` on the command
//! 2. `$TOOLPATH_SHARE_TARGET`
//! 3. `default_target` in `config.json` (`path auth default <target>`)
//! 4. Pathbase
//!
//! Nothing is inferred from which credentials happen to exist. A share
//! that silently changes destination is a data-egress bug, not a
//! convenience — so the default only ever moves because someone moved
//! it. The one concession is a guard at the bottom of the order: see
//! [`resolve`].

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config::config_dir;
use crate::store::Destination;

pub(crate) const CONFIG_FILE: &str = "config.json";
pub(crate) const TARGET_ENV: &str = "TOOLPATH_SHARE_TARGET";

/// The literal that selects Pathbase, for `--to` and for the stored
/// default. Everything else is parsed as an object-storage location.
pub(crate) const PATHBASE: &str = "pathbase";

/// General CLI settings at `~/.toolpath/config.json`.
///
/// Not credentials — this file is preferences, and is the one place a
/// future non-credential setting should land rather than growing a
/// fourth file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Settings {
    /// `pathbase`, or an object-storage URL. Stored canonicalized (a
    /// folder is written as its `file://` URL) so re-reading it from a
    /// different working directory can't change where it points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_target: Option<String>,
}

pub(crate) fn settings_path() -> Result<PathBuf> {
    Ok(config_dir()?.join(CONFIG_FILE))
}

pub(crate) fn load_settings(path: &Path) -> Result<Settings> {
    Ok(crate::config::read_private_json(path)?.unwrap_or_default())
}

pub(crate) fn store_settings(path: &Path, s: &Settings) -> Result<()> {
    crate::config::write_private_json(path, s)
}

/// Where a share goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Pathbase,
    Object(Destination),
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Pathbase => f.write_str(PATHBASE),
            Target::Object(d) => write!(f, "{d}"),
        }
    }
}

impl Target {
    /// Parse a `--to` value or a stored default.
    pub(crate) fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if raw.eq_ignore_ascii_case(PATHBASE) {
            return Ok(Target::Pathbase);
        }
        Ok(Target::Object(Destination::parse(raw)?))
    }

    /// The canonical string to persist. Object destinations round-trip
    /// as URLs so a bare relative path can't be stored.
    pub(crate) fn as_stored(&self) -> String {
        match self {
            Target::Pathbase => PATHBASE.to_string(),
            Target::Object(d) => d.as_url().to_string(),
        }
    }
}

/// Where a resolved target came from, so `path auth default` and the
/// share confirmation can say *why* this is the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Flag,
    Env,
    Config,
    Builtin,
}

impl Origin {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Origin::Flag => "--to",
            Origin::Env => TARGET_ENV,
            Origin::Config => "configured default",
            Origin::Builtin => "built-in default",
        }
    }
}

/// The explicitly-chosen target, if there is one: flag, then env, then
/// the stored default. `None` means nothing has been configured and the
/// caller decides what the absence means.
fn lookup(flag: Option<&str>) -> Result<Option<(Target, Origin)>> {
    if let Some(raw) = flag.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(Some((
            Target::parse(raw).with_context(|| format!("--to {raw}"))?,
            Origin::Flag,
        )));
    }
    if let Some(raw) = std::env::var(TARGET_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        return Ok(Some((
            Target::parse(&raw).with_context(|| format!("${TARGET_ENV}"))?,
            Origin::Env,
        )));
    }
    let settings_path = settings_path()?;
    if let Some(raw) = load_settings(&settings_path)?.default_target {
        return Ok(Some((
            Target::parse(&raw)
                .with_context(|| format!("default_target in {}", settings_path.display()))?,
            Origin::Config,
        )));
    }
    Ok(None)
}

/// True when falling through to Pathbase would be a bad guess: S3
/// credentials exist, no Pathbase session does, and nothing has been
/// designated. Uploading anyway would hit the *anonymous public*
/// endpoint — a surprising place for someone who has only ever
/// configured S3.
fn fallback_is_a_bad_guess() -> Result<bool> {
    Ok(s3_configured()? && !pathbase_logged_in()?)
}

/// Resolve the share target for an upload. Errors rather than guessing
/// when the fallback would be surprising — see
/// [`fallback_is_a_bad_guess`].
pub(crate) fn resolve(flag: Option<&str>) -> Result<(Target, Origin)> {
    if let Some(hit) = lookup(flag)? {
        return Ok(hit);
    }
    if fallback_is_a_bad_guess()? {
        bail!(
            "No share target configured. S3 credentials are stored but there's no \
             default target and no Pathbase login, and defaulting to Pathbase here \
             would publish anonymously.\n\
             \n  \
             path auth default s3://my-bucket/traces   # or a folder: path auth default ~/traces\n  \
             path share --to pathbase                  # to publish to Pathbase this once"
        );
    }
    Ok((Target::Pathbase, Origin::Builtin))
}

/// What [`resolve`] would do, as a line of prose that never fails.
///
/// Status commands report the situation; they must not inherit an
/// upload-time refusal, or `path auth s3 status` would break precisely
/// when the user most needs it to explain itself.
pub(crate) fn describe_effective() -> Result<String> {
    if let Some((target, origin)) = lookup(None)? {
        return Ok(format!("{target} ({})", origin.describe()));
    }
    if fallback_is_a_bad_guess()? {
        return Ok(
            "not configured — S3 credentials are stored but no default target is set \
             (run `path auth default`)"
                .to_string(),
        );
    }
    Ok(format!(
        "{} ({})",
        Target::Pathbase,
        Origin::Builtin.describe()
    ))
}

/// The stored default, if any, plus where it is stored.
pub(crate) fn default_target() -> Result<(Option<Target>, PathBuf)> {
    let path = settings_path()?;
    let stored = load_settings(&path)?.default_target;
    let parsed = stored.as_deref().map(Target::parse).transpose()?;
    Ok((parsed, path))
}

pub(crate) fn set_default(target: &Target) -> Result<PathBuf> {
    let path = settings_path()?;
    let mut settings = load_settings(&path)?;
    settings.default_target = Some(target.as_stored());
    store_settings(&path, &settings)?;
    Ok(path)
}

pub(crate) fn clear_default() -> Result<PathBuf> {
    let path = settings_path()?;
    let mut settings = load_settings(&path)?;
    settings.default_target = None;
    store_settings(&path, &settings)?;
    Ok(path)
}

fn s3_configured() -> Result<bool> {
    Ok(crate::store::load_stored(&crate::store::config_path()?)?.is_some())
}

fn pathbase_logged_in() -> Result<bool> {
    Ok(crate::cmd_pathbase::load_session(&crate::cmd_pathbase::credentials_path()?)?.is_some())
}

/// Reject the Pathbase-only flags when the target isn't Pathbase, and
/// treat their presence as selecting Pathbase when no `--to` was given.
///
/// `--repo` / `--public` / `--anon` / `--url` / `--name` are meaningless
/// for object storage, so using one is a clear statement of intent —
/// clear enough to override a configured S3 default, and clear enough
/// that combining it with an explicit `--to s3://…` is a mistake worth
/// naming rather than silently resolving.
pub(crate) fn apply_pathbase_flags(
    resolved: (Target, Origin),
    pathbase_flags: bool,
) -> Result<(Target, Origin)> {
    let (target, origin) = resolved;
    if !pathbase_flags {
        return Ok((target, origin));
    }
    match (&target, origin) {
        (Target::Pathbase, _) => Ok((target, origin)),
        (Target::Object(_), Origin::Flag) => bail!(
            "--repo / --public / --anon / --url / --name are Pathbase options, \
             but --to names object storage. Drop the Pathbase flags, or use \
             `--to pathbase`."
        ),
        // The default said object storage, but this invocation asked
        // for something only Pathbase can do. Honor the request.
        (Target::Object(_), _) => Ok((Target::Pathbase, Origin::Flag)),
    }
}

/// Guard against a destination that can't work: an `s3://` target with
/// no credentials anywhere. A local folder never needs any, which is
/// the whole appeal of `path auth default ~/traces`.
pub(crate) fn check_reachable(target: &Target, settings: &crate::store::S3Settings) -> Result<()> {
    let Target::Object(dest) = target else {
        return Ok(());
    };
    if dest.is_local() {
        return Ok(());
    }
    if settings.access_key_id.is_some() {
        return Ok(());
    }
    // No explicit credentials — object_store will still try the AWS
    // credential chain (instance role, web identity), so this is a
    // note, not a failure.
    eprintln!(
        "note: no S3 credentials configured; falling back to the AWS credential chain. \
         Run `path auth s3 login` if the upload fails."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CONFIG_DIR_ENV, TEST_ENV_LOCK};

    /// Pin `$TOOLPATH_CONFIG_DIR` at an empty tempdir and clear
    /// `$TOOLPATH_SHARE_TARGET`, so resolution can't see the
    /// developer's real configuration.
    fn with_sandbox<R>(f: impl FnOnce(&Path) -> R) -> R {
        let temp = tempfile::tempdir().unwrap();
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_dir = std::env::var_os(CONFIG_DIR_ENV);
        let prev_target = std::env::var_os(TARGET_ENV);
        unsafe {
            std::env::set_var(CONFIG_DIR_ENV, temp.path());
            std::env::remove_var(TARGET_ENV);
        }
        let out = f(temp.path());
        unsafe {
            match prev_dir {
                Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
                None => std::env::remove_var(CONFIG_DIR_ENV),
            }
            match prev_target {
                Some(v) => std::env::set_var(TARGET_ENV, v),
                None => std::env::remove_var(TARGET_ENV),
            }
        }
        out
    }

    #[test]
    fn pathbase_is_spelled_by_name() {
        assert_eq!(Target::parse("pathbase").unwrap(), Target::Pathbase);
        assert_eq!(Target::parse("PathBase").unwrap(), Target::Pathbase);
        assert_eq!(Target::parse("pathbase").unwrap().as_stored(), "pathbase");
    }

    #[test]
    fn a_bucket_url_is_an_object_target() {
        let t = Target::parse("s3://bkt/traces").unwrap();
        assert_eq!(t.to_string(), "s3://bkt/traces");
        assert_eq!(t.as_stored(), "s3://bkt/traces");
    }

    #[test]
    fn a_folder_is_an_object_target_stored_as_a_url() {
        let t = Target::parse("/srv/traces").unwrap();
        // Displayed as a path…
        assert_eq!(t.to_string(), "/srv/traces");
        // …but persisted as a URL, so it can't drift with cwd.
        assert_eq!(t.as_stored(), "file:///srv/traces/");
    }

    #[test]
    fn nothing_configured_resolves_to_pathbase() {
        with_sandbox(|_| {
            let (target, origin) = resolve(None).unwrap();
            assert_eq!(target, Target::Pathbase);
            assert_eq!(origin, Origin::Builtin);
        });
    }

    #[test]
    fn the_flag_beats_everything() {
        with_sandbox(|_| {
            set_default(&Target::parse("s3://configured/x").unwrap()).unwrap();
            let (target, origin) = resolve(Some("s3://flagged/y")).unwrap();
            assert_eq!(target.to_string(), "s3://flagged/y");
            assert_eq!(origin, Origin::Flag);
        });
    }

    #[test]
    fn the_env_beats_the_stored_default() {
        with_sandbox(|_| {
            set_default(&Target::parse("s3://configured/x").unwrap()).unwrap();
            unsafe { std::env::set_var(TARGET_ENV, "s3://from-env/z") };
            let (target, origin) = resolve(None).unwrap();
            unsafe { std::env::remove_var(TARGET_ENV) };
            assert_eq!(target.to_string(), "s3://from-env/z");
            assert_eq!(origin, Origin::Env);
        });
    }

    #[test]
    fn the_stored_default_is_used_when_no_flag_or_env() {
        with_sandbox(|_| {
            set_default(&Target::parse("s3://configured/x").unwrap()).unwrap();
            let (target, origin) = resolve(None).unwrap();
            assert_eq!(target.to_string(), "s3://configured/x");
            assert_eq!(origin, Origin::Config);
        });
    }

    #[test]
    fn a_stored_folder_default_survives_a_round_trip() {
        with_sandbox(|_| {
            set_default(&Target::parse("/srv/traces").unwrap()).unwrap();
            let (target, _) = resolve(None).unwrap();
            assert_eq!(target.to_string(), "/srv/traces");
        });
    }

    #[test]
    fn clearing_the_default_falls_back_to_pathbase() {
        with_sandbox(|_| {
            set_default(&Target::parse("s3://configured/x").unwrap()).unwrap();
            clear_default().unwrap();
            assert_eq!(resolve(None).unwrap().1, Origin::Builtin);
        });
    }

    #[test]
    fn s3_credentials_without_a_default_refuse_to_publish_anonymously() {
        with_sandbox(|dir| {
            crate::store::store(
                &dir.join(crate::store::S3_CONFIG_FILE),
                &crate::store::S3Settings {
                    access_key_id: Some("AK".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
            let err = match resolve(None) {
                Err(e) => e.to_string(),
                Ok(t) => panic!("expected a refusal, got {t:?}"),
            };
            assert!(err.contains("path auth default"), "{err}");
        });
    }

    #[test]
    fn an_invalid_stored_default_names_the_file() {
        with_sandbox(|dir| {
            store_settings(
                &dir.join(CONFIG_FILE),
                &Settings {
                    default_target: Some("gs://nope/x".to_string()),
                },
            )
            .unwrap();
            let err = match resolve(None) {
                Err(e) => format!("{e:#}"),
                Ok(t) => panic!("expected a parse failure, got {t:?}"),
            };
            assert!(err.contains("default_target"), "{err}");
            assert!(err.contains(CONFIG_FILE), "{err}");
        });
    }

    // ── Pathbase-flag interaction ────────────────────────────────────

    #[test]
    fn pathbase_flags_override_an_object_default() {
        let resolved = (Target::parse("s3://bkt/x").unwrap(), Origin::Config);
        let (target, origin) = apply_pathbase_flags(resolved, true).unwrap();
        assert_eq!(target, Target::Pathbase);
        assert_eq!(origin, Origin::Flag);
    }

    #[test]
    fn pathbase_flags_with_an_explicit_object_to_is_an_error() {
        let resolved = (Target::parse("s3://bkt/x").unwrap(), Origin::Flag);
        let err = match apply_pathbase_flags(resolved, true) {
            Err(e) => e.to_string(),
            Ok(t) => panic!("expected a conflict, got {t:?}"),
        };
        assert!(err.contains("--to pathbase"), "{err}");
    }

    #[test]
    fn pathbase_flags_are_a_no_op_when_the_target_is_already_pathbase() {
        let resolved = (Target::Pathbase, Origin::Builtin);
        assert_eq!(
            apply_pathbase_flags(resolved.clone(), true).unwrap(),
            resolved
        );
    }

    #[test]
    fn a_local_folder_needs_no_credentials() {
        let target = Target::parse("/srv/traces").unwrap();
        check_reachable(&target, &crate::store::S3Settings::default()).unwrap();
    }
}
