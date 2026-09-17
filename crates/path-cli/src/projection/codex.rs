//! Projection of a document into a Codex CLI rollout file.

use anyhow::{Context, Result};

/// Project `path` into a Codex session and return the resulting session id.
pub(crate) fn project_codex(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    use toolpath_convo::ConversationProjector;
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let cwd_str = project_dir.to_string_lossy().to_string();

    let view = toolpath_convo::extract_conversation(path);
    let projector = toolpath_codex::project::CodexProjector::new().with_cwd(cwd_str);
    let session = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;
    if session.id.is_empty() {
        anyhow::bail!("Projected session has no id");
    }
    write_into_codex_project(&session)?;
    Ok(session.id)
}

/// `--project` mode: write the resume-ready layout under
/// `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. The date partitioning
/// matches Codex's own filing convention; the timestamp prefix is
/// derived from the session's first event time.
pub(crate) fn write_into_codex_project(session: &toolpath_codex::Session) -> Result<()> {
    let session_ts = codex_session_timestamp(session)?;
    let resolver = toolpath_codex::PathResolver::new();
    let sessions_root = resolver
        .sessions_root()
        .map_err(|e| anyhow::anyhow!("Cannot resolve Codex sessions dir: {}", e))?;

    // sessions/YYYY/MM/DD/
    let date_dir = sessions_root
        .join(session_ts.format("%Y").to_string())
        .join(session_ts.format("%m").to_string())
        .join(session_ts.format("%d").to_string());
    std::fs::create_dir_all(&date_dir).with_context(|| format!("create {}", date_dir.display()))?;

    let stem = codex_rollout_stem(session, &session_ts);
    let out_path = date_dir.join(format!("{}.jsonl", stem));
    let bytes = serialize_codex_jsonl(session)?;
    std::fs::write(&out_path, &bytes).with_context(|| format!("write {}", out_path.display()))?;

    // `codex resume` reads from state_5.sqlite, not the filesystem;
    // without a thread row the rollout file is invisible.
    let codex_dir = resolver
        .codex_dir()
        .map_err(|e| anyhow::anyhow!("Cannot resolve ~/.codex dir: {}", e))?;
    let registration = register_codex_thread(&codex_dir, session, &out_path, &session_ts);

    eprintln!(
        "Exported Codex session {} ({} lines) → {}",
        session.id,
        session.lines.len(),
        out_path.display()
    );
    match registration {
        Ok(true) => eprintln!("  registered in {}/state_5.sqlite", codex_dir.display()),
        Ok(false) => eprintln!(
            "  warning: state_5.sqlite not found at {} — `codex resume` won't see this session",
            codex_dir.display()
        ),
        Err(e) => eprintln!(
            "  warning: failed to register thread in state_5.sqlite: {} — `codex resume` may not see this session",
            e
        ),
    }
    eprintln!();
    eprintln!("Loadable via:");
    eprintln!("  path import codex --session {}", session.id);
    eprintln!();
    eprintln!("Open conversation with:");
    eprintln!("  codex resume {}", session.id);
    Ok(())
}

/// Schema-fragile: targets the `state_5.sqlite` shape. Returns
/// `Ok(false)` when the DB doesn't exist.
fn register_codex_thread(
    codex_dir: &std::path::Path,
    session: &toolpath_codex::Session,
    rollout_path: &std::path::Path,
    session_ts: &chrono::DateTime<chrono::Utc>,
) -> std::result::Result<bool, rusqlite::Error> {
    let db_path = codex_dir.join("state_5.sqlite");
    if !db_path.exists() {
        return Ok(false);
    }
    let conn = rusqlite::Connection::open(&db_path)?;

    let created_at = session_ts.timestamp();
    let created_at_ms = session_ts.timestamp_millis();
    let (cwd, model_provider, cli_version) = match session.meta() {
        Some(m) => (
            m.cwd.to_string_lossy().to_string(),
            m.model_provider.clone().unwrap_or_else(|| "openai".into()),
            m.cli_version,
        ),
        None => ("/".to_string(), "openai".to_string(), String::new()),
    };
    let first_user_message = first_user_message_text(session);
    let title = first_user_message.chars().take(200).collect::<String>();
    let has_user_event: i64 = if first_user_message.is_empty() { 0 } else { 1 };
    let sandbox_policy_json = serde_json::json!({
        "type": "workspace-write",
        "writable_roots": [],
        "network_access": false,
        "exclude_tmpdir_env_var": false,
        "exclude_slash_tmp": false,
    })
    .to_string();

    conn.execute(
        "INSERT OR REPLACE INTO threads (
            id, rollout_path, created_at, updated_at, source, model_provider,
            cwd, title, sandbox_policy, approval_mode, tokens_used, has_user_event,
            archived, cli_version, first_user_message, memory_mode,
            created_at_ms, updated_at_ms
         ) VALUES (
            ?1, ?2, ?3, ?4, 'cli', ?5,
            ?6, ?7, ?8, 'on-request', 0, ?9,
            0, ?10, ?11, 'enabled',
            ?12, ?13
         )",
        rusqlite::params![
            session.id,
            rollout_path.to_string_lossy(),
            created_at,
            created_at,
            model_provider,
            cwd,
            title,
            sandbox_policy_json,
            has_user_event,
            cli_version,
            first_user_message,
            created_at_ms,
            created_at_ms,
        ],
    )?;
    Ok(true)
}

fn first_user_message_text(session: &toolpath_codex::Session) -> String {
    use toolpath_codex::types::{ResponseItem, RolloutItem};
    for line in &session.lines {
        if let RolloutItem::ResponseItem(ResponseItem::Message(m)) = line.item()
            && m.role == "user"
        {
            let t = m.text();
            if !t.is_empty() {
                return t;
            }
        }
    }
    String::new()
}

/// Pull the session's intended timestamp out of its session_meta line.
/// Falls back to the wall clock at projection time when the metadata
/// is missing or unparseable — Codex's directory layout requires SOME
/// date to file under.
fn codex_session_timestamp(
    session: &toolpath_codex::Session,
) -> Result<chrono::DateTime<chrono::Utc>> {
    if let Some(meta) = session.meta()
        && let Ok(dt) = meta.timestamp.parse::<chrono::DateTime<chrono::Utc>>()
    {
        return Ok(dt);
    }
    Ok(chrono::Utc::now())
}

/// Filename stem matching Codex's own naming:
/// `rollout-YYYY-MM-DDThh-mm-ss-<session-uuid>`.
fn codex_rollout_stem(
    session: &toolpath_codex::Session,
    ts: &chrono::DateTime<chrono::Utc>,
) -> String {
    let stamp = ts.format("%Y-%m-%dT%H-%M-%S").to_string();
    let uuid_safe: String = session
        .id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    format!("rollout-{}-{}", stamp, uuid_safe)
}

/// Serialize a Codex `Session` to JSONL — one [`RolloutLine`] per line,
/// trailing newline included. This matches the on-disk shape Codex's
/// rollout recorder writes.
pub(crate) fn serialize_codex_jsonl(session: &toolpath_codex::Session) -> Result<String> {
    let mut lines: Vec<String> = Vec::with_capacity(session.lines.len());
    for line in &session.lines {
        lines.push(serde_json::to_string(line)?);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::test_support::make_convo_path;

    #[test]
    fn project_codex_returns_session_id_and_writes_rollout() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let session_uuid = "019dabc6-cccc-dddd-eeee-ffffffffffff";
        let path = make_convo_path(&format!("codex://{}", session_uuid));

        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &fake_home);
        }
        let result = project_codex(&path, &cwd);
        unsafe {
            match prior_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }

        let returned_id = result.expect("project_codex should succeed");
        assert_eq!(returned_id, session_uuid);

        let codex_sessions = fake_home.join(".codex/sessions");
        assert!(codex_sessions.exists(), "codex sessions dir missing");
    }
}
