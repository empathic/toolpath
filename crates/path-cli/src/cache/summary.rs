//! The agent session a cached document records, as a few facts read
//! from the document: what a listing shows without the steps.

use chrono::{DateTime, Utc};
use toolpath::v1::Path;

use crate::artifact::ArtifactType;

/// The actor prefix of an agent's step.
const AGENT_ACTOR: &str = "agent:";

/// The scheme of a local directory in `path.base.uri`.
const FILE_SCHEME: &str = "file://";

/// The agent session a cached document records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub harness: ArtifactType,
    /// The cached document a resume reads.
    pub cache_id: String,
    /// The directory the session ran in; empty when the document names
    /// none.
    pub dir: String,
    /// `dir` is a directory on this machine, so a resume can run there.
    pub dir_exists: bool,
    pub title: String,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
}

impl SessionSummary {
    /// Reads the summary of the cached document `cache_id`, which
    /// `harness` derived. `None` when the document holds no agent
    /// session: no single path, or no step by an agent.
    #[cfg(not(feature = "cache-index"))]
    pub(crate) fn read(harness: ArtifactType, cache_id: &str) -> anyhow::Result<Option<Self>> {
        let json = std::fs::read_to_string(super::cache_path(cache_id)?)?;
        let doc = toolpath::v1::Graph::from_json(&json)?;
        Ok(doc
            .single_path()
            .and_then(|path| Self::from_path(harness, cache_id, path))
            .map(|summary| Self {
                dir_exists: !summary.dir.is_empty() && std::path::Path::new(&summary.dir).is_dir(),
                ..summary
            }))
    }

    /// Summarizes `path`. `None` when no agent took a turn: such a
    /// session has nothing to resume. `dir_exists` is `false`; the
    /// caller checks the directory.
    pub(super) fn from_path(harness: ArtifactType, cache_id: &str, path: &Path) -> Option<Self> {
        if !path
            .steps
            .iter()
            .any(|step| step.step.actor.starts_with(AGENT_ACTOR))
        {
            return None;
        }
        let times = || {
            path.steps
                .iter()
                .filter_map(|step| step.step.timestamp.parse::<DateTime<Utc>>().ok())
        };
        Some(Self {
            harness,
            cache_id: cache_id.to_string(),
            dir: path
                .path
                .base
                .as_ref()
                .and_then(|base| base.uri.strip_prefix(FILE_SCHEME))
                .unwrap_or_default()
                .to_string(),
            dir_exists: false,
            title: path
                .meta
                .as_ref()
                .and_then(|meta| meta.title.clone())
                .unwrap_or_default(),
            started_at: times().min(),
            last_activity: times().max(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{TURNS, derive};
    use super::*;

    #[test]
    fn a_summary_takes_its_facts_from_the_document() {
        let path = derive(Some("Parser fix"), &TURNS);
        let summary = SessionSummary::from_path(ArtifactType::Claude, "claude-x", &path).unwrap();
        assert_eq!(
            summary,
            SessionSummary {
                harness: ArtifactType::Claude,
                cache_id: "claude-x".to_string(),
                dir: "/work/project".to_string(),
                dir_exists: false,
                title: "Parser fix".to_string(),
                started_at: Some("2026-09-23T10:00:00Z".parse().unwrap()),
                last_activity: Some("2026-09-23T10:30:00Z".parse().unwrap()),
            }
        );
    }

    #[test]
    fn a_session_with_no_agent_turn_has_no_summary() {
        let path = derive(None, &TURNS[..2]);
        assert_eq!(
            SessionSummary::from_path(ArtifactType::Claude, "claude-x", &path),
            None
        );
    }
}
