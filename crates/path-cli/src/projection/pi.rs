//! Projection of a document into a Pi session file.

use anyhow::{Context, Result};

/// Project `path` into a Pi session under `project_dir` and return the
/// resulting session id.
pub(crate) fn project_pi(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    use toolpath_convo::ConversationProjector;
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let cwd_str = project_dir.to_string_lossy().to_string();

    let view = toolpath_convo::extract_conversation(path);
    let projector = toolpath_pi::project::PiProjector::new().with_cwd(cwd_str.clone());
    let session = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;
    if session.header.id.is_empty() {
        anyhow::bail!("Projected session has no id");
    }
    write_into_pi_project(&session, &cwd_str)?;
    Ok(session.header.id)
}

/// `--project` mode: write the resume-ready layout under
/// `~/.pi/agent/sessions/--<encoded-cwd>--/<session>.jsonl`.
pub(crate) fn write_into_pi_project(session: &toolpath_pi::PiSession, cwd: &str) -> Result<()> {
    let resolver = toolpath_pi::PathResolver::new();
    let project_dir = resolver.project_dir(cwd);
    std::fs::create_dir_all(&project_dir)
        .with_context(|| format!("create {}", project_dir.display()))?;

    let stem = pi_session_stem(session);
    let out_path = project_dir.join(format!("{}.jsonl", stem));
    let bytes = serialize_pi_jsonl(session)?;
    std::fs::write(&out_path, &bytes).with_context(|| format!("write {}", out_path.display()))?;

    let entry_count = session.entries.len().saturating_sub(1); // minus header
    eprintln!(
        "Exported Pi session {} ({} entries) → {}",
        session.header.id,
        entry_count,
        out_path.display()
    );
    eprintln!();
    eprintln!("Loadable via:");
    eprintln!(
        "  path import pi --session {} --project {}",
        session.header.id, cwd
    );
    eprintln!();
    eprintln!("Open conversation with:");
    eprintln!("  pi --session {}", session.header.id);
    Ok(())
}

/// Stem for a Pi session JSONL filename. Pi's own files use
/// `<date>_<uuid>.jsonl`; for projected sessions the session id is
/// already unique enough, so we use it directly with a safe-character
/// fallback for unusual UUIDs.
fn pi_session_stem(session: &toolpath_pi::PiSession) -> String {
    // Strip any dashes-replaced-as-underscores oddities; for plain
    // UUIDs / short ids, the value is already filename-safe.
    session
        .header
        .id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Serialize a `PiSession` to its JSONL on-disk shape: header line
/// followed by one entry per line. Returns the joined string with a
/// trailing newline (Pi's reader is happy with or without it; we add
/// one to match the convention real Pi sessions use).
pub(crate) fn serialize_pi_jsonl(session: &toolpath_pi::PiSession) -> Result<String> {
    let mut lines: Vec<String> = Vec::with_capacity(session.entries.len());
    for entry in &session.entries {
        lines.push(serde_json::to_string(entry)?);
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
    fn project_pi_returns_session_id_and_writes_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let session_id = "pi-wrapper-test-session";
        let path = make_convo_path(&format!("pi://{}", session_id));

        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &fake_home);
        }
        let result = project_pi(&path, &cwd);
        unsafe {
            match prior_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }

        let returned_id = result.expect("project_pi should succeed");
        assert_eq!(returned_id, session_id);

        let pi_sessions = fake_home.join(".pi/agent/sessions");
        assert!(pi_sessions.exists(), "pi sessions dir missing");
    }
}
