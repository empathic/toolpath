//! The page's sessions: one [`SessionSummary`] per cached document of
//! an agent session. The sync manifest names the documents, and
//! [`read_sessions`] reads them and sends each one's session, for a
//! thread to run while the page is open.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::mpsc::Sender;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rayon::prelude::*;

use crate::artifact::ArtifactType;
use crate::cache::SessionSummary;

/// What reading one cached document gave.
#[derive(Debug)]
pub struct DocumentRead {
    pub cache_id: String,
    /// The document's session; `None` when it holds none.
    pub session: Result<Option<SessionSummary>>,
}

/// Lists the cached documents of `types` that the manifest in
/// `config_dir` names, as cache ID and type, the newest source first.
pub fn list_documents(
    config_dir: &Path,
    types: &[ArtifactType],
) -> Result<Vec<(String, ArtifactType)>> {
    let manifest = crate::sync::load_manifest(config_dir)?;
    let mut newest: BTreeMap<&str, (Option<DateTime<Utc>>, ArtifactType)> = BTreeMap::new();
    for &artifact_type in types {
        let records = manifest.get(artifact_type.name());
        for record in records.into_iter().flat_map(|records| records.values()) {
            let Some(cache_id) = record.cache_id.as_deref() else {
                continue;
            };
            let (modified, _) = newest
                .entry(cache_id)
                .or_insert((record.modified, artifact_type));
            *modified = (*modified).max(record.modified);
        }
    }
    let mut documents: Vec<_> = newest.into_iter().collect();
    documents.sort_by_key(|(_, (modified, _))| std::cmp::Reverse(*modified));
    Ok(documents
        .into_iter()
        .map(|(cache_id, (_, artifact_type))| (cache_id.to_string(), artifact_type))
        .collect())
}

/// Reads a [`SessionSummary`] from each of `documents` on the rayon
/// pool and sends each [`DocumentRead`] when it is ready. A free thread
/// takes the next document in order, so the first documents are read
/// first. Returns when every document is read or the receiver is gone.
pub fn read_sessions(documents: &[(String, ArtifactType)], reads: &Sender<DocumentRead>) {
    // `par_iter` splits the slice among the threads, so one thread
    // alone would read the first documents.
    let _ = documents
        .iter()
        .par_bridge()
        .try_for_each(|(cache_id, harness)| {
            let read = DocumentRead {
                cache_id: cache_id.clone(),
                session: SessionSummary::read(*harness, cache_id),
            };
            reads.send(read).ok()
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documents_are_those_of_the_types_newest_source_first() {
        let config_dir = tempfile::tempdir().unwrap();
        let record = |cache_id: Option<&str>, modified: Option<&str>| {
            serde_json::json!({
                "cache_id": cache_id,
                "modified": modified,
                "synced_at": "2026-09-23T12:00:00Z",
            })
        };
        let manifest = serde_json::json!({
            "claude": {
                "old": record(Some("claude-old"), Some("2026-09-01T00:00:00Z")),
                "unstamped": record(Some("claude-unstamped"), None),
                "known-only": record(None, Some("2026-09-22T00:00:00Z")),
                "new": record(Some("claude-new"), Some("2026-09-21T00:00:00Z")),
                // Two artifacts of one document: its newest stamp counts.
                "segment": record(Some("claude-old"), Some("2026-09-10T00:00:00Z")),
            },
            "codex": {
                "rollout": record(Some("codex-rollout"), Some("2026-09-20T00:00:00Z")),
            },
            "git": {
                "main": record(Some("git-main"), Some("2026-09-23T00:00:00Z")),
            },
        });
        std::fs::write(
            config_dir.path().join(crate::config::MANIFEST_FILE_NAME),
            manifest.to_string(),
        )
        .unwrap();

        let documents = list_documents(
            config_dir.path(),
            &[ArtifactType::Claude, ArtifactType::Codex],
        )
        .unwrap();
        assert_eq!(
            documents,
            [
                ("claude-new".to_string(), ArtifactType::Claude),
                ("codex-rollout".to_string(), ArtifactType::Codex),
                ("claude-old".to_string(), ArtifactType::Claude),
                ("claude-unstamped".to_string(), ArtifactType::Claude),
            ]
        );
    }
}
