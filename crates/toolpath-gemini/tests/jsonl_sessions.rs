//! gemini-cli 0.47 sessions on disk: `chats/session-<ts>-<id8>.jsonl`, a
//! record stream (header, message records upserted by id, `$set`
//! updates). The real capture `test-fixtures/gemini/convo.jsonl` is placed
//! where 0.47 writes it, and every entry point finds, resolves and reads it:
//! listing, resolving by stem and by inner `sessionId`, reading, and derive.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use toolpath_gemini::derive::{DeriveConfig, derive_path};
use toolpath_gemini::{ConversationReader, GeminiConvo, PathResolver};

const PROJECT: &str = "/work/proj";
const STEM: &str = "session-2026-05-07T18-28-9534b902";
const SESSION_ID: &str = "9534b902-4b8a-437e-ba2e-2e4db4b82fec";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/gemini/convo.jsonl")
}

/// `~/.gemini/tmp/<sha256(project)>/chats/<STEM>.jsonl`, as 0.47 lays it out.
fn place(home: &Path) -> (GeminiConvo, PathBuf) {
    let gemini = home.join(".gemini");
    let chats = gemini
        .join("tmp")
        .join(toolpath_gemini::paths::project_hash(PROJECT))
        .join("chats");
    fs::create_dir_all(&chats).unwrap();
    let file = chats.join(format!("{STEM}.jsonl"));
    fs::copy(fixture(), &file).unwrap();
    let resolver = PathResolver::new().with_home(home).with_gemini_dir(&gemini);
    (GeminiConvo::with_resolver(resolver), file)
}

/// The fixture's message records, deduplicated by id as gemini-cli does.
fn unique_message_ids() -> Vec<String> {
    let text = fs::read_to_string(fixture()).unwrap();
    let mut ids: Vec<String> = Vec::new();
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        if let Some(id) = v.get("id").and_then(|i| i.as_str())
            && !ids.iter().any(|x| x == id)
        {
            ids.push(id.to_string());
        }
    }
    ids
}

#[test]
fn a_047_session_is_listed_and_resolved() {
    let home = TempDir::new().unwrap();
    let (mgr, file) = place(home.path());
    assert_eq!(mgr.list_conversations(PROJECT).unwrap(), vec![STEM]);
    let resolver = mgr.resolver();
    assert_eq!(
        resolver.resolve_main_file(PROJECT, STEM).unwrap(),
        Some(file.clone()),
        "by file stem"
    );
    assert_eq!(
        resolver.resolve_main_file(PROJECT, SESSION_ID).unwrap(),
        Some(file),
        "by inner sessionId, as `gemini --resume <uuid>` resolves it"
    );
}

#[test]
fn a_047_session_reads_as_gemini_cli_reads_it() {
    let home = TempDir::new().unwrap();
    let (mgr, _) = place(home.path());
    let convo = mgr.read_conversation(PROJECT, STEM).unwrap();
    let ids: Vec<String> = convo.main.messages.iter().map(|m| m.id.clone()).collect();
    assert_eq!(
        ids,
        unique_message_ids(),
        "one message per id, first-seen order"
    );
    // The re-appended record (tool calls completed) is the one kept.
    assert!(
        convo.main.messages[1..]
            .iter()
            .any(|m| !m.tool_calls().is_empty())
    );
    // The last `$set` is the session's lastUpdated, not the header's.
    assert_eq!(
        convo.main.last_updated.unwrap().to_rfc3339(),
        "2026-05-07T18:30:14.544+00:00"
    );
}

#[test]
fn a_047_session_derives() {
    let home = TempDir::new().unwrap();
    let (mgr, _) = place(home.path());
    let convo = mgr.read_conversation(PROJECT, SESSION_ID).unwrap();
    let path = derive_path(&convo, &DeriveConfig::default());
    assert!(!path.steps.is_empty(), "derive sees the session");
}

#[test]
fn the_jsonl_twin_wins_over_a_projected_json() {
    // 0.47 leaves a projected `.json` untouched and keeps writing a
    // `.jsonl` with the same stem: the stream is the live session.
    let home = TempDir::new().unwrap();
    let (mgr, file) = place(home.path());
    let stale = file.with_extension("json");
    fs::write(
        &stale,
        format!(r#"{{"sessionId":"{SESSION_ID}","projectHash":"h","messages":[]}}"#),
    )
    .unwrap();
    assert_eq!(mgr.list_conversations(PROJECT).unwrap(), vec![STEM]);
    assert_eq!(
        mgr.resolver().resolve_main_file(PROJECT, STEM).unwrap(),
        Some(file)
    );
    let convo = mgr.read_conversation(PROJECT, STEM).unwrap();
    assert!(!convo.main.messages.is_empty());
    // An explicit `.json` name still reads that file.
    let only_json = ConversationReader::read_chat_file(&stale).unwrap();
    assert!(only_json.messages.is_empty());
}
