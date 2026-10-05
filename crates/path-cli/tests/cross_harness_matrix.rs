//! Cross-harness translation matrix.
//!
//! For every pair `(source, target)` of harnesses, run
//! `source → IR → target → IR → target` and assert a small set of
//! invariants on the IR. Cells aggregate every invariant failure rather
//! than aborting on the first; one cell's failures land grouped under
//! its label so triage is possible from a single test run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;
use toolpath::v1::Graph;
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, Role, Turn, derive_path,
    extract_conversation,
};

trait Harness {
    fn name(&self) -> &'static str;
    /// Project an IR view through this harness's native shape and forward
    /// back to IR. The returned view is what a downstream IR consumer
    /// would see after this harness's native session round-tripped.
    fn roundtrip(&self, view: &ConversationView) -> ConversationView;
    /// Load this harness's captured real-world fixture (from
    /// `test-fixtures/<harness>/convo.{jsonl,json}` at the workspace
    /// root) through the harness's native reader and forward it to IR.
    /// Returns `None` if the fixture isn't on disk.
    fn load_fixture(&self) -> Option<ConversationView>;
    /// Project the IR view, serialize the native output to its on-disk
    /// wire format, and re-parse through the harness's own reader.
    /// Returns Err with a descriptive message if the wire round-trip
    /// fails (the projector wrote something the reader rejects).
    /// Some harnesses don't have a JSON/JSONL wire (opencode is SQL);
    /// those can return Ok(()) with the rationale documented inline.
    fn schema_validates(&self, view: &ConversationView) -> Result<(), String>;
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
}

struct ClaudeHarness;
impl Harness for ClaudeHarness {
    fn name(&self) -> &'static str {
        "claude"
    }
    fn roundtrip(&self, view: &ConversationView) -> ConversationView {
        let projector = toolpath_claude::ClaudeProjector;
        let convo = projector.project(view).expect("claude project");
        toolpath_claude::provider::to_view(&convo)
    }
    fn load_fixture(&self) -> Option<ConversationView> {
        let path = fixtures_dir().join("claude/convo.jsonl");
        if !path.exists() {
            return None;
        }
        let convo = toolpath_claude::ConversationReader::read_conversation(&path)
            .expect("claude fixture parse");
        Some(toolpath_claude::provider::to_view(&convo))
    }
    fn schema_validates(&self, view: &ConversationView) -> Result<(), String> {
        let projector = toolpath_claude::ClaudeProjector;
        let convo = projector
            .project(view)
            .map_err(|e| format!("project: {}", e))?;
        let mut lines: Vec<String> = Vec::new();
        for raw in &convo.preamble {
            lines.push(serde_json::to_string(raw).map_err(|e| format!("preamble: {}", e))?);
        }
        for entry in &convo.entries {
            lines.push(serde_json::to_string(entry).map_err(|e| format!("entry: {}", e))?);
        }
        let tmp = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .map_err(|e| format!("tempfile: {}", e))?;
        std::fs::write(tmp.path(), lines.join("\n")).map_err(|e| format!("write: {}", e))?;
        toolpath_claude::ConversationReader::read_conversation(tmp.path())
            .map_err(|e| format!("re-read: {}", e))?;
        Ok(())
    }
}

struct CodexHarness;
impl Harness for CodexHarness {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn roundtrip(&self, view: &ConversationView) -> ConversationView {
        let projector = toolpath_codex::project::CodexProjector::new();
        let session = projector.project(view).expect("codex project");
        toolpath_codex::to_view(&session)
    }
    fn load_fixture(&self) -> Option<ConversationView> {
        let path = fixtures_dir().join("codex/convo.jsonl");
        if !path.exists() {
            return None;
        }
        let session =
            toolpath_codex::RolloutReader::read_session(&path).expect("codex fixture parse");
        Some(toolpath_codex::to_view(&session))
    }
    fn schema_validates(&self, view: &ConversationView) -> Result<(), String> {
        let projector = toolpath_codex::project::CodexProjector::new();
        let session = projector
            .project(view)
            .map_err(|e| format!("project: {}", e))?;
        let mut lines: Vec<String> = Vec::new();
        for line in &session.lines {
            lines.push(serde_json::to_string(line).map_err(|e| format!("line: {}", e))?);
        }
        let tmp = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .map_err(|e| format!("tempfile: {}", e))?;
        std::fs::write(tmp.path(), lines.join("\n")).map_err(|e| format!("write: {}", e))?;
        toolpath_codex::RolloutReader::read_session(tmp.path())
            .map_err(|e| format!("re-read: {}", e))?;
        Ok(())
    }
}

struct CopilotHarness;
impl Harness for CopilotHarness {
    fn name(&self) -> &'static str {
        "copilot"
    }
    fn roundtrip(&self, view: &ConversationView) -> ConversationView {
        let projector = toolpath_copilot::CopilotProjector::new();
        let session = projector.project(view).expect("copilot project");
        toolpath_copilot::to_view(&session)
    }
    fn load_fixture(&self) -> Option<ConversationView> {
        let path = fixtures_dir().join("copilot/convo.jsonl");
        if !path.exists() {
            return None;
        }
        let lines =
            toolpath_copilot::EventReader::read_lines(&path).expect("copilot fixture parse");
        let session = toolpath_copilot::Session {
            id: "fixture".to_string(),
            dir_path: path.clone(),
            lines,
            workspace: None,
        };
        Some(toolpath_copilot::to_view(&session))
    }
    fn schema_validates(&self, view: &ConversationView) -> Result<(), String> {
        let projector = toolpath_copilot::CopilotProjector::new();
        let session = projector
            .project(view)
            .map_err(|e| format!("project: {}", e))?;
        let mut lines: Vec<String> = Vec::new();
        for line in &session.lines {
            lines.push(serde_json::to_string(line).map_err(|e| format!("line: {}", e))?);
        }
        let dir = tempfile::tempdir().map_err(|e| format!("tempdir: {}", e))?;
        std::fs::write(dir.path().join("events.jsonl"), lines.join("\n"))
            .map_err(|e| format!("write: {}", e))?;
        toolpath_copilot::EventReader::read_session_dir(dir.path())
            .map_err(|e| format!("re-read: {}", e))?;
        Ok(())
    }
}

struct PiHarness;
impl Harness for PiHarness {
    fn name(&self) -> &'static str {
        "pi"
    }
    fn roundtrip(&self, view: &ConversationView) -> ConversationView {
        let projector = toolpath_pi::project::PiProjector::new();
        let session = projector.project(view).expect("pi project");
        toolpath_pi::session_to_view(&session)
    }
    fn load_fixture(&self) -> Option<ConversationView> {
        let path = fixtures_dir().join("pi/convo.jsonl");
        if !path.exists() {
            return None;
        }
        let session = toolpath_pi::reader::read_session_from_file(&path).expect("pi fixture parse");
        Some(toolpath_pi::session_to_view(&session))
    }
    fn schema_validates(&self, view: &ConversationView) -> Result<(), String> {
        let projector = toolpath_pi::project::PiProjector::new();
        let session = projector
            .project(view)
            .map_err(|e| format!("project: {}", e))?;
        let mut lines: Vec<String> = Vec::new();
        for entry in &session.entries {
            lines.push(serde_json::to_string(entry).map_err(|e| format!("entry: {}", e))?);
        }
        let tmp = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .map_err(|e| format!("tempfile: {}", e))?;
        std::fs::write(tmp.path(), lines.join("\n")).map_err(|e| format!("write: {}", e))?;
        toolpath_pi::reader::read_session_from_file(tmp.path())
            .map_err(|e| format!("re-read: {}", e))?;
        Ok(())
    }
}

struct GeminiHarness;
impl Harness for GeminiHarness {
    fn name(&self) -> &'static str {
        "gemini"
    }
    fn roundtrip(&self, view: &ConversationView) -> ConversationView {
        let projector = toolpath_gemini::project::GeminiProjector::default();
        let convo = projector.project(view).expect("gemini project");
        toolpath_gemini::provider::to_view(&convo)
    }
    fn load_fixture(&self) -> Option<ConversationView> {
        let path = fixtures_dir().join("gemini/convo.jsonl");
        if !path.exists() {
            return None;
        }
        // Recent gemini versions write JSONL (line 1 = session header,
        // line 2+ = individual messages); the older `read_chat_file`
        // expects a single JSON object. Build a `ChatFile` by hand from
        // the line stream.
        let content = std::fs::read_to_string(&path).expect("gemini fixture read");
        let mut lines = content.lines().filter(|l| !l.trim().is_empty());
        let header = lines.next().expect("gemini header line");
        let mut chat_file: toolpath_gemini::types::ChatFile =
            serde_json::from_str(header).expect("gemini header parse");
        for line in lines {
            // Subsequent JSONL lines are either full messages (have `type`)
            // or `$set`-style header mutations (e.g. `lastUpdated` bumps);
            // skip the latter.
            let v: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("type").is_none() {
                continue;
            }
            // Gemini rewrites a message by appending it again under the same
            // id; the last copy is the message.
            if let Ok(msg) = serde_json::from_value::<toolpath_gemini::types::GeminiMessage>(v) {
                match chat_file.messages.iter().position(|m| m.id == msg.id) {
                    Some(i) => chat_file.messages[i] = msg,
                    None => chat_file.messages.push(msg),
                }
            }
        }
        let session_uuid = chat_file.session_id.clone();
        let convo = toolpath_gemini::types::Conversation::new(session_uuid, chat_file);
        Some(toolpath_gemini::provider::to_view(&convo))
    }
    fn schema_validates(&self, view: &ConversationView) -> Result<(), String> {
        let projector = toolpath_gemini::project::GeminiProjector::default();
        let convo = projector
            .project(view)
            .map_err(|e| format!("project: {}", e))?;
        let json =
            serde_json::to_string_pretty(&convo.main).map_err(|e| format!("serialize: {}", e))?;
        let tmp = tempfile::Builder::new()
            .suffix(".json")
            .tempfile()
            .map_err(|e| format!("tempfile: {}", e))?;
        std::fs::write(tmp.path(), &json).map_err(|e| format!("write: {}", e))?;
        toolpath_gemini::ConversationReader::read_chat_file(tmp.path())
            .map_err(|e| format!("re-read: {}", e))?;
        Ok(())
    }
}

struct OpencodeHarness;
impl Harness for OpencodeHarness {
    fn name(&self) -> &'static str {
        "opencode"
    }
    fn roundtrip(&self, view: &ConversationView) -> ConversationView {
        let projector = toolpath_opencode::project::OpencodeProjector::new();
        let session = projector.project(view).expect("opencode project");
        toolpath_opencode::to_view(&session)
    }
    fn load_fixture(&self) -> Option<ConversationView> {
        let path = fixtures_dir().join("opencode/convo.json");
        if !path.exists() {
            return None;
        }
        let json = std::fs::read_to_string(&path).expect("opencode fixture read");
        let session = parse_opencode_export(&json);
        Some(toolpath_opencode::to_view(&session))
    }
    fn schema_validates(&self, view: &ConversationView) -> Result<(), String> {
        // Opencode's wire format is SQLite + JSON-in-TEXT columns;
        // a full wire round-trip would need to spin up a temporary
        // database with the schema and run the writer. The
        // toolpath-opencode crate's `tests/projection_roundtrip.rs`
        // does exactly this against `BASIC_SQL`. Here we cover a
        // narrower contract: the Session struct is symmetrically
        // (de)serializable as JSON, which is the actual format
        // `path export opencode --output` produces.
        let projector = toolpath_opencode::project::OpencodeProjector::new();
        let session = projector
            .project(view)
            .map_err(|e| format!("project: {}", e))?;
        let json = serde_json::to_string(&session).map_err(|e| format!("serialize: {}", e))?;
        let _back: toolpath_opencode::Session =
            serde_json::from_str(&json).map_err(|e| format!("re-parse: {}", e))?;
        Ok(())
    }
}

struct CursorHarness;
impl Harness for CursorHarness {
    fn name(&self) -> &'static str {
        "cursor"
    }
    fn roundtrip(&self, view: &ConversationView) -> ConversationView {
        let projector = toolpath_cursor::project::CursorProjector::new();
        let session = projector.project(view).expect("cursor project");
        toolpath_cursor::session_to_view(&session)
    }
    fn load_fixture(&self) -> Option<ConversationView> {
        let path = fixtures_dir().join("cursor/convo.json");
        if !path.exists() {
            return None;
        }
        let json = std::fs::read_to_string(&path).expect("cursor fixture read");
        let session: toolpath_cursor::CursorSession =
            serde_json::from_str(&json).expect("cursor fixture parse");
        Some(toolpath_cursor::session_to_view(&session))
    }
    fn schema_validates(&self, view: &ConversationView) -> Result<(), String> {
        // Cursor's wire format is SQLite (`state.vscdb` cursorDiskKV
        // rows + composer.composerHeaders) — a full wire round-trip
        // would mean spinning up a temporary DB with the schema and
        // running per-row writes through it. The toolpath-cursor
        // crate's `tests/projection_roundtrip.rs` already exercises
        // exactly that against a `BASIC_FIXTURE`. Here we cover the
        // narrower contract: the projected `CursorSession` is
        // symmetrically (de)serializable as JSON, which is the
        // format the test fixture uses and what `path export cursor`
        // would emit.
        let projector = toolpath_cursor::project::CursorProjector::new();
        let session = projector
            .project(view)
            .map_err(|e| format!("project: {}", e))?;
        let json = serde_json::to_string(&session).map_err(|e| format!("serialize: {}", e))?;
        let _back: toolpath_cursor::CursorSession =
            serde_json::from_str(&json).map_err(|e| format!("re-parse: {}", e))?;
        Ok(())
    }
}

/// Convert opencode's `export` JSON wrapper format into the
/// `Session` struct shape that `to_view` expects. The wrapper uses
/// camelCase + nested `info` blocks; the Session uses snake_case +
/// flat row columns.
fn parse_opencode_export(json: &str) -> toolpath_opencode::Session {
    use toolpath_opencode::types::{Message, MessageData, Part, PartData};

    let v: Value = serde_json::from_str(json).expect("opencode wrapper parse");
    let info = &v["info"];
    let msgs_in = v["messages"].as_array().cloned().unwrap_or_default();

    let str_or = |key: &str, fallback: &str| -> String {
        info.get(key)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_string()
    };
    let i64_at = |path: &[&str]| -> Option<i64> {
        let mut cur = info;
        for k in path {
            cur = cur.get(*k)?;
        }
        cur.as_i64()
    };

    let mut messages: Vec<Message> = Vec::with_capacity(msgs_in.len());
    for m in msgs_in {
        let mi = m.get("info").cloned().unwrap_or(Value::Null);
        let mi_obj = mi.as_object().cloned().unwrap_or_default();
        let id = mi_obj
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let session_id = mi_obj
            .get("sessionID")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let time_created = mi_obj
            .get("time")
            .and_then(|t| t.get("created"))
            .and_then(Value::as_i64)
            .unwrap_or(0);

        // The MessageData payload is everything in `info` minus the row
        // metadata that lives in dedicated columns.
        let mut data_obj = mi_obj.clone();
        data_obj.remove("id");
        data_obj.remove("sessionID");
        let data: MessageData =
            serde_json::from_value(Value::Object(data_obj)).unwrap_or(MessageData::Other);

        // Parts: each part already has `type`-tagged PartData payload
        // alongside its row metadata.
        let mut parts: Vec<Part> = Vec::new();
        if let Some(parts_in) = m.get("parts").and_then(Value::as_array) {
            for p in parts_in {
                let p_obj = p.as_object().cloned().unwrap_or_default();
                let pid = p_obj
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let pmsg = p_obj
                    .get("messageID")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string();
                let psess = p_obj
                    .get("sessionID")
                    .and_then(Value::as_str)
                    .unwrap_or(&session_id)
                    .to_string();
                let mut data_obj = p_obj.clone();
                data_obj.remove("id");
                data_obj.remove("messageID");
                data_obj.remove("sessionID");
                let part_data: PartData =
                    serde_json::from_value(Value::Object(data_obj)).unwrap_or(PartData::Unknown);
                parts.push(Part {
                    id: pid,
                    message_id: pmsg,
                    session_id: psess,
                    time_created,
                    time_updated: time_created,
                    data: part_data,
                });
            }
        }

        messages.push(Message {
            id,
            session_id,
            time_created,
            time_updated: time_created,
            data,
            parts,
        });
    }

    toolpath_opencode::Session {
        id: str_or("id", ""),
        project_id: str_or("projectID", ""),
        workspace_id: info
            .get("workspaceID")
            .and_then(Value::as_str)
            .map(str::to_string),
        parent_id: info
            .get("parentID")
            .and_then(Value::as_str)
            .map(str::to_string),
        slug: str_or("slug", ""),
        directory: PathBuf::from(str_or("directory", "/")),
        title: str_or("title", ""),
        version: str_or("version", "0.0.0"),
        share_url: info
            .get("shareURL")
            .and_then(Value::as_str)
            .map(str::to_string),
        summary_additions: i64_at(&["summary", "additions"]),
        summary_deletions: i64_at(&["summary", "deletions"]),
        summary_files: i64_at(&["summary", "files"]),
        time_created: i64_at(&["time", "created"]).unwrap_or(0),
        time_updated: i64_at(&["time", "updated"])
            .or_else(|| i64_at(&["time", "created"]))
            .unwrap_or(0),
        time_compacting: i64_at(&["time", "compacting"]),
        time_archived: i64_at(&["time", "archived"]),
        messages,
    }
}

/// The path derived from `view`, as a document `path p validate` reads.
fn derived_document(view: &ConversationView) -> Value {
    let graph = Graph::from_path(derive_path(view, &DeriveConfig::default()));
    serde_json::from_str(&graph.to_json().expect("serialize Graph")).expect("parse Graph JSON")
}

fn ir_roundtrip(view: &ConversationView) -> ConversationView {
    let path = derive_path(view, &DeriveConfig::default());
    let graph = Graph::from_path(path);
    let json = graph.to_json().expect("serialize Graph");
    let back = Graph::from_json(&json).expect("parse Graph");
    let path = back.into_single_path().expect("single path");
    extract_conversation(&path)
}

// ── Invariants ───────────────────────────────────────────────────────
//
// Each invariant pushes a description of every distinct divergence into
// `failures` rather than panicking. A single cell can surface several
// independent bugs in one test run instead of forcing serial debugging.

mod invariants {
    use super::*;

    /// User-text-only system envelopes (caveats, environment_context, …)
    /// don't survive cross-harness translation by design.
    fn is_system_envelope(turn: &Turn) -> bool {
        if !matches!(turn.role, Role::User) {
            return false;
        }
        let t = turn.text.trim_start();
        t.starts_with('<') && t.contains('>')
    }

    fn meaningful_turns(view: &ConversationView) -> Vec<&Turn> {
        view.turns
            .iter()
            .filter(|t| !is_system_envelope(t))
            .collect()
    }

    pub fn turn_count_and_role_sequence(
        original: &ConversationView,
        final_: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let o = meaningful_turns(original);
        let f = meaningful_turns(final_);
        if o.len() != f.len() {
            failures.push(format!(
                "turn count diverged: first={} second={}",
                o.len(),
                f.len()
            ));
            // Don't try role pairing when counts differ — would just add noise.
            return;
        }
        for (i, (a, b)) in o.iter().zip(f.iter()).enumerate() {
            if a.role != b.role {
                failures.push(format!(
                    "role at turn {} diverged: first={:?} second={:?}",
                    i, a.role, b.role
                ));
            }
        }
    }

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    pub fn turn_text(
        original: &ConversationView,
        final_: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let o = meaningful_turns(original);
        let f = meaningful_turns(final_);
        for (i, (a, b)) in o.iter().zip(f.iter()).enumerate() {
            if norm(&a.text) != norm(&b.text) {
                failures.push(format!(
                    "text at turn {} diverged\n      first:  {:?}\n      second: {:?}",
                    i, a.text, b.text
                ));
            }
        }
    }

    /// Snake-case all object keys recursively so `filePath` and `file_path`
    /// compare equal after a cross-harness leg.
    fn canonicalize_keys(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut out = serde_json::Map::new();
                for (k, vv) in m {
                    out.insert(camel_to_snake(k), canonicalize_keys(vv));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(canonicalize_keys).collect()),
            _ => v.clone(),
        }
    }

    fn camel_to_snake(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 4);
        for (i, ch) in s.chars().enumerate() {
            if ch.is_ascii_uppercase() && i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        }
        out
    }

    pub fn tool_calls(
        original: &ConversationView,
        final_: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let o = meaningful_turns(original);
        let f = meaningful_turns(final_);
        for (i, (a, b)) in o.iter().zip(f.iter()).enumerate() {
            if !matches!(a.role, Role::Assistant) {
                continue;
            }
            let a_ids: BTreeSet<&str> = a.tool_uses.iter().map(|t| t.id.as_str()).collect();
            let b_ids: BTreeSet<&str> = b.tool_uses.iter().map(|t| t.id.as_str()).collect();
            if a_ids != b_ids {
                failures.push(format!(
                    "tool call_id set at turn {} diverged\n      first:  {:?}\n      second: {:?}",
                    i, a_ids, b_ids
                ));
                continue;
            }
            for tu_a in &a.tool_uses {
                let tu_b = match b.tool_uses.iter().find(|t| t.id == tu_a.id) {
                    Some(t) => t,
                    None => continue, // covered by the set check above
                };
                let in_a = canonicalize_keys(&tu_a.input);
                let in_b = canonicalize_keys(&tu_b.input);
                if in_a != in_b {
                    failures.push(format!(
                        "tool {} input shape diverged\n      first:  {}\n      second: {}",
                        tu_a.id, in_a, in_b
                    ));
                }
                match (&tu_a.result, &tu_b.result) {
                    (Some(ra), Some(rb)) => {
                        if ra.content != rb.content {
                            failures.push(format!(
                                "tool {} output diverged\n      first:  {:?}\n      second: {:?}",
                                tu_a.id, ra.content, rb.content
                            ));
                        }
                        if ra.is_error != rb.is_error {
                            failures.push(format!(
                                "tool {} error flag diverged: first={} second={}",
                                tu_a.id, ra.is_error, rb.is_error
                            ));
                        }
                    }
                    (None, None) => {}
                    (l, r) => failures.push(format!(
                        "tool {} result presence diverged: first={} second={}",
                        tu_a.id,
                        l.is_some(),
                        r.is_some()
                    )),
                }
            }
        }
    }

    pub fn token_usage(
        original: &ConversationView,
        final_: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let o = meaningful_turns(original);
        let f = meaningful_turns(final_);
        for (i, (a, b)) in o.iter().zip(f.iter()).enumerate() {
            if a.token_usage != b.token_usage {
                failures.push(format!(
                    "token_usage at turn {} diverged\n      first:  {:?}\n      second: {:?}",
                    i, a.token_usage, b.token_usage
                ));
            }
        }
        if original.total_usage != final_.total_usage {
            failures.push(format!(
                "total_usage diverged\n      first:  {:?}\n      second: {:?}",
                original.total_usage, final_.total_usage
            ));
        }
    }

    /// Survival check: tokens present in the pre-B IR must survive the B
    /// leg. Catches projectors that silently drop tokens (idempotence
    /// alone won't — both passes drop identically). Pair by assistant
    /// subsequence: some forward paths (pi) insert `Role::Other("tool")`
    /// turns for tool results, which would misalign a positional pairing.
    pub fn token_usage_survives(
        before_target: &ConversationView,
        after_target: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        // Harnesses legitimately fold or split turns in translation
        // (e.g. thinking-only claude turns merge into codex `reasoning`
        // lines), so assistant indexes don't align across harnesses.
        // The accounting invariant is order-preserving instead: the
        // sequence of usage values on assistant turns survives, compared
        // on input/output — the fields every wire carries (codex has no
        // cache_write analog, cursor carries no cache counters at all).
        let usage_seq = |v: &ConversationView| -> Vec<(Option<u32>, Option<u32>)> {
            v.turns
                .iter()
                .filter(|t| matches!(t.role, Role::Assistant))
                .filter_map(|t| t.token_usage.as_ref())
                .map(|u| (u.input_tokens, u.output_tokens))
                .collect()
        };
        let pre = usage_seq(before_target);
        let post = usage_seq(after_target);
        if pre != post {
            failures.push(format!(
                "assistant usage sequence diverged ({} -> {} entries)\n      first:  {:?}\n      second: {:?}",
                pre.len(),
                post.len(),
                pre,
                post
            ));
        }
        if before_target.total_usage.is_some() && after_target.total_usage.is_none() {
            failures.push(format!(
                "total_usage dropped (had {:?})",
                before_target.total_usage
            ));
        }
    }

    /// Strict equality on `Turn.thinking` between the two views.
    /// Cross-harness loss of provider-specific reasoning structure
    /// (Anthropic signatures, codex encrypted blobs) is allowed at
    /// the *content* level — `thinking` is a plain `Option<String>`
    /// in the IR — but the presence and text must round-trip
    /// idempotently through B's projector + forward.
    pub fn thinking(
        original: &ConversationView,
        final_: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let o = meaningful_turns(original);
        let f = meaningful_turns(final_);
        for (i, (a, b)) in o.iter().zip(f.iter()).enumerate() {
            if a.thinking != b.thinking {
                failures.push(format!(
                    "thinking at turn {} diverged\n      first:  {:?}\n      second: {:?}",
                    i, a.thinking, b.thinking
                ));
            }
        }
    }

    pub fn thinking_survives(
        before_target: &ConversationView,
        after_target: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let pre: Vec<&Turn> = before_target
            .turns
            .iter()
            .filter(|t| matches!(t.role, Role::Assistant))
            .collect();
        let post: Vec<&Turn> = after_target
            .turns
            .iter()
            .filter(|t| matches!(t.role, Role::Assistant))
            .collect();
        for (i, (a, b)) in pre.iter().zip(post.iter()).enumerate() {
            if a.thinking.as_ref().is_some_and(|s| !s.is_empty())
                && b.thinking.as_ref().is_none_or(|s| s.is_empty())
            {
                failures.push(format!(
                    "thinking at assistant #{} dropped (had {} chars)",
                    i,
                    a.thinking.as_ref().map(String::len).unwrap_or(0)
                ));
            }
        }
    }

    pub fn model_field(
        original: &ConversationView,
        final_: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let o = meaningful_turns(original);
        let f = meaningful_turns(final_);
        for (i, (a, b)) in o.iter().zip(f.iter()).enumerate() {
            if a.model != b.model {
                failures.push(format!(
                    "model at turn {} diverged: first={:?} second={:?}",
                    i, a.model, b.model
                ));
            }
        }
    }

    pub fn stop_reason(
        original: &ConversationView,
        final_: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let o = meaningful_turns(original);
        let f = meaningful_turns(final_);
        for (i, (a, b)) in o.iter().zip(f.iter()).enumerate() {
            if !matches!(a.role, Role::Assistant) {
                continue;
            }
            if a.stop_reason != b.stop_reason {
                failures.push(format!(
                    "stop_reason at turn {} diverged: first={:?} second={:?}",
                    i, a.stop_reason, b.stop_reason
                ));
            }
        }
    }

    /// Edge-set equality on the parent_id graph. Same {(id, parent_id)}
    /// set across the two views — ordering is allowed to differ but no
    /// edge may be added or dropped.
    pub fn parent_id_graph(
        original: &ConversationView,
        final_: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let edges = |v: &ConversationView| -> BTreeSet<(String, Option<String>)> {
            v.turns
                .iter()
                .map(|t| (t.id.clone(), t.parent_id.clone()))
                .collect()
        };
        let o = edges(original);
        let f = edges(final_);
        if o != f {
            let only_o: Vec<_> = o.difference(&f).collect();
            let only_f: Vec<_> = f.difference(&o).collect();
            failures.push(format!(
                "parent_id graph diverged\n      only in first:  {:?}\n      only in second: {:?}",
                only_o, only_f
            ));
        }
    }

    pub fn environment(
        original: &ConversationView,
        final_: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let o = meaningful_turns(original);
        let f = meaningful_turns(final_);
        for (i, (a, b)) in o.iter().zip(f.iter()).enumerate() {
            // Compare working_dir; vcs fields vary per provider and are
            // legitimately not always populated.
            let aw = a.environment.as_ref().and_then(|e| e.working_dir.as_ref());
            let bw = b.environment.as_ref().and_then(|e| e.working_dir.as_ref());
            if aw != bw {
                failures.push(format!(
                    "environment.working_dir at turn {} diverged: first={:?} second={:?}",
                    i, aw, bw
                ));
            }
        }
    }

    /// Per-delegation content equality (idempotence). Beyond total count
    /// (the previous bar), this asserts each delegation's `agent_id`,
    /// child-turn count, and prompt text survive byte-for-byte across a
    /// no-op B→B pass. Today's fixtures carry zero delegations, so this
    /// passes vacuously; the assertion fires the moment a fixture
    /// (refreshed via `scripts/capture-elicit-fixtures.sh` after the
    /// elicit prompt was extended to dispatch sub-agents) carries one.
    pub fn delegations(
        original: &ConversationView,
        final_: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let count =
            |v: &ConversationView| -> usize { v.turns.iter().map(|t| t.delegations.len()).sum() };
        let o = count(original);
        let f = count(final_);
        if o != f {
            failures.push(format!(
                "delegation count diverged: first={} second={}",
                o, f
            ));
            return;
        }

        for (i, (a, b)) in original.turns.iter().zip(final_.turns.iter()).enumerate() {
            if a.delegations.len() != b.delegations.len() {
                failures.push(format!(
                    "turn {} delegation count diverged: first={} second={}",
                    i,
                    a.delegations.len(),
                    b.delegations.len()
                ));
                continue;
            }
            let a_ids: BTreeSet<&str> = a.delegations.iter().map(|d| d.agent_id.as_str()).collect();
            let b_ids: BTreeSet<&str> = b.delegations.iter().map(|d| d.agent_id.as_str()).collect();
            if a_ids != b_ids {
                failures.push(format!(
                    "turn {} delegation agent_id set diverged\n      first:  {:?}\n      second: {:?}",
                    i, a_ids, b_ids
                ));
                continue;
            }
            for da in &a.delegations {
                let db = match b.delegations.iter().find(|d| d.agent_id == da.agent_id) {
                    Some(d) => d,
                    None => continue,
                };
                if norm(&da.prompt) != norm(&db.prompt) {
                    failures.push(format!(
                        "delegation {} prompt diverged at turn {}\n      first:  {:?}\n      second: {:?}",
                        da.agent_id, i, da.prompt, db.prompt
                    ));
                }
                if da.turns.len() != db.turns.len() {
                    failures.push(format!(
                        "delegation {} child-turn count diverged at turn {}: first={} second={}",
                        da.agent_id,
                        i,
                        da.turns.len(),
                        db.turns.len()
                    ));
                }
            }
        }
    }

    /// Cross-leg survival: every source delegation's `agent_id` must
    /// remain findable in the target IR — either as a delegation
    /// (preferred — preserves the structural "sub-agent" semantics) or
    /// as a regular `tool_use` call_id (acceptable — the user-visible
    /// tool call is still present even if the harness can't natively
    /// model delegation).
    ///
    /// The looser bar here matches the "good UX" goal: when you open
    /// a Claude session in a harness without first-class sub-agents
    /// (Codex, opencode), you still see the dispatch tool call and
    /// its result; you just lose the metadata that says "this was a
    /// delegation." A flat-out drop of the call_id would be the
    /// regression — the parent tool call disappearing is what would
    /// confuse a reader.
    pub fn delegations_survive(
        before_target: &ConversationView,
        after_target: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let agent_ids = |v: &ConversationView| -> BTreeSet<String> {
            v.turns
                .iter()
                .flat_map(|t| t.delegations.iter().map(|d| d.agent_id.clone()))
                .collect()
        };
        let tool_use_ids = |v: &ConversationView| -> BTreeSet<String> {
            v.turns
                .iter()
                .flat_map(|t| t.tool_uses.iter().map(|tu| tu.id.clone()))
                .collect()
        };

        let pre = agent_ids(before_target);
        let post_delegations = agent_ids(after_target);
        let post_tool_uses = tool_use_ids(after_target);

        let truly_dropped: Vec<&String> = pre
            .difference(&post_delegations)
            .filter(|id| !post_tool_uses.contains(id.as_str()))
            .collect();
        if !truly_dropped.is_empty() {
            failures.push(format!(
                "delegations dropped on translation leg (not preserved as delegation or tool_use): {:?}",
                truly_dropped
            ));
        }
    }

    /// The derived path satisfies its kind: the schema plus the accounting
    /// rules `path p validate` checks.
    pub fn kind_conforms(view: &ConversationView, failures: &mut Vec<String>) {
        if let Err(e) = path_cli::schema::validate(&derived_document(view)) {
            failures.push(format!("derived path breaks its kind: {e}"));
        }
    }

    pub fn files_changed(
        original: &ConversationView,
        final_: &ConversationView,
        failures: &mut Vec<String>,
    ) {
        let set_of = |v: &ConversationView| -> BTreeSet<String> {
            v.files_changed.iter().cloned().collect()
        };
        let o = set_of(original);
        let f = set_of(final_);
        if o != f {
            let only_o: Vec<_> = o.difference(&f).collect();
            let only_f: Vec<_> = f.difference(&o).collect();
            failures.push(format!(
                "files_changed set diverged\n      only in first:  {:?}\n      only in second: {:?}",
                only_o, only_f
            ));
        }
    }
}

// ── Matrix runner ────────────────────────────────────────────────────

fn run_cell(
    source: &dyn Harness,
    target: &dyn Harness,
    canonical: &ConversationView,
) -> Vec<String> {
    // Cell shape: A → IR → B → IR → B. After one A→B translation, B's
    // own projector + forward should be a fixed point on the resulting
    // IR. If it isn't, B is synthesizing different output the second
    // time around — i.e., B's representation depends on IR provenance
    // rather than IR content.
    let view_after_source = ir_roundtrip(&source.roundtrip(canonical));
    let view_first = ir_roundtrip(&target.roundtrip(&view_after_source));
    let view_second = ir_roundtrip(&target.roundtrip(&view_first));

    let mut failures: Vec<String> = Vec::new();
    invariants::turn_count_and_role_sequence(&view_first, &view_second, &mut failures);
    invariants::turn_text(&view_first, &view_second, &mut failures);
    invariants::tool_calls(&view_first, &view_second, &mut failures);
    invariants::token_usage(&view_first, &view_second, &mut failures);
    invariants::token_usage_survives(&view_after_source, &view_first, &mut failures);
    invariants::thinking(&view_first, &view_second, &mut failures);
    invariants::thinking_survives(&view_after_source, &view_first, &mut failures);
    invariants::model_field(&view_first, &view_second, &mut failures);
    invariants::stop_reason(&view_first, &view_second, &mut failures);
    invariants::parent_id_graph(&view_first, &view_second, &mut failures);
    invariants::environment(&view_first, &view_second, &mut failures);
    invariants::delegations(&view_first, &view_second, &mut failures);
    invariants::delegations_survive(&view_after_source, &view_first, &mut failures);
    invariants::files_changed(&view_first, &view_second, &mut failures);
    invariants::kind_conforms(&view_first, &mut failures);
    failures
}

fn run_matrix(label: &str, sources: &[(String, ConversationView)]) {
    let harnesses: Vec<Box<dyn Harness>> = vec![
        Box::new(ClaudeHarness),
        Box::new(CodexHarness),
        Box::new(CopilotHarness),
        Box::new(PiHarness),
        Box::new(GeminiHarness),
        Box::new(OpencodeHarness),
        Box::new(CursorHarness),
    ];
    let by_name: BTreeMap<&str, &dyn Harness> =
        harnesses.iter().map(|h| (h.name(), h.as_ref())).collect();

    let mut failed_cells: Vec<(String, Vec<String>)> = Vec::new();
    eprintln!("══ {} ══", label);
    for (source_name, canonical) in sources {
        let source = match by_name.get(source_name.as_str()) {
            Some(h) => *h,
            None => continue,
        };
        for target in &harnesses {
            let cell = format!("{} → {} idempotent", source.name(), target.name());
            let failures = run_cell(source, target.as_ref(), canonical);
            if failures.is_empty() {
                eprintln!("✓ {}", cell);
            } else {
                eprintln!(
                    "✗ {} ({} failure{})",
                    cell,
                    failures.len(),
                    if failures.len() == 1 { "" } else { "s" }
                );
                for f in &failures {
                    eprintln!("    {}", f);
                }
                failed_cells.push((cell, failures));
            }
        }
    }

    if !failed_cells.is_empty() {
        let total: usize = failed_cells.iter().map(|(_, fs)| fs.len()).sum();
        panic!(
            "{} had {} failure(s) across {} cell(s); see logs above",
            label,
            total,
            failed_cells.len()
        );
    }
}

fn all_harnesses() -> Vec<Box<dyn Harness>> {
    vec![
        Box::new(ClaudeHarness),
        Box::new(CodexHarness),
        Box::new(CopilotHarness),
        Box::new(PiHarness),
        Box::new(GeminiHarness),
        Box::new(OpencodeHarness),
        Box::new(CursorHarness),
    ]
}

#[test]
fn matrix_translation() {
    // Each source uses its own captured real-world session as input.
    // Cell shape: A → IR → B → IR → B. After one A→B translation, B's
    // projector + forward must be a fixed point on the resulting IR.
    let harnesses = all_harnesses();
    let mut sources: Vec<(String, ConversationView)> = Vec::new();
    for h in &harnesses {
        let view = h.load_fixture().unwrap_or_else(|| {
            panic!(
                "{} fixture missing — run scripts/capture-elicit-fixtures.sh {}",
                h.name(),
                h.name()
            )
        });
        eprintln!("loaded {} fixture: {} turns", h.name(), view.turns.len());
        sources.push((h.name().to_string(), view));
    }
    run_matrix("matrix (real fixtures)", &sources);
}

#[test]
fn matrix_schema_validation() {
    // For each harness, project its real fixture, serialize the native
    // output to its on-disk wire format, and re-parse it through the
    // harness's own reader. Catches projector output that's "valid IR"
    // but produces native bytes the reader rejects — the class of bug
    // that surfaces only at `path import …` time or live in the
    // harness's CLI.
    let harnesses = all_harnesses();

    let mut failures: Vec<String> = Vec::new();
    eprintln!("══ schema validation ══");
    for h in &harnesses {
        let view = h.load_fixture().unwrap_or_else(|| {
            panic!(
                "{} fixture missing — run scripts/capture-elicit-fixtures.sh {}",
                h.name(),
                h.name()
            )
        });
        match h.schema_validates(&view) {
            Ok(()) => eprintln!("✓ {}", h.name()),
            Err(e) => {
                eprintln!("✗ {}: {}", h.name(), e);
                failures.push(format!("{}: {}", h.name(), e));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "schema validation had {} failure(s); see logs above",
            failures.len()
        );
    }
}

#[test]
fn matrix_kind_validation() {
    // Each harness's real fixture derives to a path that passes
    // `path p validate`: base schema, kind schema, and the kind's
    // accounting rules.
    let mut failures: Vec<String> = Vec::new();
    for h in &all_harnesses() {
        let view = h.load_fixture().expect("fixture on disk");
        let mut cell = Vec::new();
        invariants::kind_conforms(&view, &mut cell);
        match cell.as_slice() {
            [] => eprintln!("✓ {}", h.name()),
            fs => {
                for f in fs {
                    eprintln!("✗ {}: {}", h.name(), f);
                    failures.push(format!("{}: {}", h.name(), f));
                }
            }
        }
    }
    assert!(failures.is_empty(), "kind validation failed: {failures:#?}");
}

// ── Additive token classes ───────────────────────────────────────────
//
// The kind's classes are additive: `input_tokens` excludes cached prompt
// tokens, so `input + cache_read + cache_write` summed over a derived path
// is the session's prompt. Each oracle reads that prompt total straight
// from the fixture's wire JSON, in the source's own units, independently
// of the provider crate. Only fixtures that record cached input are here:
// pi and cursor fixtures carry zero cache counters, so they cannot tell
// additive classes from inclusive ones.

fn fixture_lines(rel: &str) -> Vec<Value> {
    std::fs::read_to_string(fixtures_dir().join(rel))
        .expect("fixture read")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("fixture line parses"))
        .collect()
}

fn u64_at(v: &Value, pointer: &str) -> u64 {
    v.pointer(pointer).and_then(Value::as_u64).unwrap_or(0)
}

/// A session's prompt and its cached parts, in tokens.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
struct Prompt {
    total: u64,
    cache_read: u64,
    cache_write: u64,
}

impl std::ops::AddAssign for Prompt {
    fn add_assign(&mut self, o: Prompt) {
        self.total += o.total;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
    }
}

/// Claude's `input_tokens` already excludes cache. One message spans several
/// lines, each repeating its `usage` (a growing snapshot in older Claude Code
/// versions), so each field takes its largest value across the lines.
fn claude_prompt() -> Prompt {
    let mut per_message: BTreeMap<String, [u64; 3]> = BTreeMap::new();
    for line in fixture_lines("claude/convo.jsonl") {
        let (Some(id), Some(_)) = (
            line.pointer("/message/id").and_then(Value::as_str),
            line.pointer("/message/usage"),
        ) else {
            continue;
        };
        let max = per_message.entry(id.to_string()).or_default();
        for (slot, field) in [
            "input_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ]
        .iter()
        .enumerate()
        {
            max[slot] = max[slot].max(u64_at(&line, &format!("/message/usage/{field}")));
        }
    }
    let mut p = Prompt::default();
    for [input, read, write] in per_message.into_values() {
        p += Prompt {
            total: input + read + write,
            cache_read: read,
            cache_write: write,
        };
    }
    p
}

/// Codex's `input_tokens` includes `cached_input_tokens` and, from Codex
/// 0.145.0, `cache_write_input_tokens`; the last cumulative
/// `total_token_usage` is the session's.
fn codex_prompt() -> Prompt {
    let lines = fixture_lines("codex/convo.jsonl");
    let total = lines
        .iter()
        .filter_map(|l| l.pointer("/payload/info/total_token_usage"))
        .next_back()
        .expect("codex fixture has a token_count");
    Prompt {
        total: u64_at(total, "/input_tokens"),
        cache_read: u64_at(total, "/cached_input_tokens"),
        cache_write: u64_at(total, "/cache_write_input_tokens"),
    }
}

/// Gemini's `input` (`promptTokenCount`) includes `cached`; `tool`
/// (`toolUsePromptTokenCount`) is prompt outside it. A message rewritten
/// under the same id counts once.
fn gemini_prompt() -> Prompt {
    let mut per_message: BTreeMap<String, Prompt> = BTreeMap::new();
    for line in fixture_lines("gemini/convo.jsonl") {
        if let (Some(id), Some(tokens)) =
            (line.get("id").and_then(Value::as_str), line.get("tokens"))
        {
            per_message.insert(
                id.to_string(),
                Prompt {
                    total: u64_at(tokens, "/input") + u64_at(tokens, "/tool"),
                    cache_read: u64_at(tokens, "/cached"),
                    cache_write: 0,
                },
            );
        }
    }
    let mut p = Prompt::default();
    for m in per_message.into_values() {
        p += m;
    }
    p
}

/// Copilot's `session.shutdown` reports each model's `inputTokens`, which
/// includes its cache reads and writes.
fn copilot_prompt() -> Prompt {
    let mut p = Prompt::default();
    for line in fixture_lines("copilot/convo.jsonl") {
        let Some(models) = line
            .pointer("/data/modelMetrics")
            .and_then(Value::as_object)
        else {
            continue;
        };
        for m in models.values() {
            p += Prompt {
                total: u64_at(m, "/usage/inputTokens"),
                cache_read: u64_at(m, "/usage/cacheReadTokens"),
                cache_write: u64_at(m, "/usage/cacheWriteTokens"),
            };
        }
    }
    p
}

/// opencode's `total` is `input + output + reasoning + cache.read +
/// cache.write`, so the prompt is what `total` holds beyond the output; this
/// reads opencode's own sum rather than trusting what `input` means.
fn opencode_prompt() -> Prompt {
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("opencode/convo.json")).expect("fixture read"),
    )
    .expect("opencode fixture parses");
    let mut p = Prompt::default();
    for t in doc["messages"]
        .as_array()
        .expect("opencode messages")
        .iter()
        .filter_map(|m| m.pointer("/info/tokens"))
    {
        let total = t
            .get("total")
            .and_then(Value::as_u64)
            .expect("opencode tokens.total");
        p += Prompt {
            total: total
                .saturating_sub(u64_at(t, "/output"))
                .saturating_sub(u64_at(t, "/reasoning")),
            cache_read: u64_at(t, "/cache/read"),
            cache_write: u64_at(t, "/cache/write"),
        };
    }
    p
}

fn prompt_of(u: &Value) -> Prompt {
    let (read, write) = (
        u64_at(u, "/cache_read_tokens"),
        u64_at(u, "/cache_write_tokens"),
    );
    Prompt {
        total: u64_at(u, "/input_tokens") + read + write,
        cache_read: read,
        cache_write: write,
    }
}

/// The prompt over a derived path's group totals.
fn derived_prompt(view: &ConversationView) -> Prompt {
    let doc = derived_document(view);
    let mut p = Prompt::default();
    for u in doc
        .pointer("/paths/0/steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|step| step.get("change").and_then(Value::as_object))
        .flat_map(|change| change.values())
        .filter_map(|c| c.get("structural"))
        .filter(|s| s.get("type").and_then(Value::as_str) == Some("conversation.append"))
        .filter_map(|s| s.get("token_usage"))
    {
        p += prompt_of(u);
    }
    p
}

/// The prompt over the view's session total. Copilot records input and cache
/// only on `session.shutdown`, which lands in `total_usage` and on no turn, so
/// a derived Copilot path carries output tokens alone (a documented exception
/// in the kind).
fn session_prompt(view: &ConversationView) -> Prompt {
    view.total_usage
        .as_ref()
        .map(|u| prompt_of(&serde_json::to_value(u).expect("serialize TokenUsage")))
        .unwrap_or_default()
}

type Derived = fn(&ConversationView) -> Prompt;
type Source = fn() -> Prompt;

#[test]
fn token_classes_add_up_to_the_source_prompt() {
    let oracles: [(&dyn Harness, Derived, Source); 5] = [
        (&ClaudeHarness, derived_prompt, claude_prompt),
        (&CodexHarness, derived_prompt, codex_prompt),
        (&GeminiHarness, derived_prompt, gemini_prompt),
        (&CopilotHarness, session_prompt, copilot_prompt),
        (&OpencodeHarness, derived_prompt, opencode_prompt),
    ];
    let mut failures: Vec<String> = Vec::new();
    for (h, derived, source) in oracles {
        let view = h.load_fixture().expect("fixture on disk");
        let (got, want) = (derived(&view), source());
        if got == want {
            eprintln!("✓ {}: {:?}", h.name(), want);
        } else {
            eprintln!("✗ {}: derived {:?} vs source {:?}", h.name(), got, want);
            failures.push(format!("{}: derived {got:?}, source {want:?}", h.name()));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
