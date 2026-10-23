//! The pipeline: one set in, every stage's output out, or the first stage
//! that failed.
//!
//! `read` (bytes -> the agent's conversation type, through its adapter's
//! reader), `ir` (`to_view`), `derive` (the toolpath graph `path p derive`
//! prints, thinking included), `project` (every other agent's transcript, as
//! `path p export <agent> -o` writes it), round trips (each projection read
//! back by its own adapter and derived again), `validate` (`path p validate`
//! over the document and each round trip), `same_format` (the set's own
//! format again), then the shed report and the known defects over all of
//! that. Nothing here reads a manifest, hashes a file, runs a child process
//! or resolves a store under `$HOME`: given the same adapters and the same
//! bytes it produces the same [`Stages`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use toolpath::v1::{Base, Graph, Path as Document};
use toolpath_convo::{ConversationProjector, ConversationView, extract_conversation};

use crate::canonical::{canonicalize, canonicalize_document};
use crate::contracts::{check_document, check_projection};
use crate::defects::{Evidence, known_defects};
use crate::keypaths::json_values;
use crate::losses::{compare, compare_passes};
use crate::model::{
    Agent, RoundTrip, Set, Stage, StageError, Stages, TwiceAround, Validate, Validation,
};
use crate::shed::report;
use crate::validate::validate;

/// The working directory `path p export codex|pi|copilot|gemini` records as the session's: the
/// caller's (`--project`, `path resume -C`, else the shell's), not the source's. Not a defect: codex's
/// resume picker lists only sessions whose `cwd` is where it runs, pi and gemini key the project on it
/// (gemini's `projectHash`, `directories`). The pipeline resumes in the DOCUMENT's own working
/// directory (`resume_dir`, as `path resume -C <project>` does for a user resuming where the
/// session ran), so the written session can be read back with paths relative to it; this `/` (the
/// directory the hermetic tool ran from) is only the fallback for a document that names none.
pub const CALLER_CWD: &str = "/";

/// The `blobEncryptionKey` a `cursor-agent` projection carries in the
/// baseline. `path resume` writes 32 random bytes per chat (the CLI sends it
/// to Cursor's server; nothing local is encrypted with it); pinned here so
/// the projection is the same on every run.
pub const PINNED_BLOB_KEY: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// Run every stage over `set`. `scratch` is a directory the adapters' readers
/// may write transcripts into (they read files, not bytes) and the only
/// place the opencode adapter looks for a snapshot store; nothing in it
/// survives the call, and its path must not appear in the output, which the
/// caller checks with [`crate::leaks::leaks`].
pub fn run(set: &Set, scratch: &Path) -> Result<Stages, StageError> {
    let fail = |stage: Stage| move |e: String| StageError::new(stage, e);

    let source = set.text();
    let convo = read(set.agent, &set.input, scratch).map_err(fail(Stage::Read))?;
    let ir = convo.view(scratch);
    let document_json = convo
        .derive(&source, scratch)
        .map_err(fail(Stage::Derive))?;
    let document: Value = serde_json::from_str(&document_json)
        .map_err(|e| StageError::new(Stage::Derive, format!("document is not JSON: {e}")))?;
    let path = single_path(&document_json).map_err(fail(Stage::Derive))?;

    let mut raw = BTreeMap::new();
    for target in set.agent.targets() {
        let text = project(&path, target)
            .map_err(|e| StageError::new(Stage::Project, format!("-> {target}: {e}")))?;
        raw.insert(target, text);
    }
    let mut round_trips = BTreeMap::new();
    let mut twice_around = BTreeMap::new();
    let mut round_trip_validations = BTreeMap::new();
    for (target, text) in &raw {
        let (trip, again) = round_trip(&document, *target, text, scratch);
        twice_around.insert(*target, second_pass(again.as_ref(), *target, scratch));
        round_trips.insert(*target, trip);
        round_trip_validations.insert(
            *target,
            again.as_ref().map_or(Validation::NotDerived, validate),
        );
    }
    let validate = Validate {
        document: validate(&document),
        round_trips: round_trip_validations,
    };
    // The contracts are checked on the raw form, as the harness would read it.
    let mut contracts: Vec<String> = check_document(&document)
        .iter()
        .map(ToString::to_string)
        .collect();
    for (target, text) in &raw {
        contracts.extend(
            check_projection(*target, text)
                .iter()
                .map(ToString::to_string),
        );
    }
    let projections: BTreeMap<Agent, String> = raw
        .into_iter()
        .map(|(target, text)| (target, settle(target, text)))
        .collect();

    let same_format = project(&path, set.agent).map_err(fail(Stage::SameFormat))?;
    contracts.extend(
        check_projection(set.agent, &same_format)
            .iter()
            .map(ToString::to_string),
    );
    let compared = convo.written_shape();
    let shed = report(
        &source,
        compared.as_deref().unwrap_or(&source),
        &document_json,
        &document,
        &projections,
        &same_format,
    );
    let known_defects = known_defects(&Evidence {
        set,
        ir: &ir,
        document: &document,
        projections: &projections,
        same_format: &same_format,
    });
    Ok(Stages {
        ir,
        document,
        projections,
        round_trips,
        twice_around,
        validate,
        shed,
        known_defects,
        contracts,
    })
}

/// The form a projection is pinned in: raw for every agent whose output is
/// the same on every run; canonical for Copilot, whose projection mints a
/// session id, and for Cursor, whose composer rows carry hash maps the
/// projection writes in a different key order each run.
fn settle(target: Agent, text: String) -> String {
    match target {
        Agent::Copilot => canonicalize(&text),
        Agent::Cursor => canonicalize_document(&text),
        Agent::Claude
        | Agent::Codex
        | Agent::Pi
        | Agent::Gemini
        | Agent::Opencode
        | Agent::CursorAgent => text,
    }
}

/// A transcript read by its agent's adapter.
enum Conversation {
    Claude(toolpath_claude::Conversation),
    Codex(toolpath_codex::Session),
    Pi(toolpath_pi::PiSession),
    Copilot(toolpath_copilot::Session),
    Gemini(toolpath_gemini::Conversation),
    Opencode(toolpath_opencode::Session),
    /// Boxed: a composer carries every bubble and blob inline.
    Cursor(Box<toolpath_cursor::CursorSession>),
    /// A `cursor-agent` chat, read into its view (the store keeps no more).
    CursorAgent(Box<ConversationView>),
}

impl Conversation {
    /// The source in the shape the writer gives back, when the recorded file has another: gemini
    /// records a JSONL record stream and writes one chat document; opencode's export nests
    /// `info`/`parts` where its rows nest `data`. The same-format comparison is made on this, so a
    /// path that only moved does not count as lost. `None` when the recording is already in that shape.
    fn written_shape(&self) -> Option<String> {
        match self {
            Conversation::Gemini(c) => serde_json::to_string_pretty(&c.main).ok(),
            Conversation::Opencode(s) => serde_json::to_string_pretty(s).ok(),
            _ => None,
        }
    }

    /// Stage `ir`.
    fn view(&self, scratch: &Path) -> ConversationView {
        match self {
            Conversation::Claude(c) => toolpath_claude::provider::to_view(c),
            Conversation::Codex(s) => toolpath_codex::to_view(s),
            Conversation::Pi(s) => toolpath_pi::session_to_view(s),
            Conversation::Copilot(s) => toolpath_copilot::to_view(s),
            Conversation::Gemini(c) => toolpath_gemini::provider::to_view(c),
            Conversation::Opencode(s) => {
                toolpath_opencode::provider::to_view_with_resolver(s, &opencode_store(scratch))
            }
            Conversation::Cursor(s) => toolpath_cursor::session_to_view(s),
            Conversation::CursorAgent(v) => (**v).clone(),
        }
    }

    /// Stage `derive`: the graph `path p derive <agent>` prints, as compact
    /// JSON. Claude and Gemini include thinking blocks, as the CLI does;
    /// Claude falls back to the transcript's first `cwd` as the base when
    /// the entries carry none, Gemini to the chat file's first directory
    /// (the CLI stamps the project it found the session under).
    fn derive(&self, source: &str, scratch: &Path) -> Result<String, String> {
        let path = match self {
            Conversation::Claude(c) => {
                let cfg = toolpath_claude::derive::DeriveConfig {
                    project_path: None,
                    include_thinking: true,
                };
                let mut path = toolpath_claude::derive::derive_path(c, &cfg);
                if path.path.base.is_none()
                    && let Some(cwd) = first_cwd(source).filter(|c| c.starts_with('/'))
                {
                    path.path.base = Some(Base {
                        uri: format!("file://{cwd}"),
                        ref_str: None,
                        branch: None,
                    });
                }
                path
            }
            Conversation::Codex(s) => toolpath_codex::derive::derive_path(
                s,
                &toolpath_codex::derive::DeriveConfig { project_path: None },
            ),
            Conversation::Pi(s) => {
                toolpath_pi::derive::derive_path(s, &toolpath_pi::DeriveConfig::default())
            }
            Conversation::Copilot(s) => toolpath_copilot::derive::derive_path(
                s,
                &toolpath_copilot::derive::DeriveConfig { project_path: None },
            ),
            Conversation::Gemini(c) => toolpath_gemini::derive::derive_path(
                c,
                &toolpath_gemini::derive::DeriveConfig {
                    project_path: c
                        .main
                        .directories
                        .as_ref()
                        .and_then(|d| d.first())
                        .map(|d| d.to_string_lossy().into_owned()),
                    include_thinking: true,
                },
            ),
            Conversation::Opencode(s) => toolpath_opencode::derive::derive_path_with_resolver(
                s,
                &toolpath_opencode::derive::DeriveConfig::default(),
                &opencode_store(scratch),
            ),
            Conversation::Cursor(s) => {
                toolpath_cursor::derive_path(s, &toolpath_cursor::DeriveConfig::default())
            }
            // No `path p derive cursor-agent` yet: the shared derive, as
            // every provider's derive ends in.
            Conversation::CursorAgent(v) => {
                toolpath_convo::derive_path(v, &toolpath_convo::DeriveConfig::default())
            }
        };
        Graph::from_path(path).to_json().map_err(|e| e.to_string())
    }
}

/// The opencode store the adapter resolves snapshot diffs from, rooted in
/// `scratch` so a golden never sees this machine's `opencode-stable.db` or
/// its snapshot repository (there is none under `scratch`: no diffs).
fn opencode_store(scratch: &Path) -> toolpath_opencode::PathResolver {
    toolpath_opencode::PathResolver::new()
        .with_home(scratch)
        .with_data_dir(scratch)
}

/// Stage `read`. The adapters that read files get the bytes as a file under
/// `scratch` first, named the way each expects (Claude derives the session
/// id from the file stem). opencode and Cursor read rows of a database;
/// their `-o` form and their fixtures are those rows as JSON, read through
/// the adapters' own types. Errors never carry the scratch path.
fn read(agent: Agent, input: &[u8], scratch: &Path) -> Result<Conversation, String> {
    let dir = tempfile::tempdir_in(scratch).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(input);
    let mask = |e: String| e.replace(&dir.path().display().to_string(), "<tmp>");
    let file = |name: &str| -> Result<PathBuf, String> {
        let p = dir.path().join(name);
        std::fs::write(&p, input).map_err(|e| e.to_string())?;
        Ok(p)
    };
    match agent {
        Agent::Claude => {
            let id = first_string(&text, "sessionId")
                .ok_or("transcript names no sessionId, which the claude adapter needs")?;
            toolpath_claude::ConversationReader::read_conversation(file(&format!("{id}.jsonl"))?)
                .map(Conversation::Claude)
                .map_err(|e| mask(format!("read claude jsonl: {e}")))
        }
        Agent::Codex => toolpath_codex::RolloutReader::read_session(file("rollout.jsonl")?)
            .map(Conversation::Codex)
            .map_err(|e| mask(format!("read codex rollout: {e}"))),
        Agent::Pi => toolpath_pi::reader::read_session_with_parent(
            &file("session.jsonl")?,
            toolpath_pi::reader::DEFAULT_MAX_PARENT_DEPTH,
        )
        .map(Conversation::Pi)
        .map_err(|e| mask(format!("read pi session: {e}"))),
        Agent::Copilot => {
            let id = json_values(&text)
                .find_map(|v| v["data"]["sessionId"].as_str().map(str::to_owned))
                .ok_or("transcript names no data.sessionId, which the copilot adapter needs")?;
            let lines = toolpath_copilot::EventReader::read_lines(file("events.jsonl")?)
                .map_err(|e| mask(format!("read copilot events: {e}")))?;
            Ok(Conversation::Copilot(toolpath_copilot::Session {
                dir_path: Path::new("session-state").join(&id),
                id,
                lines,
                workspace: None,
            }))
        }
        Agent::Gemini => {
            // One JSON document (before 0.47, and `p export gemini -o`) or
            // the 0.47 record stream: the adapter reads both.
            let chat = toolpath_gemini::ConversationReader::read_chat_file(file("main.json")?)
                .map_err(|e| mask(format!("read gemini chat file: {e}")))?;
            let id = chat.session_id.clone();
            Ok(Conversation::Gemini(toolpath_gemini::Conversation::new(
                id, chat,
            )))
        }
        Agent::Opencode => {
            let v: Value = serde_json::from_str(&text)
                .map_err(|e| format!("read opencode session: not JSON: {e}"))?;
            let session = if v.get("info").is_some() {
                opencode_export(&v)?
            } else {
                // The rows as `p export opencode -o` writes them.
                serde_json::from_value(v).map_err(|e| format!("read opencode session: {e}"))?
            };
            Ok(Conversation::Opencode(session))
        }
        Agent::Cursor => serde_json::from_str(&text)
            .map(|s| Conversation::Cursor(Box::new(s)))
            .map_err(|e| format!("read cursor session: {e}")),
        Agent::CursorAgent => serde_json::from_str::<toolpath_cursor::AgentStore>(&text)
            .map_err(|e| format!("read cursor-agent store: {e}"))?
            .to_view()
            .map(|v| Conversation::CursorAgent(Box::new(v)))
            .map_err(|e| format!("read cursor-agent store: {e}")),
    }
}

/// `opencode export <id>`: the session row under `info`, each message's row
/// under its own `info` with the parts beside it, camelCase. The adapter
/// reads these rows from the database; this is the same translation
/// `crates/path-cli/tests/cross_harness_matrix.rs` makes, and a row whose
/// payload the adapter's types refuse becomes `Other`/`Unknown` as the
/// database reader makes it.
fn opencode_export(v: &Value) -> Result<toolpath_opencode::Session, String> {
    use toolpath_opencode::{Message, MessageData, Part, PartData, Session};

    fn take(v: &mut Value, key: &str) -> Option<String> {
        v.as_object_mut()?
            .remove(key)
            .and_then(|x| x.as_str().map(str::to_owned))
    }

    let info = &v["info"];
    let text = |key: &str| info[key].as_str().map(str::to_owned);
    let created = info["time"]["created"].as_i64();
    let messages = v["messages"]
        .as_array()
        .ok_or("opencode export has no messages array")?
        .iter()
        .map(|m| {
            let mut row = m["info"].clone();
            let id = take(&mut row, "id").unwrap_or_default();
            let session_id = take(&mut row, "sessionID").unwrap_or_default();
            let time_created = row["time"]["created"].as_i64().unwrap_or(0);
            let data = serde_json::from_value(row).unwrap_or(MessageData::Other);
            let parts = m["parts"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|p| {
                    let mut row = p.clone();
                    Part {
                        id: take(&mut row, "id").unwrap_or_default(),
                        message_id: take(&mut row, "messageID").unwrap_or_else(|| id.clone()),
                        session_id: take(&mut row, "sessionID")
                            .unwrap_or_else(|| session_id.clone()),
                        time_created,
                        time_updated: time_created,
                        data: serde_json::from_value(row).unwrap_or(PartData::Unknown),
                    }
                })
                .collect();
            Message {
                id,
                session_id,
                time_created,
                time_updated: time_created,
                data,
                parts,
            }
        })
        .collect();
    Ok(Session {
        id: text("id").unwrap_or_default(),
        project_id: text("projectID").unwrap_or_default(),
        workspace_id: text("workspaceID"),
        parent_id: text("parentID"),
        slug: text("slug").unwrap_or_default(),
        directory: PathBuf::from(info["directory"].as_str().unwrap_or("/")),
        title: text("title").unwrap_or_default(),
        version: text("version").unwrap_or_else(|| "0.0.0".to_owned()),
        share_url: text("shareURL"),
        summary_additions: info["summary"]["additions"].as_i64(),
        summary_deletions: info["summary"]["deletions"].as_i64(),
        summary_files: info["summary"]["files"].as_i64(),
        time_created: created.unwrap_or(0),
        time_updated: info["time"]["updated"].as_i64().or(created).unwrap_or(0),
        time_compacting: info["time"]["compacting"].as_i64(),
        time_archived: info["time"]["archived"].as_i64(),
        messages,
    })
}

fn first_string(text: &str, key: &str) -> Option<String> {
    json_values(text).find_map(|v| v[key].as_str().map(str::to_owned))
}

fn first_cwd(text: &str) -> Option<String> {
    first_string(text, "cwd")
}

/// The one path of a graph, as `path p export` reads it.
fn single_path(document_json: &str) -> Result<Document, String> {
    Graph::from_json(document_json)
        .map_err(|e| format!("parse document: {e}"))?
        .into_single_path()
        .ok_or_else(|| "expected a single-path graph".to_owned())
}

/// The directory a projection is resumed in: the document's own working directory (its base
/// `file://` uri), as a user passes it with `path resume -C <project>` when resuming where the
/// session ran; [`CALLER_CWD`] when the document names none.
fn resume_dir(path: &Document) -> String {
    path.path
        .base
        .as_ref()
        .and_then(|b| b.uri.strip_prefix("file://"))
        .filter(|d| d.starts_with('/'))
        .unwrap_or(CALLER_CWD)
        .to_owned()
}

/// The document's own working directory (its base `file://` uri), still in JSON form; None when
/// it names none. Unlike [`resume_dir`] there is no [`CALLER_CWD`] fallback: that `/` is where
/// the hermetic tool ran, not where the session did, so a read-back must not report it.
fn value_cwd(document: &Value) -> Option<String> {
    document
        .pointer("/paths/0/path/base/uri")
        .and_then(Value::as_str)
        .and_then(|u| u.strip_prefix("file://"))
        .filter(|d| d.starts_with('/'))
        .map(str::to_owned)
}

/// [`read`], then tell a reader whose format keeps no working directory where the session was
/// resumed. A cursor-agent store holds only md5(cwd) in its path, so a reader handed the bare
/// store cannot make file keys relative; the caller, who chose the directory (`path resume -C`),
/// says it. A document that named no directory gets none (it would be invented). Other formats
/// record their own and are read as they are.
fn read_in(
    agent: Agent,
    input: &[u8],
    scratch: &Path,
    cwd: Option<&str>,
) -> Result<Conversation, String> {
    let mut convo = read(agent, input, scratch)?;
    if let (Conversation::CursorAgent(v), Some(cwd)) = (&mut convo, cwd) {
        v.base.get_or_insert_with(Default::default).working_dir = Some(cwd.to_owned());
    }
    Ok(convo)
}

/// Stage `project`: the transcript `path p export <target> -i <doc> -o <file>`
/// writes for a document, byte for byte.
fn project(path: &Document, target: Agent) -> Result<String, String> {
    let view = extract_conversation(path);
    let cwd = resume_dir(path);
    let cwd = cwd.as_str();
    match target {
        Agent::Claude => {
            let conv = toolpath_claude::ClaudeProjector
                .project(&view)
                .map_err(|e| e.to_string())?;
            if conv.session_id.trim().is_empty() {
                return Err("the document names no Claude session id".into());
            }
            let mut buf = Vec::new();
            toolpath_claude::ConversationWriter::write_conversation(&conv, &mut buf)
                .map_err(|e| e.to_string())?;
            String::from_utf8(buf).map_err(|e| e.to_string())
        }
        Agent::Codex => {
            let session = toolpath_codex::project::CodexProjector::new()
                .with_cwd(cwd)
                .project(&view)
                .map_err(|e| e.to_string())?;
            if session.id.is_empty() {
                return Err("projected session has no id".into());
            }
            jsonl(&session.lines)
        }
        Agent::Pi => {
            let session = toolpath_pi::project::PiProjector::new()
                .with_cwd(cwd)
                .project(&view)
                .map_err(|e| e.to_string())?;
            if session.header.id.is_empty() {
                return Err("projected session has no id".into());
            }
            jsonl(&session.entries)
        }
        Agent::Copilot => {
            // As `p export copilot`: the session id is derived from the document's own id
            // (the same document gives the same id), rooted at the caller's directory.
            let mut view = view;
            view.id = toolpath_copilot::project::session_id_for(&view.id);
            view.base.get_or_insert_with(Default::default).working_dir = Some(cwd.into());
            let session = toolpath_copilot::CopilotProjector::new()
                .project(&view)
                .map_err(|e| e.to_string())?;
            jsonl(&session.lines)
        }
        Agent::Gemini => {
            // As `p export gemini -o`: the caller's directory baked into
            // `projectHash` and `directories`; the main chat file, pretty.
            // Sub-agent chat files land beside it on disk and are not
            // pinned: a delegation lost here shows in the round trip.
            let conv = toolpath_gemini::project::GeminiProjector::new()
                .with_project_hash(toolpath_gemini::paths::project_hash(cwd))
                .with_project_path(cwd)
                .project(&view)
                .map_err(|e| e.to_string())?;
            if conv.session_uuid.is_empty() {
                return Err("projected conversation has no session UUID".into());
            }
            serde_json::to_string_pretty(&conv.main).map_err(|e| e.to_string())
        }
        Agent::Opencode => {
            // As `p export opencode -o`: no project directory (the `--project`
            // form takes one); the session rows, pretty.
            let session = toolpath_opencode::project::OpencodeProjector::new()
                .project(&view)
                .map_err(|e| e.to_string())?;
            if session.id.is_empty() {
                return Err("projected session has no id".into());
            }
            serde_json::to_string_pretty(&session).map_err(|e| e.to_string())
        }
        Agent::Cursor => {
            // As `p export cursor -o`: no workspace (the `--project` form
            // takes one); the composer's rows, pretty.
            let session = toolpath_cursor::CursorProjector::new()
                .project(&view)
                .map_err(|e| e.to_string())?;
            if session.data.composer_id.is_empty() {
                return Err("projected composer has no id".into());
            }
            serde_json::to_string_pretty(&session).map_err(|e| e.to_string())
        }
        Agent::CursorAgent => {
            // As `path resume --harness cursor-agent` writes it, the key
            // pinned: the store's rows (meta, blobs), pretty.
            let store = toolpath_cursor::AgentStoreProjector::new()
                .with_blob_encryption_key(PINNED_BLOB_KEY)
                .project(&view)
                .map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&store).map_err(|e| e.to_string())
        }
    }
}

/// Stage `writer`: one writer on its own. The document is projected for
/// `target` alone, read back by that agent's adapter and derived again, and
/// compared to the document it came from. Fed the same generated document for
/// every agent (`fixtures/full-view.json`), it names what each writer, with
/// its reader, drops, renames or invents, independent of which agent wrote
/// the source.
pub fn writer(document_json: &str, target: Agent, scratch: &Path) -> Result<RoundTrip, StageError> {
    writer_with_document(document_json, target, scratch).map(|(trip, _)| trip)
}

/// The raw projection of a document for `target`, as `path p export <target>
/// -o` writes it: what a writer's declared synthesized values are checked in.
pub fn projection(document_json: &str, target: Agent) -> Result<String, StageError> {
    let path = single_path(document_json).map_err(|e| StageError::new(Stage::Derive, e))?;
    project(&path, target).map_err(|e| StageError::new(Stage::Project, format!("-> {target}: {e}")))
}

/// [`writer`], plus the document the writer's output derives back to (None
/// when its own adapter cannot read it), so the caller can validate it.
pub fn writer_with_document(
    document_json: &str,
    target: Agent,
    scratch: &Path,
) -> Result<(RoundTrip, Option<Value>), StageError> {
    let document: Value = serde_json::from_str(document_json)
        .map_err(|e| StageError::new(Stage::Derive, format!("document is not JSON: {e}")))?;
    let path = single_path(document_json).map_err(|e| StageError::new(Stage::Derive, e))?;
    let text = project(&path, target)
        .map_err(|e| StageError::new(Stage::Project, format!("-> {target}: {e}")))?;
    Ok(round_trip(&document, target, &text, scratch))
}

/// Stage `twice_around`: pass 1's derived document (`first`, what [`round_trip`] returned) is
/// projected to the same target again, read back and derived; the result is compared to
/// `first`. Nothing new should be lost, gained or changed by going round a second time.
fn second_pass(first: Option<&Value>, target: Agent, scratch: &Path) -> TwiceAround {
    let Some(first) = first else {
        return TwiceAround::Unreadable {
            error: "pass 1 was unreadable".to_owned(),
        };
    };
    let second = single_path(&first.to_string())
        .and_then(|path| project(&path, target))
        .and_then(|text| {
            read_in(
                target,
                text.as_bytes(),
                scratch,
                value_cwd(first).as_deref(),
            )
            .and_then(|c| c.derive(&text, scratch))
            .and_then(|json| serde_json::from_str::<Value>(&json).map_err(|e| e.to_string()))
        });
    match second {
        Ok(second) => compare_passes(first, &second),
        Err(error) => TwiceAround::Unreadable { error },
    }
}

/// One JSON object per line, newline-terminated.
fn jsonl<T: Serialize>(items: &[T]) -> Result<String, String> {
    let mut out = String::new();
    for item in items {
        out += &serde_json::to_string(item).map_err(|e| e.to_string())?;
        out.push('\n');
    }
    Ok(out)
}

/// A projection read back by its own adapter and derived again, against the
/// document it came from, with the derived document when there is one. An
/// adapter that cannot read what was projected for it is a pinned outcome,
/// not a failed stage.
fn round_trip(
    document: &Value,
    target: Agent,
    projection: &str,
    scratch: &Path,
) -> (RoundTrip, Option<Value>) {
    let again = read_in(
        target,
        projection.as_bytes(),
        scratch,
        value_cwd(document).as_deref(),
    )
    .and_then(|c| c.derive(projection, scratch))
    .and_then(|json| serde_json::from_str::<Value>(&json).map_err(|e| e.to_string()));
    match again {
        Ok(after) => (compare(document, &after), Some(after)),
        Err(error) => (RoundTrip::Unreadable { error }, None),
    }
}
