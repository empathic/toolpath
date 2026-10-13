//! Round-trip integration test: Claude JSONL -> derive -> Path -> extract -> ConversationView -> project -> Claude JSONL

use std::fs;

use tempfile::TempDir;

use toolpath_claude::derive::{DeriveConfig, derive_path};
use toolpath_claude::project::ClaudeProjector;
use toolpath_claude::{ClaudeConvo, PathResolver};
use toolpath_convo::{ConversationProjector, extract_conversation};

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
    let conversation = convo
        .read_conversation("/test/project", "session-rt")
        .unwrap();
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
        extracted_view.turns().count(),
        original_view.turns().count(),
        "turn count mismatch: extracted {} vs original {}",
        extracted_view.turns().count(),
        original_view.turns().count(),
    );

    // ── Assertions: turn content ────────────────────────────────────

    for (i, (ext, orig)) in extracted_view
        .turns()
        .zip(original_view.turns())
        .enumerate()
    {
        assert_eq!(
            ext.role, orig.role,
            "turn {i}: role mismatch: {:?} vs {:?}",
            ext.role, orig.role,
        );

        assert_eq!(ext.text, orig.text, "turn {i}: text mismatch",);

        assert_eq!(
            ext.tool_uses.len(),
            orig.tool_uses.len(),
            "turn {i}: tool use count mismatch: {} vs {}",
            ext.tool_uses.len(),
            orig.tool_uses.len(),
        );

        for (j, (et, ot)) in ext.tool_uses.iter().zip(orig.tool_uses.iter()).enumerate() {
            assert_eq!(
                et.name, ot.name,
                "turn {i}, tool {j}: name mismatch: {} vs {}",
                et.name, ot.name,
            );

            // Tool results must survive the round-trip
            match (&et.result, &ot.result) {
                (Some(ext_r), Some(orig_r)) => {
                    assert_eq!(
                        ext_r.content, orig_r.content,
                        "turn {i}, tool {j} ({}): result content mismatch",
                        et.name,
                    );
                    assert_eq!(
                        ext_r.is_error, orig_r.is_error,
                        "turn {i}, tool {j} ({}): is_error mismatch",
                        et.name,
                    );
                }
                (None, None) => {}
                (Some(_), None) => {
                    panic!(
                        "turn {i}, tool {j} ({}): extracted has result but original does not",
                        et.name
                    );
                }
                (None, Some(_)) => {
                    panic!(
                        "turn {i}, tool {j} ({}): original has result but extracted does not",
                        et.name
                    );
                }
            }
        }
    }

    // ── Assertions: token usage ─────────────────────────────────────

    let ext_usage = extracted_view
        .total_usage
        .as_ref()
        .expect("extracted view should have total_usage");
    let orig_usage = original_view
        .total_usage
        .as_ref()
        .expect("original view should have total_usage");
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

#[test]
fn test_cli_project_command() {
    use std::collections::HashMap;
    use toolpath::v1::{ArtifactChange, PathIdentity, Step, StepIdentity, StructuralChange};

    // 1. Build a minimal Path document with a conversation.append step.
    let artifact_key = "agent://claude/test-session";

    let init_step = Step {
        step: StepIdentity {
            id: "step-001".to_string(),
            parents: vec![],
            actor: "tool:claude-code".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        },
        change: {
            let mut m = HashMap::new();
            m.insert(
                artifact_key.to_string(),
                ArtifactChange {
                    raw: None,
                    structural: Some(StructuralChange {
                        change_type: "conversation.init".to_string(),
                        extra: HashMap::new(),
                    }),
                },
            );
            m
        },
        meta: None,
    };

    let append_step = Step {
        step: StepIdentity {
            id: "step-002".to_string(),
            parents: vec!["step-001".to_string()],
            actor: "human:user".to_string(),
            timestamp: "2024-01-01T00:00:01Z".to_string(),
        },
        change: {
            let mut m = HashMap::new();
            let mut extra = HashMap::new();
            extra.insert("role".to_string(), serde_json::json!("user"));
            extra.insert("text".to_string(), serde_json::json!("Hello"));
            m.insert(
                artifact_key.to_string(),
                ArtifactChange {
                    raw: None,
                    structural: Some(StructuralChange {
                        change_type: "conversation.append".to_string(),
                        extra,
                    }),
                },
            );
            m
        },
        meta: None,
    };

    let path = toolpath::v1::Path {
        path: PathIdentity {
            id: "test-path".to_string(),
            base: None,
            head: "step-002".to_string(),
            graph_ref: None,
        },
        steps: vec![init_step, append_step],
        meta: None,
    };

    let doc = toolpath::v1::Graph::from_path(path);

    // 2. Write the Path document to a temp file.
    let temp = tempfile::TempDir::new().unwrap();
    let input_path = temp.path().join("input.json");
    let output_path = temp.path().join("output.jsonl");

    fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();

    // 3. Run `path p export claude --input <file> --output <file>`.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_path"))
        .args([
            "p",
            "export",
            "claude",
            "--input",
            input_path.to_str().unwrap(),
            "--output",
            output_path.to_str().unwrap(),
        ])
        .output()
        .expect("Failed to execute path binary");

    assert!(
        output.status.success(),
        "path p export claude failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    // 4. Verify the output file has valid JSONL entries.
    let jsonl = fs::read_to_string(&output_path).unwrap();
    assert!(!jsonl.is_empty(), "Output JSONL should not be empty");

    let mut entry_count = 0;
    for line in jsonl.lines() {
        if line.is_empty() {
            continue;
        }
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("Each line should be valid JSON");
        assert!(parsed.is_object(), "Each JSONL entry should be an object");
        assert!(parsed.get("type").is_some(), "Entry should have type");
        // Preamble entries (permission-mode) don't have uuid;
        // conversation entries do.
        let entry_type = parsed["type"].as_str().unwrap_or("");
        if entry_type != "permission-mode" {
            assert!(parsed.get("uuid").is_some(), "Entry should have uuid");
        }
        entry_count += 1;
    }

    assert!(entry_count > 0, "Output should contain at least one entry");
}
