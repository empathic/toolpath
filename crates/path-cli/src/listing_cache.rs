//! Sidecar stat-stamp cache for picker listing metadata.
//!
//! `gather_artifacts` (the `path share` / bare-resume session picker)
//! rebuilds the same row metadata — title, cwd, last activity, message
//! count — on every invocation, and the metadata scans are the
//! expensive part of picker startup. This cache stores each artifact's
//! picker-row fields next to the same stat-level fingerprint the sync
//! manifest uses (mtime+size for file providers, row updated-at for
//! the DB providers), so a gather can reuse the row for every artifact
//! whose stamp still matches and scan only what changed.
//!
//! Lives at `$CONFIG_DIR/listing-cache.json` (0600, atomic
//! temp+rename), sibling to the sync manifest but deliberately
//! simpler: it is a CACHE. Corrupt, missing, unreadable, or
//! wrong-version content is treated as empty — never an error, never a
//! blocked picker. And unlike the manifest there is no advisory lock:
//! last-writer-wins is fine for a cache, because the worst outcome of
//! a lost write is one redundant re-scan on the next gather.
//!
//! See `docs/superpowers/specs/2026-08-04-listing-cache-design.md`.

#![cfg(not(target_os = "emscripten"))]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::artifact::{ArtifactRef, ArtifactType};
use crate::config::{LISTING_CACHE_FILE_NAME, config_dir};

/// Bump to invalidate every cache on disk (schema or semantics
/// change). Old versions load as empty and are overwritten wholesale
/// on the next dirty save.
const LISTING_CACHE_VERSION: u32 = 1;

/// The cached picker-row fields for one artifact — everything an
/// `ArtifactRow` carries except `matches_cwd`, which depends on the
/// caller's cwd and is recomputed per gather from `path`/`cwd`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CachedRow {
    /// Project path for keyed providers (claude/gemini/pi).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// Recorded cwd from the session (codex/opencode/cursor/copilot).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    pub(crate) session_id: String,
    pub(crate) title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_activity: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) message_count: Option<usize>,
}

/// One cache entry: the row plus the stat-level fingerprint of the
/// source it was scanned from. Stamps serialize exactly like the sync
/// manifest's (`modified` mtime/updated-at + `size`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CachedListing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) modified: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) size: Option<u64>,
    pub(crate) row: CachedRow,
}

impl CachedListing {
    /// Whether this entry vouches for `artifact`'s current state —
    /// the same rule as sync's stat gate: at least one stamp component
    /// must be `Some`, and both must match. All-`None` stamps mean
    /// freshness is unknowable and never read as a hit.
    pub(crate) fn matches(&self, artifact: &ArtifactRef) -> bool {
        (self.modified.is_some() || self.size.is_some())
            && self.modified == artifact.modified
            && self.size == artifact.size
    }
}

/// One provider's section: artifact id → cache entry. Claude keys by
/// chain head id (rotation-stable), matching `claude_chain_stamp`.
pub(crate) type ProviderListings = BTreeMap<String, CachedListing>;

/// On-disk shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    #[serde(default)]
    providers: BTreeMap<String, ProviderListings>,
}

/// The loaded cache plus a dirty bit. Providers a gather consults
/// replace their whole section (enumeration is authoritative, so
/// vanished artifacts drop out); sections of providers that were
/// filtered away or not installed are carried through untouched.
#[derive(Debug, Default)]
pub(crate) struct ListingCache {
    providers: BTreeMap<String, ProviderListings>,
    dirty: bool,
}

impl ListingCache {
    /// Load the cache, treating every failure mode — no config dir,
    /// missing file, unreadable file, corrupt JSON, wrong version —
    /// as an empty cache.
    pub(crate) fn load() -> Self {
        let Some(path) = file_path() else {
            return Self::default();
        };
        let Ok(json) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<FileFormat>(&json) {
            Ok(f) if f.version == LISTING_CACHE_VERSION => Self {
                providers: f.providers,
                dirty: false,
            },
            _ => Self::default(),
        }
    }

    /// A clone of one provider's section (empty when absent).
    pub(crate) fn section(&self, artifact_type: ArtifactType) -> ProviderListings {
        self.providers
            .get(artifact_type.name())
            .cloned()
            .unwrap_or_default()
    }

    /// Replace one provider's section with the refreshed one, marking
    /// the cache dirty only when something actually changed — a
    /// fully-warm gather stays clean and skips the write.
    pub(crate) fn replace_section(&mut self, artifact_type: ArtifactType, fresh: ProviderListings) {
        let name = artifact_type.name();
        // An absent section and an empty one are the same state.
        let unchanged = match self.providers.get(name) {
            Some(old) => *old == fresh,
            None => fresh.is_empty(),
        };
        if unchanged {
            return;
        }
        if fresh.is_empty() {
            self.providers.remove(name);
        } else {
            self.providers.insert(name.to_string(), fresh);
        }
        self.dirty = true;
    }

    /// Write the cache back if anything changed. Failures warn and are
    /// otherwise ignored: the gather already has its rows, and the
    /// next run simply re-scans.
    pub(crate) fn save_if_dirty(&self) {
        if !self.dirty {
            return;
        }
        if let Err(e) = self.save() {
            eprintln!("warning: listing cache not updated: {e}");
        }
    }

    fn save(&self) -> anyhow::Result<()> {
        use anyhow::Context;
        let path = file_path().ok_or_else(|| anyhow::anyhow!("no config directory"))?;
        let dir = path.parent().expect("cache path has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let file = FileFormat {
            version: LISTING_CACHE_VERSION,
            providers: self.providers.clone(),
        };
        let json = serde_json::to_string_pretty(&file)?;
        let tmp = dir.join(format!(
            "{LISTING_CACHE_FILE_NAME}.{}.tmp",
            std::process::id()
        ));
        std::fs::write(&tmp, json).with_context(|| format!("write {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))
    }
}

fn file_path() -> Option<PathBuf> {
    config_dir().ok().map(|d| d.join(LISTING_CACHE_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CONFIG_DIR_ENV, TEST_ENV_LOCK};

    /// Run `f` with `$TOOLPATH_CONFIG_DIR` pinned to a tempdir.
    fn with_cfg<F: FnOnce() -> R, R>(f: F) -> R {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os(CONFIG_DIR_ENV);
        unsafe {
            std::env::set_var(CONFIG_DIR_ENV, temp.path().join(".toolpath"));
        }
        let result = f();
        unsafe {
            match prev {
                Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
                None => std::env::remove_var(CONFIG_DIR_ENV),
            }
        }
        result
    }

    fn entry(title: &str, size: u64) -> CachedListing {
        CachedListing {
            modified: Some("2026-08-01T00:00:00Z".parse().unwrap()),
            size: Some(size),
            row: CachedRow {
                path: Some("/test/project".to_string()),
                cwd: None,
                session_id: "sess-1".to_string(),
                title: title.to_string(),
                last_activity: Some("2026-08-01T00:00:00Z".parse().unwrap()),
                message_count: Some(3),
            },
        }
    }

    fn make_ref(modified: Option<&str>, size: Option<u64>) -> ArtifactRef {
        ArtifactRef {
            artifact_type: ArtifactType::Claude,
            id: "sess-1".to_string(),
            path: Some("/test/project".to_string()),
            modified: modified.map(|m| m.parse().unwrap()),
            size,
        }
    }

    #[test]
    fn roundtrips_through_disk() {
        with_cfg(|| {
            let mut cache = ListingCache::load();
            assert!(cache.section(ArtifactType::Claude).is_empty());

            let mut section = ProviderListings::new();
            section.insert("sess-1".to_string(), entry("Add a feature", 42));
            cache.replace_section(ArtifactType::Claude, section.clone());
            cache.save_if_dirty();

            let reloaded = ListingCache::load();
            assert_eq!(reloaded.section(ArtifactType::Claude), section);
            assert!(reloaded.section(ArtifactType::Codex).is_empty());
        });
    }

    #[test]
    fn corrupt_or_wrong_version_loads_as_empty() {
        with_cfg(|| {
            let mut cache = ListingCache::load();
            let mut section = ProviderListings::new();
            section.insert("sess-1".to_string(), entry("t", 1));
            cache.replace_section(ArtifactType::Claude, section);
            cache.save_if_dirty();
            let path = file_path().unwrap();

            std::fs::write(&path, "not json").unwrap();
            assert!(
                ListingCache::load()
                    .section(ArtifactType::Claude)
                    .is_empty()
            );

            let future = serde_json::json!({
                "version": LISTING_CACHE_VERSION + 1,
                "providers": { "claude": { "sess-1": entry("t", 1) } },
            });
            std::fs::write(&path, future.to_string()).unwrap();
            assert!(
                ListingCache::load()
                    .section(ArtifactType::Claude)
                    .is_empty()
            );
        });
    }

    #[test]
    fn identical_replace_is_not_dirty() {
        with_cfg(|| {
            let mut cache = ListingCache::load();
            let mut section = ProviderListings::new();
            section.insert("sess-1".to_string(), entry("t", 1));
            cache.replace_section(ArtifactType::Claude, section.clone());
            cache.save_if_dirty();

            let mut warm = ListingCache::load();
            assert!(!warm.dirty);
            warm.replace_section(ArtifactType::Claude, section);
            assert!(!warm.dirty, "identical section must not dirty the cache");
            // And an empty replace of an already-absent section is clean.
            warm.replace_section(ArtifactType::Codex, ProviderListings::new());
            assert!(!warm.dirty);

            warm.replace_section(ArtifactType::Claude, ProviderListings::new());
            assert!(warm.dirty, "dropping every entry must dirty the cache");
        });
    }

    #[test]
    fn stamp_match_requires_a_real_stamp() {
        let e = entry("t", 42);
        assert!(e.matches(&make_ref(Some("2026-08-01T00:00:00Z"), Some(42))));
        assert!(!e.matches(&make_ref(Some("2026-08-01T00:00:00Z"), Some(43))));
        assert!(!e.matches(&make_ref(Some("2026-08-02T00:00:00Z"), Some(42))));
        assert!(!e.matches(&make_ref(None, None)));

        let unstamped = CachedListing {
            modified: None,
            size: None,
            ..e
        };
        assert!(
            !unstamped.matches(&make_ref(None, None)),
            "all-None stamps must never vouch"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cache_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        with_cfg(|| {
            let mut cache = ListingCache::load();
            let mut section = ProviderListings::new();
            section.insert("sess-1".to_string(), entry("t", 1));
            cache.replace_section(ArtifactType::Claude, section);
            cache.save_if_dirty();
            let mode = std::fs::metadata(file_path().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        });
    }
}
