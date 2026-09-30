//! The agent session a cached document records, as a few facts read
//! from the document: what a listing shows without the steps.

use std::path::Path as FsPath;

use anyhow::Result;
use chrono::{DateTime, Utc};
use toolpath::v1::{Graph, Path};
use toolpath_convo::Role;

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
    /// The turns on the ancestry of the head.
    pub turn_count: usize,
    /// The model of the last agent turn.
    pub model: Option<String>,
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
        let head_ancestry = toolpath::v1::query::ancestors(&path.steps, &path.path.head);
        let conversation = toolpath_convo::extract_conversation(path);
        let turns: Vec<_> = conversation
            .turns
            .iter()
            .filter(|turn| head_ancestry.contains(&turn.id))
            .collect();
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
            turn_count: turns.len(),
            model: turns
                .iter()
                .rev()
                .find(|turn| turn.role == Role::Assistant)
                .and_then(|turn| turn.model.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath_convo::{ConversationView, Role, Turn};

    /// The `n`th turn of a session, after turn `n - 1`.
    fn turn(n: usize, role: Role, text: &str, timestamp: &str) -> Turn {
        Turn {
            id: format!("t{n}"),
            parent_id: n.checked_sub(1).map(|p| format!("t{p}")),
            group_id: None,
            role,
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
        }
    }

    /// The document of a session with a prompt and an answer, as a
    /// derive writes it, with `title` when the session has one.
    fn derive(title: Option<&str>, turns: &[(Role, &str, &str)]) -> Path {
        derive_turns(
            title,
            turns
                .iter()
                .enumerate()
                .map(|(n, (role, text, timestamp))| turn(n, role.clone(), text, timestamp))
                .collect(),
        )
    }

    fn derive_turns(title: Option<&str>, turns: Vec<Turn>) -> Path {
        let conversation = ConversationView {
            id: "1a2b3c4d-0000-0000-0000-000000000000".to_string(),
            provider_id: Some("claude-code".to_string()),
            turns,
            ..Default::default()
        };
        toolpath_convo::derive_path(
            &conversation,
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
                turn_count: 3,
                model: None,
            }
        );
    }

    #[test]
    fn the_facts_come_from_the_turns_on_the_live_line() {
        let mut first = turn(1, Role::Assistant, "Started.", "2026-09-23T10:10:00Z");
        first.model = Some("claude-opus-4".to_string());
        let mut answer = turn(2, Role::Assistant, "Done.", "2026-09-23T10:30:00Z");
        answer.model = Some("claude-opus-5".to_string());
        let mut path = derive_turns(
            None,
            vec![
                turn(0, Role::User, "fix it", "2026-09-23T10:00:00Z"),
                first,
                answer,
            ],
        );
        // A dead end: a turn off the head's ancestry.
        let mut dead = path.steps[0].clone();
        dead.step.id = "dead".to_string();
        path.steps.push(dead);

        let summary = SessionSummary::from_path(ArtifactType::Claude, "claude-x", &path).unwrap();
        assert_eq!(summary.turn_count, 3);
        assert_eq!(summary.model.as_deref(), Some("claude-opus-5"));
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
