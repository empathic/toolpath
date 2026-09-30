//! The page's sessions: one [`SessionSummary`] per cached document of
//! an agent session. The sync manifest names the documents.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use rayon::prelude::*;

use crate::artifact::ArtifactType;
use crate::cache::SessionSummary;

/// Reads a [`SessionSummary`] from each cached document of an agent
/// harness that the manifest in `config_dir` names. A document that
/// cannot be read or holds no session is left out with a warning on
/// stderr.
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
    let sessions = cached
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
        .collect();
    Ok(sessions)
}
