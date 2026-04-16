//! Round-trip integration test: Claude JSONL -> derive -> Path -> extract -> ConversationView -> project -> Claude JSONL

use std::fs;

use tempfile::TempDir;

use toolpath_claude::derive::{derive_path, DeriveConfig};
use toolpath_claude::project::ClaudeProjector;
use toolpath_claude::{ClaudeConvo, PathResolver};
use toolpath_convo::{extract_conversation, ConversationProjector};

const FIXTURE: &str = r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-rt","cwd":"/test/project","gitBranch":"main","message":{"role":"user","content":"Fix the authentication bug in login.rs"}}
{"uuid":"uuid-2","type":"assistant","parentUuid":"uuid-1","timestamp":"2024-01-01T00:00:01Z","sessionId":"session-rt","message":{"role":"assistant","content":[{"type":"thinking","thinking":"The bug is in the token validation"},{"type":"text","text":"I'll fix that. Let me read the file first."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"src/login.rs"}}],"model":"claude-opus-4-6","stop_reason":"tool_use","usage":{"input_tokens":100,"output_tokens":50}}}
{"uuid":"uuid-3","type":"user","parentUuid":"uuid-2","timestamp":"2024-01-01T00:00:02Z","sessionId":"session-rt","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"fn login() { validate_token(); }","is_error":false}]}}
{"uuid":"uuid-4","type":"assistant","parentUuid":"uuid-3","timestamp":"2024-01-01T00:00:03Z","sessionId":"session-rt","message":{"role":"assistant","content":[{"type":"text","text":"I see the issue. Let me fix it."},{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"src/login.rs","old_string":"validate_token()","new_string":"validate_token_v2()"}}],"model":"claude-opus-4-6","stop_reason":"tool_use","usage":{"input_tokens":200,"output_tokens":100}}}
{"uuid":"uuid-5","type":"user","parentUuid":"uuid-4","timestamp":"2024-01-01T00:00:04Z","sessionId":"session-rt","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"File written successfully","is_error":false}]}}
{"uuid":"uuid-6","type":"assistant","parentUuid":"uuid-5","timestamp":"2024-01-01T00:00:05Z","sessionId":"session-rt","message":{"role":"assistant","content":"Done! The authentication bug is fixed.","model":"claude-opus-4-6","stop_reason":"end_turn","usage":{"input_tokens":150,"output_tokens":30}}}
{"uuid":"uuid-7","type":"user","parentUuid":"uuid-6","timestamp":"2024-01-01T00:00:06Z","sessionId":"session-rt","message":{"role":"user","content":"Thanks!"}}"#;

/// Set up a temp directory mimicking `~/.claude/projects/-test-project/session-rt.jsonl`.
fn setup_fixture() -> (TempDir, ClaudeConvo) {
    let temp = TempDir::new().unwrap();
    let claude_dir = temp.path().join(".claude");
    let project_dir = claude_dir.join("projects/-test-project");
    fs::create_dir_all(&project_dir).unwrap();

    fs::write(project_dir.join("session-rt.jsonl"), FIXTURE).unwrap();

    let resolver = PathResolver::new().with_home(temp.path());
    let convo = ClaudeConvo::with_resolver(resolver);
    (temp, convo)
}

#[test]
fn roundtrip_claude_conversation() {
    let (_temp, convo) = setup_fixture();

    // Step 1: Read the original conversation and produce a reference view.
    let conversation = convo.read_conversation("/test/project", "session-rt").unwrap();
    let original_view = toolpath_claude::provider::to_view(&conversation);

    // Step 2: Derive a toolpath Path from the conversation.
    let config = DeriveConfig {
        project_path: Some("/test/project".to_string()),
        include_thinking: true,
    };
    let path = derive_path(&conversation, &config);

    // Step 3: Extract a ConversationView from the Path.
    let extracted_view = extract_conversation(&path);

    // Step 4: Project the extracted view back to a Claude Conversation.
    let projector = ClaudeProjector;
    let projected = projector.project(&extracted_view).unwrap();

    // ── Assertions: turn count ──────────────────────────────────────

    // The original view does cross-entry tool-result assembly: tool-result-only
    // user entries (uuid-3, uuid-5) are merged into the preceding assistant turn
    // rather than emitted as separate turns. So the original view should have
    // 5 turns: user, assistant, assistant, assistant, user.
    //
    // The extracted view comes from the toolpath Path which also emits
    // conversation.append steps only for real turns (not tool-result-only entries).
    assert_eq!(
        extracted_view.turns.len(),
        original_view.turns.len(),
        "turn count mismatch: extracted {} vs original {}",
        extracted_view.turns.len(),
        original_view.turns.len(),
    );

    // ── Assertions: turn content ────────────────────────────────────

    for (i, (ext, orig)) in extracted_view
        .turns
        .iter()
        .zip(original_view.turns.iter())
        .enumerate()
    {
        assert_eq!(
            ext.role, orig.role,
            "turn {i}: role mismatch: {:?} vs {:?}",
            ext.role, orig.role,
        );

        assert_eq!(
            ext.text, orig.text,
            "turn {i}: text mismatch",
        );

        assert_eq!(
            ext.tool_uses.len(),
            orig.tool_uses.len(),
            "turn {i}: tool use count mismatch: {} vs {}",
            ext.tool_uses.len(),
            orig.tool_uses.len(),
        );

        for (j, (et, ot)) in ext
            .tool_uses
            .iter()
            .zip(orig.tool_uses.iter())
            .enumerate()
        {
            assert_eq!(
                et.name, ot.name,
                "turn {i}, tool {j}: name mismatch: {} vs {}",
                et.name, ot.name,
            );
        }
    }

    // ── Assertions: token usage ─────────────────────────────────────

    let ext_usage = extracted_view.total_usage.as_ref().expect("extracted view should have total_usage");
    let orig_usage = original_view.total_usage.as_ref().expect("original view should have total_usage");
    assert_eq!(
        ext_usage.input_tokens, orig_usage.input_tokens,
        "input token mismatch",
    );
    assert_eq!(
        ext_usage.output_tokens, orig_usage.output_tokens,
        "output token mismatch",
    );

    // ── Assertions: projected conversation ──────────────────────────

    assert_eq!(
        projected.session_id, extracted_view.id,
        "projected session_id should match extracted view id",
    );
    assert!(
        !projected.entries.is_empty(),
        "projected conversation should have entries",
    );
}
