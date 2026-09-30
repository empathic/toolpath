//! The agent session a cached document records, as a few facts read
//! from the document: what a listing shows without the steps.

use std::path::Path as FsPath;

use anyhow::Result;
use chrono::{DateTime, Utc};
use toolpath::v1::{Graph, Path};

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
    pub(crate) fn read(harness: ArtifactType, cache_id: &str) -> Result<Option<Self>> {
        let json = std::fs::read_to_string(super::cache_path(cache_id)?)?;
        let doc = Graph::from_json(&json)?;
        Ok(doc
            .single_path()
            .and_then(|path| Self::from_path(harness, cache_id, path))
            .map(|summary| Self {
                dir_exists: !summary.dir.is_empty() && FsPath::new(&summary.dir).is_dir(),
                ..summary
            }))
    }

    /// Summarizes `path`. `None` when no agent took a turn: such a
    /// session has nothing to resume. `dir_exists` is `false`; [`read`]
    /// checks the directory.
    ///
    /// [`read`]: Self::read
    fn from_path(harness: ArtifactType, cache_id: &str, path: &Path) -> Option<Self> {
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
    use super::*;
    use toolpath_convo::{ConversationView, Role, Turn};

    /// The document of a session with a prompt and an answer, as a
    /// derive writes it, with `title` when the session has one.
    fn derive(title: Option<&str>, turns: &[(Role, &str, &str)]) -> Path {
        let view = ConversationView {
            id: "1a2b3c4d-0000-0000-0000-000000000000".to_string(),
            provider_id: Some("claude-code".to_string()),
            turns: turns
                .iter()
                .enumerate()
                .map(|(n, (role, text, timestamp))| Turn {
                    id: format!("t{n}"),
                    parent_id: n.checked_sub(1).map(|p| format!("t{p}")),
                    group_id: None,
                    role: role.clone(),
                    timestamp: timestamp.to_string(),
                    text: text.to_string(),
                    thinking: None,
                    tool_uses: vec![],
                    model: None,
                    stop_reason: None,
                    token_usage: None,
                    attributed_token_usage: None,
                    environment: None,
                    delegations: vec![],
                    file_mutations: vec![],
                })
                .collect(),
            ..Default::default()
        };
        toolpath_convo::derive_path(
            &view,
            &toolpath_convo::DeriveConfig {
                base_uri: Some("file:///work/project".to_string()),
                title: title.map(str::to_string),
                ..Default::default()
            },
        )
    }

    const TURNS: [(Role, &str, &str); 3] = [
        (Role::User, "hello", "2026-09-23T10:00:00Z"),
        (Role::User, "fix the parser", "2026-09-23T10:00:05Z"),
        (Role::Assistant, "Fixed.", "2026-09-23T10:30:00Z"),
    ];

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
