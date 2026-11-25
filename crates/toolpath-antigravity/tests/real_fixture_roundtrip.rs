//! Real-fixture derive test: three genuine `agy` 1.4.8 conversations
//! (plain text, one tool call, one leg of a `--conversation` resume),
//! captured in the `planar-lima` Linux VM with an AI Studio API key —
//! never hand-built. See `test-fixtures/antigravity/SOURCE.md` for
//! exactly how, and for the JSON envelope each assertion below is
//! checked against.

use std::path::{Path, PathBuf};
use toolpath_antigravity::reader::ConvoIO;
use toolpath_antigravity::{provider, types::Conversation};
use toolpath_convo::{Item, Role, ToolCategory};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("antigravity")
}

fn load(subdir: &str, conversation_id: &str) -> Conversation {
    let resolver =
        toolpath_antigravity::PathResolver::new().with_gemini_dir(fixtures_dir().join(subdir));
    ConvoIO::with_resolver(resolver)
        .read_conversation(conversation_id)
        .unwrap_or_else(|e| panic!("read fixture {subdir}/{conversation_id}: {e}"))
}

// ── plain text ───────────────────────────────────────────────────────

const PLAIN_ID: &str = "752ac59f-6c6f-4fa0-8b63-0cb0b2466125";

#[test]
fn plain_text_decodes_user_and_assistant_turns() {
    // Real shape: 10 steps (idx0 user_input, idx1-8 retry-attempt
    // filler steps from the 503s `agy`'s own envelope reported as
    // "attempt 8", idx9 the eventual planner_response) — this crate
    // only turns the two that carry text into Turns.
    let convo = load("plain", PLAIN_ID);
    let view = provider::to_view(&convo);
    let turns: Vec<_> = view.items.iter().filter_map(Item::as_turn).collect();
    assert_eq!(turns.len(), 2, "{turns:#?}");
    assert_eq!(turns[0].role, Role::User);
    assert_eq!(turns[0].text, "Reply with exactly: hello world test");
    assert_eq!(turns[1].role, Role::Assistant);
    assert_eq!(turns[1].text, "hello world test");
    assert!(turns[1].tool_uses.is_empty());
}

#[test]
fn plain_text_token_usage_matches_the_captured_envelope() {
    // Envelope: {"usage":{"input_tokens":6787,"output_tokens":3,"thinking_tokens":0}}
    let convo = load("plain", PLAIN_ID);
    let view = provider::to_view(&convo);
    let total = view.total_usage.unwrap();
    assert_eq!(total.input_tokens, Some(6787));
    assert_eq!(total.output_tokens, Some(3));
}

#[test]
fn plain_text_steps_report_done_status() {
    let convo = load("plain", PLAIN_ID);
    // steps.status: 3 = DONE, confirmed in SOURCE.md.
    assert!(!convo.steps.is_empty());
    assert!(convo.steps.iter().all(|s| s.status == 3));
}

// ── one tool call ────────────────────────────────────────────────────

const TOOL_ID: &str = "aa79cad9-c443-4070-9c84-f6df6d3920b2";

#[test]
fn tool_call_decodes_the_call_and_its_result() {
    let convo = load("tool", TOOL_ID);
    let view = provider::to_view(&convo);
    let turns: Vec<_> = view.items.iter().filter_map(Item::as_turn).collect();
    assert_eq!(turns.len(), 2, "{turns:#?}");
    assert_eq!(turns[0].role, Role::User);
    assert_eq!(turns[1].role, Role::Assistant);

    let tool_uses = &turns[1].tool_uses;
    assert_eq!(tool_uses.len(), 1, "{tool_uses:#?}");
    let call = &tool_uses[0];
    assert_eq!(call.name, "read_file");
    assert_eq!(call.category, Some(ToolCategory::FileRead));
    assert_eq!(
        call.input.get("Path").and_then(|v| v.as_str()),
        Some("/tmp/agy/tool/work/notes.txt")
    );
    let result = call.result.as_ref().expect("decoded read_file result");
    assert!(
        result.content.contains("the magic number is 42"),
        "{:?}",
        result.content
    );
    assert!(!result.is_error);
}

#[test]
fn tool_call_final_answer_is_the_captured_response() {
    // Envelope: {"status":"SUCCESS","response":"42\n"}
    let convo = load("tool", TOOL_ID);
    let view = provider::to_view(&convo);
    let turns: Vec<_> = view.items.iter().filter_map(Item::as_turn).collect();
    assert_eq!(turns[1].text, "42");
}

#[test]
fn tool_call_token_usage_matches_the_captured_envelope() {
    // Envelope: {"usage":{"input_tokens":13715,"output_tokens":29,"thinking_tokens":0}}
    let convo = load("tool", TOOL_ID);
    let view = provider::to_view(&convo);
    let total = view.total_usage.unwrap();
    assert_eq!(total.input_tokens, Some(13715));
    assert_eq!(total.output_tokens, Some(29));
}

// ── one leg of a `--conversation` resume ────────────────────────────

const MULTI_ID: &str = "299f4ce3-f4ee-404d-a950-d2a5bbeae09d";

#[test]
fn multi_turn_resume_decodes_both_user_turns() {
    // Real shape: idx0 user_input ("teal"), idx1 planner_response
    // ("Got it..."), idx2 a SECOND user_input (the resumed turn's
    // question) written to the store before generation was attempted,
    // idx3/idx4 filler from the 429 that then failed it (see
    // SOURCE.md). So the store genuinely has two user turns even
    // though only the first got an answer — that's what this test
    // checks, rather than asserting a fixed total turn count.
    let convo = load("multi", MULTI_ID);
    let view = provider::to_view(&convo);
    let turns: Vec<_> = view.items.iter().filter_map(Item::as_turn).collect();
    let user_texts: Vec<&str> = turns
        .iter()
        .filter(|t| t.role == Role::User)
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(
        user_texts,
        vec![
            "My favorite color is teal. Remember that.",
            "What is my favorite color? Reply with just the color word.",
        ],
        "{turns:#?}"
    );
    let assistant_texts: Vec<&str> = turns
        .iter()
        .filter(|t| t.role == Role::Assistant)
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(
        assistant_texts,
        vec!["Got it! I'll remember that your favorite color is teal."],
        "{turns:#?}"
    );
}

// ── conversation_summaries.db (a real, if nearly-empty, cache row) ──

#[test]
fn summaries_db_lists_the_captured_conversation() {
    let resolver =
        toolpath_antigravity::PathResolver::new().with_gemini_dir(fixtures_dir().join("tool"));
    let io = ConvoIO::with_resolver(resolver);
    let summaries = io.list_summaries().unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].conversation_id, TOOL_ID);
    assert_eq!(summaries[0].step_count, 4);
}
