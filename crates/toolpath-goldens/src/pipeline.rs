//! The pipeline: one set in, every stage's output out, or the first stage
//! that failed.
//!
//! `read` (bytes -> the agent's conversation type, through its adapter's
//! reader), `ir` (`to_view`), `derive` (the toolpath graph `path p derive`
//! prints, thinking included), `project` (every other agent's transcript, as
//! `path p export <agent> -o` writes it), round trips (each projection read
//! back by its own adapter and derived again), `same_format` (the set's own
//! format again), then the shed report and the known defects over all of
//! that. Nothing here reads a manifest, hashes a file or runs a child
//! process: given the same adapters and the same bytes it produces the same
//! [`Stages`].

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use toolpath::v1::{Base, Graph, Path as Document};
use toolpath_convo::{ConversationProjector, ConversationView, extract_conversation};

use crate::canonical::canonicalize;
use crate::defects::{Evidence, known_defects};
use crate::keypaths::json_lines;
use crate::losses::compare;
use crate::model::{Agent, RoundTrip, Set, Stage, StageError, Stages};
use crate::shed::report;

/// The working directory `path p export codex|pi|copilot` records as the
/// session's: the caller's, not the source session's (a known defect). The
/// hermetic tool ran from `/`; the pipeline passes the same value.
pub const CALLER_CWD: &str = "/";

/// Run every stage over `set`. `scratch` is a directory the adapters' readers
/// may write transcripts into (they read files, not bytes); nothing in it
/// survives the call, and its path must not appear in the output, which the
/// caller checks with [`crate::leaks::leaks`].
pub fn run(set: &Set, scratch: &Path) -> Result<Stages, StageError> {
    let fail = |stage: Stage| move |e: String| StageError::new(stage, e);

    let source = set.text();
    let convo = read(set.agent, &set.input, scratch).map_err(fail(Stage::Read))?;
    let ir = convo.view();
    let document_json = convo.derive(&source).map_err(fail(Stage::Derive))?;
    let document: Value = serde_json::from_str(&document_json)
        .map_err(|e| StageError::new(Stage::Derive, format!("document is not JSON: {e}")))?;
    let path = single_path(&document_json).map_err(fail(Stage::Derive))?;

    let mut raw = BTreeMap::new();
    for target in set.agent.targets() {
        let text = project(&path, target)
            .map_err(|e| StageError::new(Stage::Project, format!("-> {target}: {e}")))?;
        raw.insert(target, text);
    }
    let round_trips = raw
        .iter()
        .map(|(target, text)| (*target, round_trip(&document, *target, text, scratch)))
        .collect();
    let projections: BTreeMap<Agent, String> = raw
        .into_iter()
        .map(|(target, text)| (target, settle(target, text)))
        .collect();

    let same_format = project(&path, set.agent).map_err(fail(Stage::SameFormat))?;
    let shed = report(
        &source,
        &document_json,
        &document,
        &projections,
        &same_format,
    );
    let known_defects = known_defects(&Evidence {
        set,
        projections: &projections,
        same_format: &same_format,
    });
    Ok(Stages {
        ir,
        document,
        projections,
        round_trips,
        shed,
        known_defects,
    })
}

/// The form a projection is pinned in: raw for every agent whose output is
/// the same on every run, canonical for Copilot, whose projection mints a
/// session id.
fn settle(target: Agent, text: String) -> String {
    match target {
        Agent::Copilot => canonicalize(&text),
        Agent::Claude | Agent::Codex | Agent::Pi => text,
    }
}

/// A transcript read by its agent's adapter.
enum Conversation {
    Claude(toolpath_claude::Conversation),
    Codex(toolpath_codex::Session),
    Pi(toolpath_pi::PiSession),
    Copilot(toolpath_copilot::Session),
}

impl Conversation {
    /// Stage `ir`.
    fn view(&self) -> ConversationView {
        match self {
            Conversation::Claude(c) => toolpath_claude::provider::to_view(c),
            Conversation::Codex(s) => toolpath_codex::to_view(s),
            Conversation::Pi(s) => toolpath_pi::session_to_view(s),
            Conversation::Copilot(s) => toolpath_copilot::to_view(s),
        }
    }

    /// Stage `derive`: the graph `path p derive <agent>` prints, as compact
    /// JSON. Claude includes thinking blocks, as the CLI does, and falls
    /// back to the transcript's first `cwd` as the base when the entries
    /// carry none.
    fn derive(&self, source: &str) -> Result<String, String> {
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
        };
        Graph::from_path(path).to_json().map_err(|e| e.to_string())
    }
}

/// Stage `read`. The adapters read files, so the bytes go to a file under
/// `scratch` first, named the way each adapter expects (Claude derives the
/// session id from the file stem). Errors never carry the scratch path.
fn read(agent: Agent, input: &[u8], scratch: &Path) -> Result<Conversation, String> {
    let dir = tempfile::tempdir_in(scratch).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(input);
    let mask = |e: String| e.replace(&dir.path().display().to_string(), "<tmp>");
    let file = |name: &str| -> Result<std::path::PathBuf, String> {
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
            let id = json_lines(&text)
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
    }
}

fn first_string(text: &str, key: &str) -> Option<String> {
    json_lines(text).find_map(|v| v[key].as_str().map(str::to_owned))
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

/// Stage `project`: the transcript `path p export <target> -i <doc> -o <file>`
/// writes for a document, byte for byte.
fn project(path: &Document, target: Agent) -> Result<String, String> {
    let view = extract_conversation(path);
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
                .with_cwd(CALLER_CWD)
                .project(&view)
                .map_err(|e| e.to_string())?;
            if session.id.is_empty() {
                return Err("projected session has no id".into());
            }
            jsonl(&session.lines)
        }
        Agent::Pi => {
            let session = toolpath_pi::project::PiProjector::new()
                .with_cwd(CALLER_CWD)
                .project(&view)
                .map_err(|e| e.to_string())?;
            if session.header.id.is_empty() {
                return Err("projected session has no id".into());
            }
            jsonl(&session.entries)
        }
        Agent::Copilot => {
            // As `p export copilot`: a fresh session id so an export never
            // clobbers an existing Copilot session, rooted at the caller's
            // directory. The id is why the pinned form is canonical.
            let mut view = view;
            view.id = uuid::Uuid::new_v4().to_string();
            view.base.get_or_insert_with(Default::default).working_dir = Some(CALLER_CWD.into());
            let session = toolpath_copilot::CopilotProjector::new()
                .project(&view)
                .map_err(|e| e.to_string())?;
            jsonl(&session.lines)
        }
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
/// document it came from. An adapter that cannot read what was projected
/// for it is a pinned outcome, not a failed stage.
fn round_trip(document: &Value, target: Agent, projection: &str, scratch: &Path) -> RoundTrip {
    let again = read(target, projection.as_bytes(), scratch)
        .and_then(|c| c.derive(projection))
        .and_then(|json| serde_json::from_str::<Value>(&json).map_err(|e| e.to_string()));
    match again {
        Ok(after) => compare(document, &after),
        Err(error) => RoundTrip::Unreadable { error },
    }
}
