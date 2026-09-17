//! Projection of a document into a Claude Code session file.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Outcome of projecting a Path into a Claude project directory.
pub(crate) enum ClaudeProjection {
    /// The session file was written.
    Written { session_id: String },
    /// A session with this id already exists in the target project; nothing
    /// was written. Resuming the local copy is the least destructive move —
    /// it may be newer than the shared document.
    AlreadyLocal { session_id: String },
}

/// Project `path` into a Claude session under `project_dir`.
///
/// Never overwrites: if the session already exists locally the projection is
/// skipped and `AlreadyLocal` is returned (callers that want to clobber go
/// through `p export claude --force`).
pub(crate) fn project_claude(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<ClaudeProjection> {
    let conv = build_claude_conversation(path)?;
    if claude_session_file(&conv.session_id, project_dir)?.is_some() {
        return Ok(ClaudeProjection::AlreadyLocal {
            session_id: conv.session_id,
        });
    }
    let jsonl = serialize_jsonl(&conv)?;
    write_into_claude_project(&conv, &jsonl, project_dir, false)?;
    Ok(ClaudeProjection::Written {
        session_id: conv.session_id,
    })
}

/// Path of the session file for `session_id` under `project_dir`'s Claude
/// project directory, if it exists.
fn claude_session_file(session_id: &str, project_dir: &std::path::Path) -> Result<Option<PathBuf>> {
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let resolver = toolpath_claude::PathResolver::new();
    let claude_project_dir = resolver
        .project_dir(&project_dir.to_string_lossy())
        .map_err(|e| anyhow::anyhow!("Cannot resolve Claude project dir: {}", e))?;
    let candidate = claude_project_dir.join(format!("{}.jsonl", session_id));
    Ok(candidate.exists().then_some(candidate))
}

/// The Claude projection of a path, the one `p export claude` and
/// `path resume --remote` both write.
pub(crate) fn build_claude_conversation(
    path: &toolpath::v1::Path,
) -> Result<toolpath_claude::Conversation> {
    use toolpath_convo::ConversationProjector;
    let view = toolpath_convo::extract_conversation(path);
    let projector = toolpath_claude::ClaudeProjector;
    projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))
}

/// The session-file JSONL of a projected conversation.
pub(crate) fn serialize_jsonl(conv: &toolpath_claude::Conversation) -> Result<String> {
    let mut buf = Vec::new();
    toolpath_claude::ConversationWriter::write_conversation(conv, &mut buf)?;
    Ok(String::from_utf8(buf).expect("serde_json emits UTF-8"))
}

pub(crate) fn write_into_claude_project(
    conv: &toolpath_claude::Conversation,
    jsonl: &str,
    project_dir: &std::path::Path,
    force: bool,
) -> Result<PathBuf> {
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let project_path = project_dir.to_string_lossy();

    let resolver = toolpath_claude::PathResolver::new();
    let claude_project_dir = resolver
        .project_dir(&project_path)
        .map_err(|e| anyhow::anyhow!("Cannot resolve Claude project dir: {}", e))?;

    std::fs::create_dir_all(&claude_project_dir)
        .with_context(|| format!("create {}", claude_project_dir.display()))?;

    let session_id = &conv.session_id;
    let out_path = claude_project_dir.join(format!("{}.jsonl", session_id));
    if !force && out_path.exists() {
        anyhow::bail!(
            "Session {} already exists in this project ({}). Resume it directly with \
             `claude -r {}`, or pass --force to overwrite the local session file.",
            session_id,
            out_path.display(),
            session_id
        );
    }
    std::fs::write(&out_path, jsonl).with_context(|| format!("write {}", out_path.display()))?;
    Ok(out_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::test_support::make_convo_path;

    /// Build a minimal `toolpath::v1::Path` with a single `conversation.append`
    /// step using the given `artifact_key` (e.g. `"claude-code://my-session"`).
    /// The projectors read `view.id` from the first `<provider>://<id>` artifact
    /// key they see, so this gives them a non-empty session id to work with.
    #[test]
    fn project_claude_returns_session_id_and_writes_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        // Use a deterministic session id embedded in the artifact key.
        let session_id = "claude-wrapper-test-session";
        let path = make_convo_path(&format!("claude-code://{}", session_id));

        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &fake_home);
        }
        let result = project_claude(&path, &cwd);
        unsafe {
            match prior_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }

        let returned_id = match result.expect("project_claude should succeed") {
            ClaudeProjection::Written { session_id } => session_id,
            ClaudeProjection::AlreadyLocal { .. } => panic!("fresh project dir must be Written"),
        };
        assert_eq!(returned_id, session_id);

        let claude_projects = fake_home.join(".claude/projects");
        assert!(
            claude_projects.exists(),
            "claude projects dir missing under HOME"
        );
    }

    #[test]
    fn project_claude_never_overwrites_an_existing_session() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let session_id = "claude-clobber-test-session";
        let path = make_convo_path(&format!("claude-code://{}", session_id));

        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &fake_home);
        }
        let first = project_claude(&path, &cwd);
        // Simulate local divergence: the session gained content after the
        // first projection.
        let session_file = claude_session_file(session_id, &cwd)
            .unwrap()
            .expect("first projection must have written the session file");
        let mut contents = std::fs::read_to_string(&session_file).unwrap();
        contents.push_str("{\"local\":\"divergence\"}\n");
        std::fs::write(&session_file, &contents).unwrap();

        let second = project_claude(&path, &cwd);
        unsafe {
            match prior_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }

        assert!(matches!(
            first.expect("first projection should succeed"),
            ClaudeProjection::Written { .. }
        ));
        match second.expect("second projection should succeed") {
            ClaudeProjection::AlreadyLocal { session_id: id } => assert_eq!(id, session_id),
            ClaudeProjection::Written { .. } => panic!("existing session must not be re-projected"),
        }
        assert_eq!(
            std::fs::read_to_string(&session_file).unwrap(),
            contents,
            "existing session file must be untouched"
        );
    }
}
