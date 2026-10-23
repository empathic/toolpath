//! Projection of a document into the `cursor-agent` CLI's chat store:
//! `<config>/chats/<md5(cwd)>/<agentId>/store.db` (see
//! `toolpath_cursor::agent_store` for the layout and where it came from).

use anyhow::{Context, Result};
use toolpath_cursor::agent_store::{
    AgentStore, AgentStoreProjector, config_dir_from_env, store_path,
};

/// Write the chat for `project_dir` and return its agent id. A chat with
/// that id already there is left as it is (it may have been continued in
/// the CLI since) and resumed.
pub(crate) fn project_cursor_agent(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    let config_dir = config_dir_from_env().context("resolve the cursor-agent config directory")?;
    project_cursor_agent_into(path, project_dir, &config_dir)
}

pub(crate) fn project_cursor_agent_into(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
    config_dir: &std::path::Path,
) -> Result<String> {
    let store = build_agent_store(path)?;
    let cwd = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let db = store_path(config_dir, &cwd, store.agent_id());
    if db.exists() {
        eprintln!(
            "cursor-agent chat {} already exists for this directory; resuming the local copy.",
            store.agent_id()
        );
        return Ok(store.agent_id().to_string());
    }
    store
        .write(&db)
        .with_context(|| format!("write {}", db.display()))?;
    let root = store.root().context("decode the written root blob")?;
    eprintln!(
        "Exported cursor-agent chat {} ({} history messages, {} turns) → {}",
        store.agent_id(),
        root.messages.len(),
        root.turns.len(),
        db.display()
    );
    Ok(store.agent_id().to_string())
}

pub(crate) fn build_agent_store(path: &toolpath::v1::Path) -> Result<AgentStore> {
    use toolpath_convo::ConversationProjector;
    let view = toolpath_convo::extract_conversation(path);
    AgentStoreProjector::new()
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::test_support::make_convo_path;

    #[test]
    fn writes_the_store_where_the_cli_lists_it_and_keeps_an_existing_one() {
        let config = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let path = make_convo_path("cursor-agent://resume-test");
        let id = project_cursor_agent_into(&path, project.path(), config.path()).unwrap();

        let cwd = std::fs::canonicalize(project.path()).unwrap();
        let db = store_path(config.path(), &cwd, &id);
        let store = AgentStore::read(&db).unwrap();
        assert_eq!(store.agent_id(), id);
        let view = store.to_view().unwrap();
        assert_eq!(view.turns.len(), 1);
        assert_eq!(view.turns[0].text, "hello");

        // A second resume of the same document leaves the chat alone.
        let before = std::fs::read(&db).unwrap();
        let again = project_cursor_agent_into(&path, project.path(), config.path()).unwrap();
        assert_eq!(again, id);
        assert_eq!(std::fs::read(&db).unwrap(), before);
    }
}
