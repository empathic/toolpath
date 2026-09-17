//! Projection of a document into a Gemini CLI chat file.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Project `path` into a Gemini session under `project_dir` and return
/// the resulting session UUID.
pub(crate) fn project_gemini(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    use toolpath_convo::ConversationProjector;
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let project_path = project_dir.to_string_lossy().to_string();

    let view = toolpath_convo::extract_conversation(path);
    let project_hash = toolpath_gemini::paths::project_hash(&project_path);
    let projector = toolpath_gemini::project::GeminiProjector::new()
        .with_project_hash(project_hash)
        .with_project_path(project_path.clone());
    let conv = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;
    if conv.session_uuid.is_empty() {
        anyhow::bail!("Projected conversation has no session UUID");
    }
    write_into_gemini_project(&conv, &project_path)?;
    Ok(conv.session_uuid)
}

/// `--project` mode: write the resume-ready layout under
/// `~/.gemini/tmp/<slot>/chats/`.
pub(crate) fn write_into_gemini_project(
    conversation: &toolpath_gemini::types::Conversation,
    project_path: &str,
) -> Result<()> {
    let resolver = toolpath_gemini::PathResolver::new();
    let chats_dir = resolver
        .chats_dir(project_path)
        .map_err(|e| anyhow::anyhow!("Cannot resolve Gemini chats dir: {}", e))?;
    std::fs::create_dir_all(&chats_dir)
        .with_context(|| format!("create {}", chats_dir.display()))?;

    // Drop a `.project_root` marker so `list_project_dirs` and any
    // tooling that walks `tmp/` can pick us up even without a
    // `projects.json` entry.
    if let Some(slot_dir) = chats_dir.parent() {
        let marker = slot_dir.join(".project_root");
        if !marker.exists() {
            let _ = std::fs::write(&marker, format!("{}\n", project_path));
        }
    }

    let main_stem = gemini_main_stem(conversation);
    let main_path = chats_dir.join(format!("{}.json", main_stem));
    let written = write_main_and_subs(conversation, &main_path)?;

    print_gemini_summary(conversation, &written, &chats_dir);
    eprintln!();
    eprintln!("Resume with:");
    eprintln!(
        "  cd {} && gemini --resume {}",
        project_path, conversation.session_uuid
    );
    Ok(())
}

/// Write `conversation.main` to `main_path` and any sub-agents to a
/// sibling `<main_dir>/<session-uuid>/<stem>.json`. Returns every path
/// written, in order.
pub(crate) fn write_main_and_subs(
    conversation: &toolpath_gemini::types::Conversation,
    main_path: &std::path::Path,
) -> Result<Vec<PathBuf>> {
    std::fs::write(main_path, serde_json::to_string_pretty(&conversation.main)?)
        .with_context(|| format!("write {}", main_path.display()))?;
    let mut written: Vec<PathBuf> = vec![main_path.to_path_buf()];

    if !conversation.sub_agents.is_empty() {
        let parent = main_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let sub_dir = parent.join(&conversation.session_uuid);
        std::fs::create_dir_all(&sub_dir)
            .with_context(|| format!("create {}", sub_dir.display()))?;
        for (i, sub) in conversation.sub_agents.iter().enumerate() {
            let stem = if sub.session_id.is_empty() {
                format!("subagent-{}", i)
            } else {
                sub.session_id.clone()
            };
            let sub_path = sub_dir.join(format!("{}.json", stem));
            std::fs::write(&sub_path, serde_json::to_string_pretty(sub)?)
                .with_context(|| format!("write {}", sub_path.display()))?;
            written.push(sub_path);
        }
    }
    Ok(written)
}

pub(crate) fn print_gemini_summary(
    conversation: &toolpath_gemini::types::Conversation,
    written: &[PathBuf],
    location: &std::path::Path,
) {
    let total_messages = conversation.main.messages.len()
        + conversation
            .sub_agents
            .iter()
            .map(|s| s.messages.len())
            .sum::<usize>();
    let sub_n = conversation.sub_agents.len();
    eprintln!(
        "Exported Gemini session {} ({} messages across main + {} sub-agent{}) → {}",
        conversation.session_uuid,
        total_messages,
        sub_n,
        if sub_n == 1 { "" } else { "s" },
        location.display()
    );
    for path in written {
        eprintln!("  wrote {}", path.display());
    }
}

/// On-disk stem for a Gemini main chat file:
/// `session-<YYYY-MM-DDTHH-MM>-<first8-of-uuid>`.
///
/// The `session-` prefix is mandatory — Gemini CLI's `--list-sessions`
/// filters on it before opening any file, so a file without it is
/// invisible to `--resume`. The short suffix matches Gemini's own
/// naming convention. Falls back to `session-<uuid>` if no timestamp is
/// available on the projected conversation.
fn gemini_main_stem(convo: &toolpath_gemini::types::Conversation) -> String {
    let short: String = convo.session_uuid.chars().take(8).collect();
    let ts = convo
        .started_at
        .or(convo.last_activity)
        .or(convo.main.start_time)
        .or(convo.main.last_updated);
    match ts {
        Some(t) => format!("session-{}-{}", t.format("%Y-%m-%dT%H-%M"), short),
        None => format!("session-{}", convo.session_uuid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::test_support::make_convo_path;

    #[test]
    fn project_gemini_returns_session_id_and_writes_chat_file() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let session_uuid = "11111111-2222-3333-4444-aaaaaaaaaaaa";
        let path = make_convo_path(&format!("gemini-cli://{}", session_uuid));

        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &fake_home);
        }
        let result = project_gemini(&path, &cwd);
        unsafe {
            match prior_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }

        let returned_id = result.expect("project_gemini should succeed");
        assert_eq!(returned_id, session_uuid);

        let gemini_tmp = fake_home.join(".gemini/tmp");
        assert!(gemini_tmp.exists(), "gemini tmp dir missing under HOME");
    }
}
