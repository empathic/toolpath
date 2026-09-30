//! The page's sessions, from the document cache: one [`Session`] per
//! cached document of an agent session. The sync manifest names the
//! documents; each document gives its facts.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use toolpath::v1::{Graph, Path as TPath};

use super::model::Session;
use crate::artifact::ArtifactType;

/// The actor prefix of an agent's step.
const AGENT_ACTOR: &str = "agent:";

/// The scheme of a local directory in `path.base.uri`.
const FILE_SCHEME: &str = "file://";

/// Reads a [`Session`] from each cached document of an agent harness
/// that the manifest in `config_dir` names. A document that cannot be
/// read or holds no session is left out with a warning on stderr.
pub fn load_sessions(config_dir: &Path) -> Result<Vec<Session>> {
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
            |(cache_id, harness)| match read_session(harness, cache_id) {
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

fn read_session(harness: ArtifactType, cache_id: &str) -> Result<Option<Session>> {
    let json = std::fs::read_to_string(crate::cache::cache_path(cache_id)?)?;
    let doc = Graph::from_json(&json)?;
    Ok(doc
        .single_path()
        .and_then(|path| summarize_session(harness, cache_id, path))
        .map(|session| Session {
            dir_exists: !session.dir.is_empty() && Path::new(&session.dir).is_dir(),
            ..session
        }))
}

/// Summarizes `path` as a row. `None` when no agent took a turn: such
/// a session has nothing to resume. `dir_exists` is `false`; the
/// caller checks the directory.
fn summarize_session(harness: ArtifactType, cache_id: &str, path: &TPath) -> Option<Session> {
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
    Some(Session {
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

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath_convo::{ConversationView, Role, Turn};

    /// The document of a session with a prompt and an answer, as a
    /// derive writes it, with `title` when the session has one.
    fn derive(title: Option<&str>, turns: &[(Role, &str, &str)]) -> TPath {
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
    fn a_row_takes_its_facts_from_the_document() {
        let path = derive(Some("Parser fix"), &TURNS);
        let session = summarize_session(ArtifactType::Claude, "claude-x", &path).unwrap();
        assert_eq!(
            session,
            Session {
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
    fn a_session_with_no_agent_turn_is_not_a_row() {
        let path = derive(None, &TURNS[..2]);
        assert_eq!(
            summarize_session(ArtifactType::Claude, "claude-x", &path),
            None
        );
    }
}
