//! Round-trip fidelity contract for the fields that used to be dropped:
//! thinking/text/thought signatures, `executionMode`, `responseId`,
//! `responseModel`, compaction and branch-summary structured fields, and
//! the `totalTokens` convention. All flow through TYPED IR fields
//! (`Turn.thinking_signature`, `Turn.marker`, `ToolInvocation.thought_signature`,
//! `Turn.group_id`, …) and the full Path JSON hop — no provider-namespaced
//! extras stash exists on the IR.

use std::path::Path;

use toolpath_convo::{ConversationMarker, ConversationProjector, extract_conversation};
use toolpath_openclaw::project::OpenClawProjector;
use toolpath_openclaw::reader::read_session_from_file;
use toolpath_openclaw::types::{AgentMessage, ContentBlock, Entry};
use toolpath_openclaw::{DeriveConfig, derive_path, session_to_view};

fn fixture() -> toolpath_openclaw::OpenClawSession {
    read_session_from_file(Path::new("tests/fixtures/fidelity_session.jsonl")).unwrap()
}

/// The full chain: session → view → Path → serialize+reparse JSON → view →
/// project → session. Every fidelity field must survive to the final wire.
#[test]
fn fidelity_fields_survive_the_full_chain() {
    let src = fixture();

    // Forward: typed IR fields are populated.
    let view1 = session_to_view(&src);
    let asst = view1
        .turns
        .iter()
        .find(|t| t.id == "f2")
        .expect("assistant turn");
    assert_eq!(asst.thinking_signature.as_deref(), Some("sig-think-AAAA"));
    assert_eq!(asst.text_signature.as_deref(), Some("sig-text-BBBB"));
    assert_eq!(asst.group_id.as_deref(), Some("msg_01FIDELITY"));
    assert_eq!(
        asst.response_model.as_deref(),
        Some("claude-opus-4-8-20260115")
    );
    let call = &asst.tool_uses[0];
    assert_eq!(call.thought_signature.as_deref(), Some("sig-thought-CCCC"));
    assert_eq!(call.execution_mode.as_deref(), Some("sequential"));

    let compaction = view1.turns.iter().find(|t| t.id == "f4").unwrap();
    assert_eq!(
        compaction.marker,
        Some(ConversationMarker::Compaction {
            first_kept_id: Some("f2".into()),
            tokens_before: Some(54321),
            read_files: vec!["src/a.rs".into()],
            modified_files: vec!["src/a.rs".into()],
            from_hook: Some(true),
        })
    );
    let branch = view1.turns.iter().find(|t| t.id == "f5").unwrap();
    assert_eq!(
        branch.marker,
        Some(ConversationMarker::BranchSummary {
            from_id: Some("f3".into()),
            read_files: vec!["src/b.rs".into()],
            modified_files: vec![],
            from_hook: Some(false),
        })
    );

    // Through the Path JSON hop.
    let path_doc = derive_path(&src, &DeriveConfig::default());
    let json = serde_json::to_string(&path_doc).unwrap();
    let path_doc2: toolpath::v1::Path = serde_json::from_str(&json).unwrap();
    let view2 = extract_conversation(&path_doc2);

    let asst2 = view2.turns.iter().find(|t| t.id == "f2").unwrap();
    assert_eq!(asst2.thinking_signature, asst.thinking_signature);
    assert_eq!(asst2.text_signature, asst.text_signature);
    assert_eq!(asst2.group_id, asst.group_id);
    assert_eq!(asst2.response_model, asst.response_model);
    assert_eq!(
        asst2.tool_uses[0].thought_signature.as_deref(),
        Some("sig-thought-CCCC")
    );
    assert_eq!(asst2.tool_uses[0].execution_mode.as_deref(), Some("sequential"));
    assert_eq!(
        view2.turns.iter().find(|t| t.id == "f4").unwrap().marker,
        compaction.marker
    );
    assert_eq!(
        view2.turns.iter().find(|t| t.id == "f5").unwrap().marker,
        branch.marker
    );

    // Reverse: the projected wire carries the native fields again.
    let projected = OpenClawProjector::default().project(&view2).unwrap();
    let mut saw_assistant = false;
    let mut saw_compaction = false;
    let mut saw_branch = false;
    for entry in &projected.entries {
        match entry {
            Entry::Message {
                message:
                    AgentMessage::Assistant {
                        content,
                        usage,
                        extra,
                        ..
                    },
                ..
            } => {
                saw_assistant = true;
                assert_eq!(
                    extra.get("responseId").and_then(|v| v.as_str()),
                    Some("msg_01FIDELITY")
                );
                assert_eq!(
                    extra.get("responseModel").and_then(|v| v.as_str()),
                    Some("claude-opus-4-8-20260115")
                );
                for b in content {
                    match b {
                        ContentBlock::Thinking { extra, .. } => assert_eq!(
                            extra.get("thinkingSignature").and_then(|v| v.as_str()),
                            Some("sig-think-AAAA")
                        ),
                        ContentBlock::Text { extra, .. } => assert_eq!(
                            extra.get("textSignature").and_then(|v| v.as_str()),
                            Some("sig-text-BBBB")
                        ),
                        ContentBlock::ToolCall { extra, .. } => {
                            assert_eq!(
                                extra.get("thoughtSignature").and_then(|v| v.as_str()),
                                Some("sig-thought-CCCC")
                            );
                            assert_eq!(
                                extra.get("executionMode").and_then(|v| v.as_str()),
                                Some("sequential")
                            );
                        }
                        _ => {}
                    }
                }
                // totalTokens convention: input+output+cacheRead+cacheWrite.
                assert_eq!(usage.total_tokens, 10 + 20 + 30 + 40);
            }
            Entry::Compaction {
                first_kept_entry_id,
                tokens_before,
                details,
                from_hook,
                ..
            } => {
                saw_compaction = true;
                assert_eq!(first_kept_entry_id, "f2");
                assert_eq!(*tokens_before, 54321);
                assert_eq!(*from_hook, Some(true));
                let d = details.as_ref().expect("details re-emitted");
                assert_eq!(d["readFiles"][0], "src/a.rs");
                assert_eq!(d["modifiedFiles"][0], "src/a.rs");
            }
            Entry::BranchSummary {
                from_id,
                details,
                from_hook,
                ..
            } => {
                saw_branch = true;
                assert_eq!(from_id, "f3");
                assert_eq!(*from_hook, Some(false));
                assert_eq!(details.as_ref().unwrap()["readFiles"][0], "src/b.rs");
            }
            _ => {}
        }
    }
    assert!(saw_assistant && saw_compaction && saw_branch);
}
