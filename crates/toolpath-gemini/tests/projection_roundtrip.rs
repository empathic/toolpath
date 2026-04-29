//! End-to-end projection round-trip: Gemini `Conversation` → `ConversationView`
//! → `Path` (serialized) → `ConversationView` → `Conversation` via
//! [`toolpath_gemini::project::GeminiProjector`].
//!
//! Contract: after the full chain the projected conversation is
//! *functionally* equivalent to the source (messages, roles, content,
//! tool calls with results, thoughts, tokens) and the resulting
//! `ChatFile` round-trips through Gemini's own serde types — i.e. it
//! loads fine back into Gemini CLI.
//!
//! Byte-level fidelity is not a requirement (and is not achievable for
//! non-canonical fields like `resultDisplay` structured payloads that
//! only survive via provider extras).

use toolpath::v1::{Graph, Path};
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, derive_path, extract_conversation,
};
use toolpath_gemini::project::GeminiProjector;
use toolpath_gemini::provider::to_view;
use toolpath_gemini::types::{ChatFile, Conversation, GeminiContent, GeminiRole};

const MAIN_FIXTURE: &str = include_str!("fixtures/sample_main_with_subagent_ref.json");
const SUBAGENT_FIXTURE: &str = include_str!("fixtures/sample_subagent.json");

fn load_source_conversation() -> Conversation {
    let main: ChatFile = serde_json::from_str(MAIN_FIXTURE).expect("parse main fixture");
    let sub: ChatFile = serde_json::from_str(SUBAGENT_FIXTURE).expect("parse subagent fixture");
    let session_uuid = "f7cc36c0-980c-4914-ae79-439567272478".to_string();
    Conversation {
        session_uuid,
        project_path: Some("/abs/toolpath".into()),
        started_at: main.start_time,
        last_activity: main.last_updated,
        main,
        sub_agents: vec![sub],
    }
}

/// Forward → reverse, exercising the same serialisation that a `.path`
/// file on disk would go through.
fn roundtrip(source: &Conversation) -> (ConversationView, Conversation, Path) {
    let view_forward: ConversationView = to_view(source);

    // Serialize & re-parse the Path to simulate on-disk storage.
    let path = derive_path(&view_forward, &DeriveConfig::default());
    let doc = Graph::from_path(path.clone());
    let json = serde_json::to_string(&doc).expect("serialize Graph");
    let back: Graph = serde_json::from_str(&json).expect("parse Graph");
    let reparsed = back.into_single_path().expect("single-path graph");

    let view_back = extract_conversation(&reparsed);
    let projector = GeminiProjector::new()
        .with_project_hash(source.main.project_hash.clone())
        .with_project_path(
            source
                .project_path
                .clone()
                .unwrap_or_else(|| "/abs/toolpath".into()),
        );
    let rebuilt = projector.project(&view_back).expect("project");
    (view_back, rebuilt, reparsed)
}

#[test]
fn roundtrip_preserves_main_message_count_and_roles() {
    let source = load_source_conversation();
    let (_, rebuilt, _) = roundtrip(&source);

    assert_eq!(
        rebuilt.main.messages.len(),
        source.main.messages.len(),
        "message count mismatch"
    );

    for (i, (before, after)) in source
        .main
        .messages
        .iter()
        .zip(rebuilt.main.messages.iter())
        .enumerate()
    {
        assert_eq!(
            before.role, after.role,
            "role mismatch at message {}: {:?} vs {:?}",
            i, before.role, after.role
        );
    }
}

#[test]
fn roundtrip_preserves_user_message_text() {
    let source = load_source_conversation();
    let (_, rebuilt, _) = roundtrip(&source);

    let source_user_texts: Vec<String> = source
        .main
        .messages
        .iter()
        .filter(|m| m.role == GeminiRole::User)
        .map(|m| m.content.text())
        .collect();
    let rebuilt_user_texts: Vec<String> = rebuilt
        .main
        .messages
        .iter()
        .filter(|m| m.role == GeminiRole::User)
        .map(|m| m.content.text())
        .collect();

    assert_eq!(source_user_texts, rebuilt_user_texts);
}

#[test]
fn roundtrip_preserves_assistant_text_and_model() {
    let source = load_source_conversation();
    let (_, rebuilt, _) = roundtrip(&source);

    for (i, (before, after)) in source
        .main
        .messages
        .iter()
        .zip(rebuilt.main.messages.iter())
        .enumerate()
    {
        if before.role != GeminiRole::Gemini {
            continue;
        }
        assert_eq!(
            before.content.text(),
            after.content.text(),
            "assistant text mismatch at message {}",
            i
        );
        assert_eq!(before.model, after.model, "model mismatch at message {}", i);
    }
}

#[test]
fn roundtrip_preserves_info_turn() {
    // The "Request cancelled." info-type message must round-trip.
    let source = load_source_conversation();
    let (_, rebuilt, _) = roundtrip(&source);

    let info_count_before = source
        .main
        .messages
        .iter()
        .filter(|m| m.role == GeminiRole::Info)
        .count();
    let info_count_after = rebuilt
        .main
        .messages
        .iter()
        .filter(|m| m.role == GeminiRole::Info)
        .count();
    assert_eq!(info_count_before, info_count_after);

    let info_texts_before: Vec<String> = source
        .main
        .messages
        .iter()
        .filter(|m| m.role == GeminiRole::Info)
        .map(|m| m.content.text())
        .collect();
    let info_texts_after: Vec<String> = rebuilt
        .main
        .messages
        .iter()
        .filter(|m| m.role == GeminiRole::Info)
        .map(|m| m.content.text())
        .collect();
    assert_eq!(info_texts_before, info_texts_after);
}

#[test]
fn roundtrip_preserves_tool_calls_with_results() {
    let source = load_source_conversation();
    let (_, rebuilt, _) = roundtrip(&source);

    for (i, (before, after)) in source
        .main
        .messages
        .iter()
        .zip(rebuilt.main.messages.iter())
        .enumerate()
    {
        let before_calls = before.tool_calls();
        let after_calls = after.tool_calls();
        assert_eq!(
            before_calls.len(),
            after_calls.len(),
            "tool-call count mismatch at message {}: {} vs {}",
            i,
            before_calls.len(),
            after_calls.len()
        );
        for (j, (bc, ac)) in before_calls.iter().zip(after_calls.iter()).enumerate() {
            assert_eq!(
                bc.name, ac.name,
                "tool name mismatch at msg {} call {}",
                i, j
            );
            assert_eq!(
                bc.args, ac.args,
                "tool args mismatch at msg {} call {}",
                i, j
            );
            assert_eq!(
                bc.result_text(),
                ac.result_text(),
                "tool result text mismatch at msg {} call {}: \
                 {:?} vs {:?}",
                i,
                j,
                bc.result_text(),
                ac.result_text()
            );
            assert_eq!(
                bc.is_error(),
                ac.is_error(),
                "tool error status mismatch at msg {} call {}",
                i,
                j
            );
        }
    }
}

#[test]
fn roundtrip_preserves_tokens() {
    let source = load_source_conversation();
    let (_, rebuilt, _) = roundtrip(&source);

    for (i, (before, after)) in source
        .main
        .messages
        .iter()
        .zip(rebuilt.main.messages.iter())
        .enumerate()
    {
        let Some(bt) = &before.tokens else { continue };
        let at = after
            .tokens
            .as_ref()
            .unwrap_or_else(|| panic!("tokens lost at message {}", i));
        assert_eq!(bt.input, at.input, "input tokens at msg {}", i);
        assert_eq!(bt.output, at.output, "output tokens at msg {}", i);
        assert_eq!(bt.cached, at.cached, "cached tokens at msg {}", i);
        assert_eq!(bt.thoughts, at.thoughts, "thoughts tokens at msg {}", i);
        assert_eq!(bt.tool, at.tool, "tool tokens at msg {}", i);
        assert_eq!(bt.total, at.total, "total tokens at msg {}", i);
    }
}

#[test]
fn roundtrip_preserves_thoughts_losslessly() {
    let source = load_source_conversation();
    let (_, rebuilt, _) = roundtrip(&source);

    for (i, (before, after)) in source
        .main
        .messages
        .iter()
        .zip(rebuilt.main.messages.iter())
        .enumerate()
    {
        let bt = before.thoughts();
        let at = after.thoughts();
        assert_eq!(
            bt.len(),
            at.len(),
            "thought count mismatch at msg {}: {} vs {}",
            i,
            bt.len(),
            at.len()
        );
        for (j, (b, a)) in bt.iter().zip(at.iter()).enumerate() {
            assert_eq!(
                b.subject, a.subject,
                "thought subject mismatch at msg {} idx {}",
                i, j
            );
            assert_eq!(
                b.description, a.description,
                "thought description mismatch at msg {} idx {}",
                i, j
            );
            assert_eq!(
                b.timestamp, a.timestamp,
                "thought timestamp mismatch at msg {} idx {}",
                i, j
            );
        }
    }
}

#[test]
fn roundtrip_preserves_subagent() {
    let source = load_source_conversation();
    let (_, rebuilt, _) = roundtrip(&source);

    assert_eq!(rebuilt.sub_agents.len(), source.sub_agents.len());
    let before = &source.sub_agents[0];
    let after = &rebuilt.sub_agents[0];
    assert_eq!(after.kind.as_deref(), Some("subagent"));
    assert_eq!(after.session_id, before.session_id);
    assert_eq!(
        after.messages.len(),
        before.messages.len(),
        "subagent message count"
    );
    // Spot-check the subagent's tool calls round-trip.
    for (i, (bm, am)) in before
        .messages
        .iter()
        .zip(after.messages.iter())
        .enumerate()
    {
        assert_eq!(bm.role, am.role, "subagent role at {}", i);
        assert_eq!(
            bm.content.text(),
            am.content.text(),
            "subagent text at {}",
            i
        );
        assert_eq!(
            bm.tool_calls().len(),
            am.tool_calls().len(),
            "subagent tool_call count at msg {}",
            i
        );
    }
}

#[test]
fn projected_chat_file_is_serde_compatible_with_gemini_types() {
    // "Loads fine into Gemini" gate: serialize the projected Conversation's
    // main ChatFile and each sub-agent ChatFile and re-parse using Gemini's
    // own ChatFile type. This is the exact same deserialisation path Gemini
    // CLI uses via toolpath_gemini::GeminiConvo.
    let source = load_source_conversation();
    let (_, rebuilt, _) = roundtrip(&source);

    let main_json = serde_json::to_string(&rebuilt.main).expect("serialize main");
    let main_back: ChatFile = serde_json::from_str(&main_json).expect("reparse main as ChatFile");
    assert_eq!(main_back.messages.len(), rebuilt.main.messages.len());

    for (i, sub) in rebuilt.sub_agents.iter().enumerate() {
        let sub_json = serde_json::to_string(sub).expect("serialize subagent");
        let sub_back: ChatFile =
            serde_json::from_str(&sub_json).expect("reparse subagent as ChatFile");
        assert_eq!(
            sub_back.kind.as_deref(),
            Some("subagent"),
            "subagent {} lost kind marker",
            i
        );
        assert_eq!(sub_back.messages.len(), sub.messages.len());
    }
}

#[test]
fn projected_conversation_loads_via_convo_io() {
    // End-to-end: write the projector output to a temp gemini session
    // directory, then read it back through the normal `GeminiConvo`
    // reader (same code path Gemini CLI triggers for --resume).
    use std::fs;
    use tempfile::TempDir;
    use toolpath_gemini::{GeminiConvo, PathResolver};

    let source = load_source_conversation();
    let (_, rebuilt, _) = roundtrip(&source);

    let temp = TempDir::new().unwrap();
    let gemini_dir = temp.path().join(".gemini");
    let project_slot = gemini_dir.join("tmp/toolpath");
    let session_dir = project_slot.join("chats").join(&rebuilt.session_uuid);
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        gemini_dir.join("projects.json"),
        r#"{"projects":{"/abs/toolpath":"toolpath"}}"#,
    )
    .unwrap();

    // Write main to `main.json` and each sub-agent to `<sessionId>.json`
    // alongside it. This matches the on-disk layout the reader expects.
    fs::write(
        session_dir.join("main.json"),
        serde_json::to_string_pretty(&rebuilt.main).unwrap(),
    )
    .unwrap();
    for sub in &rebuilt.sub_agents {
        let name = if sub.session_id.is_empty() {
            "sub".to_string()
        } else {
            sub.session_id.clone()
        };
        fs::write(
            session_dir.join(format!("{}.json", name)),
            serde_json::to_string_pretty(sub).unwrap(),
        )
        .unwrap();
    }

    let resolver = PathResolver::new().with_gemini_dir(&gemini_dir);
    let convo = GeminiConvo::with_resolver(resolver);
    let loaded = convo
        .read_conversation("/abs/toolpath", &rebuilt.session_uuid)
        .expect("GeminiConvo reads projected session");

    assert_eq!(
        loaded.main.messages.len(),
        rebuilt.main.messages.len(),
        "main message count after re-read"
    );
    assert_eq!(
        loaded.sub_agents.len(),
        rebuilt.sub_agents.len(),
        "sub-agent count after re-read"
    );

    // And the re-read's first user message should match the original.
    let first_user_orig = source.first_user_text();
    let first_user_loaded = loaded.first_user_text();
    assert_eq!(first_user_orig, first_user_loaded);
}

#[test]
fn projected_user_message_uses_parts_content_shape() {
    // User turns must come out as `Parts([{text}])` — that's Gemini's
    // wire convention and what the CLI expects when reading.
    let source = load_source_conversation();
    let (_, rebuilt, _) = roundtrip(&source);

    for (i, m) in rebuilt.main.messages.iter().enumerate() {
        if m.role != GeminiRole::User {
            continue;
        }
        assert!(
            matches!(&m.content, GeminiContent::Parts(_)),
            "user message {} not projected as Parts",
            i
        );
    }
}
