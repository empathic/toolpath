//! The page's sessions: one [`SessionSummary`] per cached document of
//! an agent session. The sync manifest names the documents. With the
//! `cache-index` feature the summaries come from the document index;
//! without it each document is parsed.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::artifact::ArtifactType;
use crate::cache::SessionSummary;

/// Reads a [`SessionSummary`] for each cached document of an agent
/// harness that the manifest in `config_dir` names. A document that
/// cannot be read is left out with a warning on stderr; one that holds
/// no session is left out.
pub fn load_sessions(config_dir: &Path) -> Result<Vec<SessionSummary>> {
    let manifest = crate::sync::load_manifest(config_dir)?;
    let cached: BTreeMap<&str, ArtifactType> = manifest
        .iter()
        .filter_map(|(name, records)| Some((ArtifactType::parse(name)?, records)))
        .filter(|(harness, _)| harness.harness().is_some())
        .flat_map(|(harness, records)| {
            records
                .values()
                .filter_map(move |record| Some((record.cache_id.as_deref()?, harness)))
        })
        .collect();
    #[cfg(feature = "cache-index")]
    let sessions = read_from_index(config_dir, &cached)?;
    #[cfg(not(feature = "cache-index"))]
    let sessions = parse_documents(cached);
    Ok(sessions)
}

/// Reads the summaries of `cached` from the document index in
/// `config_dir`, after the index parses the documents it does not hold
/// at their current stamp.
#[cfg(feature = "cache-index")]
fn read_from_index(
    config_dir: &Path,
    cached: &BTreeMap<&str, ArtifactType>,
) -> Result<Vec<SessionSummary>> {
    let documents: Vec<crate::cache::CacheEntry> = crate::cache::list_cached()?
        .into_iter()
        .filter(|document| cached.contains_key(document.id.as_str()))
        .collect();
    let listed: std::collections::HashSet<&str> = documents
        .iter()
        .map(|document| document.id.as_str())
        .collect();
    for cache_id in cached.keys().filter(|id| !listed.contains(*id)) {
        eprintln!("warning: cache entry {cache_id} left out: the document file is missing");
    }

    let mut index = crate::cache::index::Index::open(config_dir)?;
    for unreadable in index.reindex_stale(&documents)? {
        eprintln!(
            "warning: cache entry {} left out: {:#}",
            unreadable.cache_id, unreadable.error
        );
    }
    let mut sessions = index.read_sessions(&listed)?;
    for session in &mut sessions {
        session.dir_exists = !session.dir.is_empty() && Path::new(&session.dir).is_dir();
    }
    Ok(sessions)
}

/// Parses each document of `cached` for its summary.
#[cfg(not(feature = "cache-index"))]
fn parse_documents(cached: BTreeMap<&str, ArtifactType>) -> Vec<SessionSummary> {
    use rayon::prelude::*;

    cached
        .into_par_iter()
        .filter_map(
            |(cache_id, harness)| match SessionSummary::read(harness, cache_id) {
                Ok(session) => session,
                Err(e) => {
                    eprintln!("warning: cache entry {cache_id} left out: {e:#}");
                    None
                }
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::ArtifactRef;
    use crate::cache::fixtures::{TURNS, derive};
    use crate::config::{CONFIG_DIR_ENV, Config, TEST_ENV_LOCK};
    use toolpath::v1::Graph;

    /// Runs `f` with the config directory pinned to a fresh temporary
    /// directory, which `f` receives.
    fn with_cfg<F: FnOnce(&Path) -> R, R>(f: F) -> R {
        let temp = tempfile::tempdir().unwrap();
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(CONFIG_DIR_ENV, temp.path());
        }
        let result = f(temp.path());
        unsafe {
            std::env::remove_var(CONFIG_DIR_ENV);
        }
        result
    }

    /// Caches the document of a Claude session titled `title` at
    /// `cache_id` and names it in the manifest.
    fn cache_session(config_dir: &Path, cache_id: &str, title: &str) {
        let doc = Graph::from_path(derive(Some(title), &TURNS));
        let _ = crate::cache::write_cached(cache_id, &doc, true).unwrap();
        let config = Config {
            toolpath_config_dir: Some(config_dir.to_path_buf()),
            ..Default::default()
        };
        let artifact = ArtifactRef {
            artifact_type: ArtifactType::Claude,
            id: cache_id.trim_start_matches("claude-").to_string(),
            path: None,
            modified: None,
            size: None,
        };
        crate::sync::record_artifact(&config, &artifact, cache_id).unwrap();
    }

    fn titles(sessions: &[SessionSummary]) -> Vec<&str> {
        sessions.iter().map(|s| s.title.as_str()).collect()
    }

    #[test]
    fn a_document_the_manifest_names_is_a_session() {
        with_cfg(|dir| {
            cache_session(dir, "claude-b", "second");
            cache_session(dir, "claude-a", "first");
            let sessions = load_sessions(dir).unwrap();
            assert_eq!(titles(&sessions), ["first", "second"]);
            assert_eq!(sessions[0].harness, ArtifactType::Claude);
            assert_eq!(sessions[0].dir, "/work/project");
            assert!(!sessions[0].dir_exists);
        });
    }

    #[test]
    fn a_rewritten_document_gives_its_new_summary() {
        with_cfg(|dir| {
            cache_session(dir, "claude-a", "first");
            load_sessions(dir).unwrap();
            cache_session(dir, "claude-a", "second title");
            assert_eq!(titles(&load_sessions(dir).unwrap()), ["second title"]);
        });
    }

    #[test]
    fn a_missing_document_is_left_out() {
        with_cfg(|dir| {
            cache_session(dir, "claude-a", "first");
            cache_session(dir, "claude-b", "second");
            std::fs::remove_file(crate::cache::cache_path("claude-b").unwrap()).unwrap();
            assert_eq!(titles(&load_sessions(dir).unwrap()), ["first"]);
        });
    }
}
