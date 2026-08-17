//! In-memory Claude projection: `Path` → `toolpath_claude::Conversation`
//! → resume-ready JSONL. Shared by `p export claude` and
//! `path resume --remote`; cmd modules consume it, never the other way
//! around.

use anyhow::Result;

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

pub(crate) fn serialize_jsonl(conv: &toolpath_claude::Conversation) -> Result<String> {
    let mut lines = Vec::with_capacity(conv.preamble.len() + conv.entries.len());
    for raw in &conv.preamble {
        lines.push(serde_json::to_string(raw)?);
    }
    for entry in &conv.entries {
        lines.push(serde_json::to_string(entry)?);
    }
    // Trailing newline matters: Claude Code appends to this file on resume,
    // and without it the first appended entry lands on the last line.
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}
