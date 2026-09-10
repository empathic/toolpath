//! Per-session sync state: the graph currently receiving uploads and
//! the frozen boundary it continues. Kept beside the manifest rather
//! than in it because the owned and frozen id sets are as large as the
//! session, and every query loads the manifest.

use crate::artifact::ArtifactType;
use crate::config::SYNC_STATE_DIR_NAME;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::api::GraphState;
use super::segment::FrozenBoundary;

/// The graph that currently owns this session's newest steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CurrentGraph {
    pub(crate) graph_id: String,
    pub(crate) url: String,
    /// `<server>/u/<owner>/<name>`, the destination this state is for.
    pub(crate) repo_url: String,
    pub(crate) state: GraphState,
    pub(crate) generation: i64,
    /// Owned step ids the server acknowledged, in document order.
    pub(crate) owned_ids: Vec<String>,
    pub(crate) head: String,
    /// Owned steps on the head's ancestry (see `segment::Segmentation`).
    #[serde(default)]
    pub(crate) main_line: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) base_from: Option<String>,
}

/// The frozen ancestry a continuation must respect: the newest frozen
/// graph's head plus every id frozen anywhere along the chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FrozenRecord {
    pub(crate) graph_id: String,
    pub(crate) url: String,
    pub(crate) path_id: String,
    pub(crate) head: String,
    pub(crate) step_ids: Vec<String>,
    /// The frozen head's ancestry along the whole chain.
    #[serde(default)]
    pub(crate) main_line: Vec<String>,
}

impl FrozenRecord {
    pub(crate) fn boundary(&self) -> FrozenBoundary {
        FrozenBoundary {
            document_url: self.url.clone(),
            path_id: self.path_id.clone(),
            head: self.head.clone(),
            step_ids: self.step_ids.iter().cloned().collect(),
            main_line: self.main_line.iter().cloned().collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) current: Option<CurrentGraph>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) frozen: Option<FrozenRecord>,
}

impl SessionState {
    /// Move the current graph into the frozen boundary. Its owned ids
    /// join every id frozen before it.
    pub(crate) fn freeze_current(&mut self, path_id: &str) {
        let Some(current) = self.current.take() else {
            return;
        };
        let (mut step_ids, mut main_line) = self
            .frozen
            .take()
            .map(|f| (f.step_ids, f.main_line))
            .unwrap_or_default();
        append_new(&mut step_ids, &current.owned_ids);
        append_new(&mut main_line, &current.main_line);
        self.frozen = Some(FrozenRecord {
            graph_id: current.graph_id,
            url: current.url,
            path_id: path_id.to_string(),
            head: current.head,
            step_ids,
            main_line,
        });
    }
}

fn append_new(into: &mut Vec<String>, ids: &[String]) {
    let known: HashSet<&str> = into.iter().map(String::as_str).collect();
    let mut added: Vec<String> = ids
        .iter()
        .filter(|id| !known.contains(id.as_str()))
        .cloned()
        .collect();
    into.append(&mut added);
}

fn state_path(config_dir: &Path, harness: ArtifactType, session: &str) -> Result<PathBuf> {
    if session.is_empty() || session.contains('/') || session.contains('\\') {
        bail!("invalid session id for sync state: {session:?}");
    }
    Ok(config_dir
        .join(SYNC_STATE_DIR_NAME)
        .join(format!("{}-{session}.json", harness.name())))
}

pub(crate) fn load(
    config_dir: &Path,
    harness: ArtifactType,
    session: &str,
) -> Result<SessionState> {
    let path = state_path(config_dir, harness, session)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SessionState::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

pub(crate) fn save(
    config_dir: &Path,
    harness: ArtifactType,
    session: &str,
    state: &SessionState,
) -> Result<()> {
    let path = state_path(config_dir, harness, session)?;
    let dir = path.parent().expect("state path has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        std::io::Write::write_all(&mut file, serde_json::to_string_pretty(state)?.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path).with_context(|| format!("rename into {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current(ids: &[&str], head: &str) -> CurrentGraph {
        CurrentGraph {
            graph_id: "g2".into(),
            url: "https://h/u/o/r/graphs/g2".into(),
            repo_url: "https://h/u/o/r".into(),
            state: GraphState::Mutable,
            generation: 4,
            owned_ids: ids.iter().map(|s| s.to_string()).collect(),
            head: head.into(),
            main_line: ids.iter().map(|s| s.to_string()).collect(),
            base_from: None,
        }
    }

    #[test]
    fn freezing_accumulates_ids_along_the_chain() {
        let mut state = SessionState {
            current: Some(current(&["c", "d"], "d")),
            frozen: Some(FrozenRecord {
                graph_id: "g1".into(),
                url: "https://h/u/o/r/graphs/g1".into(),
                path_id: "p".into(),
                head: "b".into(),
                step_ids: vec!["a".into(), "b".into()],
                main_line: vec!["a".into(), "b".into()],
            }),
        };
        state.freeze_current("p");
        assert!(state.current.is_none());
        let frozen = state.frozen.unwrap();
        assert_eq!(frozen.graph_id, "g2");
        assert_eq!(frozen.head, "d");
        assert_eq!(frozen.step_ids, ["a", "b", "c", "d"]);
        assert_eq!(frozen.main_line, ["a", "b", "c", "d"]);
        let boundary = frozen.boundary();
        assert_eq!(boundary.document_url, "https://h/u/o/r/graphs/g2");
        assert!(boundary.step_ids.contains("a"));
    }

    #[test]
    fn state_roundtrips_and_is_absent_by_default() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(
            load(dir.path(), ArtifactType::Codex, "s").unwrap(),
            SessionState::default()
        );
        let state = SessionState {
            current: Some(current(&["a"], "a")),
            frozen: None,
        };
        save(dir.path(), ArtifactType::Codex, "s", &state).unwrap();
        assert_eq!(load(dir.path(), ArtifactType::Codex, "s").unwrap(), state);
        assert!(load(dir.path(), ArtifactType::Codex, "a/b").is_err());
    }
}
