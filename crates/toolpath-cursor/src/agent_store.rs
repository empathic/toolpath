//! The `cursor-agent` CLI's chat store: write one from a
//! [`ConversationView`], read one back.
//!
//! Layout, from the CLI bundle (cursor-cli 0-unstable-2026-08-11:
//! `cursor-config` `oe()`, `src/state/index.ts`, `sqlite-blob-store.js`,
//! `agent-kv`):
//!
//! - `<config>/chats/<md5(abs cwd)>/<agentId>/store.db`, where `<config>` is
//!   `$CURSOR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cursor`, else `~/.cursor`
//!   ([`config_dir_from_env`]). The CLI hashes `path.resolve(process.cwd())`,
//!   which is the canonical path once a process runs there.
//! - `store.db`: `blobs(id TEXT PRIMARY KEY, data BLOB)` and
//!   `meta(key TEXT PRIMARY KEY, value TEXT)`, `user_version` 1, WAL. A
//!   blob's id is the hex SHA-256 of its bytes.
//! - `meta['0']` is hex of the UTF-8 JSON [`AgentMeta`];
//!   `latestRootBlobId` names the root blob.
//! - The root blob is an `agent.v1.ConversationStateStructure`: field 1
//!   repeats the ids of the AI SDK `ModelMessage` JSON blobs the model is
//!   sent (the encoding the IDE writer uses for `conversationState`, from
//!   `project::model_messages`); field 8 repeats the ids of
//!   `ConversationTurnStructure` blobs, which the CLI renders as the chat:
//!   `agent_conversation_turn` (1) → `user_message` (1, a `UserMessage` blob
//!   id: `text` 1, `message_id` 2) and `steps` (2, `ConversationStep` blob
//!   ids: `assistant_message` 1 → `text` 1, `thinking_message` 3 → `text` 1).
//!
//! `cursor-agent ls` lists a chat written here (checked with no login, in a
//! temp HOME). That `cursor-agent --resume <id>` sends the history to the
//! model is not yet checked against a live account.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use toolpath_convo::{
    ConversationProjector, ConversationView, ProducerInfo, Role, ToolInvocation, ToolResult, Turn,
};

use crate::error::{CursorError, Result};
use crate::project::{
    ModelHistory, ensure_uuid_shape, hex_lower, model_messages, parse_timestamp_ms,
};
use crate::proto::{self, Value as Wire, bytes_field, put_bytes};

/// The provider id a view read from a `cursor-agent` store carries.
pub const AGENT_PROVIDER_ID: &str = "cursor-agent";
/// The chat database inside `<config>/chats/<hash>/<agentId>/`.
pub const STORE_FILE: &str = "store.db";
/// The `meta` row holding the hex-encoded [`AgentMeta`].
pub const META_KEY: &str = "0";
/// `PRAGMA user_version` of the store the CLI writes.
pub const STORE_USER_VERSION: i32 = 1;
/// The chat name the CLI gives a new agent.
const DEFAULT_NAME: &str = "New Agent";
const NAME_MAX_CHARS: usize = 60;

// agent.v1 field numbers.
const ROOT_MESSAGES: u32 = 1;
const ROOT_TURNS: u32 = 8;
const TURN_AGENT: u32 = 1;
const AGENT_TURN_USER: u32 = 1;
const AGENT_TURN_STEPS: u32 = 2;
const USER_TEXT: u32 = 1;
const USER_MESSAGE_ID: u32 = 2;
const STEP_ASSISTANT: u32 = 1;
const STEP_THINKING: u32 = 3;
const MESSAGE_TEXT: u32 = 1;

/// `meta['0']`, decoded. Unknown keys the CLI adds later
/// (`lastUsedModel`, `approvalMode`, …) are kept in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMeta {
    pub agent_id: String,
    /// Hex id of the root `ConversationStateStructure` blob.
    pub latest_root_blob_id: String,
    pub name: String,
    pub mode: String,
    pub is_run_everything: bool,
    /// Milliseconds since the epoch.
    pub created_at: i64,
    /// 64 hex characters, random per chat. The CLI sends it to the server
    /// as `x-blob-encryption-key`; the local blobs are plain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_encryption_key: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// One blob's bytes. As JSON it is `{"text": …}` when the bytes are UTF-8
/// (the `ModelMessage` blobs) and `{"hex": …}` otherwise (the protobuf
/// blobs), so the `-o` form stays readable and exact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob(pub Vec<u8>);

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum BlobRepr {
    Text(String),
    Hex(String),
}

impl Serialize for Blob {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match std::str::from_utf8(&self.0) {
            Ok(t) => BlobRepr::Text(t.to_string()),
            Err(_) => BlobRepr::Hex(hex_lower(&self.0)),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Blob {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        match BlobRepr::deserialize(d)? {
            BlobRepr::Text(t) => Ok(Blob(t.into_bytes())),
            BlobRepr::Hex(h) => decode_hex(&h).map(Blob).map_err(serde::de::Error::custom),
        }
    }
}

/// One chat: its meta row and every blob, keyed by hex SHA-256.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStore {
    pub meta: AgentMeta,
    pub blobs: BTreeMap<String, Blob>,
}

/// The root blob: the model history and the rendered turns, as blob ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub messages: Vec<String>,
    pub turns: Vec<String>,
}

/// One rendered turn of the chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRecord {
    pub user_text: String,
    pub user_message_id: String,
    pub steps: Vec<StepRecord>,
}

/// One step of a rendered turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepRecord {
    Assistant(String),
    Thinking(String),
    /// A step kind this reader does not decode (a tool call), by field.
    Other(u32),
}

impl AgentStore {
    pub fn agent_id(&self) -> &str {
        &self.meta.agent_id
    }

    fn blob(&self, id: &str) -> Result<&[u8]> {
        self.blobs
            .get(id)
            .map(|b| b.0.as_slice())
            .ok_or_else(|| malformed("blob", format!("no blob {id}")))
    }

    /// The root blob, decoded.
    pub fn root(&self) -> Result<Root> {
        let f = proto::fields(self.blob(&self.meta.latest_root_blob_id)?)?;
        Ok(Root {
            messages: ids(&f, ROOT_MESSAGES),
            turns: ids(&f, ROOT_TURNS),
        })
    }

    /// The model history, in order, as JSON.
    pub fn model_messages(&self) -> Result<Vec<Value>> {
        self.root()?
            .messages
            .iter()
            .map(|id| Ok(serde_json::from_slice(self.blob(id)?)?))
            .collect()
    }

    /// The rendered turns, in order.
    pub fn turns(&self) -> Result<Vec<TurnRecord>> {
        let mut out = Vec::new();
        for id in self.root()?.turns {
            let turn = proto::fields(self.blob(&id)?)?;
            let Some(agent) = proto::last_bytes(&turn, TURN_AGENT) else {
                continue; // a shell turn
            };
            let agent = proto::fields(agent)?;
            let (user_text, user_message_id) = match proto::last_bytes(&agent, AGENT_TURN_USER) {
                Some(um) => {
                    let um = proto::fields(self.blob(&hex_lower(um))?)?;
                    (string(&um, USER_TEXT)?, string(&um, USER_MESSAGE_ID)?)
                }
                None => Default::default(),
            };
            let mut steps = Vec::new();
            for sid in ids(&agent, AGENT_TURN_STEPS) {
                let step = proto::fields(self.blob(&sid)?)?;
                steps.push(match step.first() {
                    Some((STEP_ASSISTANT, Wire::Bytes(m))) => {
                        StepRecord::Assistant(string(&proto::fields(m)?, MESSAGE_TEXT)?)
                    }
                    Some((STEP_THINKING, Wire::Bytes(m))) => {
                        StepRecord::Thinking(string(&proto::fields(m)?, MESSAGE_TEXT)?)
                    }
                    Some((f, _)) => StepRecord::Other(*f),
                    None => StepRecord::Other(0),
                });
            }
            out.push(TurnRecord {
                user_text,
                user_message_id,
                steps,
            });
        }
        Ok(out)
    }

    /// The chat as a [`ConversationView`], from the model history (the
    /// richer of the two lists: it carries reasoning, tool calls and their
    /// results). User turns take their ids from the rendered turns; every
    /// turn takes the chat's `createdAt`, the only time the store keeps.
    pub fn to_view(&self) -> Result<ConversationView> {
        let created = DateTime::<Utc>::from_timestamp_millis(self.meta.created_at);
        let timestamp = created.map(|t| t.to_rfc3339()).unwrap_or_default();
        let mut user_ids = self
            .turns()?
            .into_iter()
            .filter(|t| !t.user_text.is_empty())
            .map(|t| t.user_message_id);
        let mut turns: Vec<Turn> = Vec::new();
        for (i, msg) in self.model_messages()?.iter().enumerate() {
            let parts = msg["content"].as_array().cloned().unwrap_or_default();
            let texts = |kind: &str| -> String {
                parts
                    .iter()
                    .filter(|p| p["type"] == kind)
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            match msg["role"].as_str() {
                Some("system") => {
                    turns.push(new_turn(
                        format!("system-{i}"),
                        Role::System,
                        &timestamp,
                        system_text(msg),
                        &turns,
                    ));
                }
                Some("user") => {
                    let id = user_ids
                        .next()
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| format!("user-{i}"));
                    turns.push(new_turn(id, Role::User, &timestamp, texts("text"), &turns));
                }
                Some("assistant") => {
                    let id = msg["id"]
                        .as_str()
                        .map_or_else(|| format!("assistant-{i}"), str::to_string);
                    let mut turn = new_turn(id, Role::Assistant, &timestamp, texts("text"), &turns);
                    let thinking = texts("reasoning");
                    turn.thinking = (!thinking.is_empty()).then_some(thinking);
                    turn.tool_uses = parts
                        .iter()
                        .filter(|p| p["type"] == "tool-call")
                        .map(|p| {
                            let name = p["toolName"].as_str().unwrap_or_default().to_string();
                            ToolInvocation {
                                id: p["toolCallId"].as_str().unwrap_or_default().to_string(),
                                category: crate::tool_category(0, &name),
                                name,
                                input: p["args"].clone(),
                                result: None,
                            }
                        })
                        .collect();
                    turns.push(turn);
                }
                Some("tool") => {
                    for p in parts.iter().filter(|p| p["type"] == "tool-result") {
                        let call = p["toolCallId"].as_str().unwrap_or_default();
                        let Some(tu) = turns
                            .iter_mut()
                            .rev()
                            .flat_map(|t| t.tool_uses.iter_mut())
                            .find(|tu| tu.id == call)
                        else {
                            continue;
                        };
                        tu.result = Some(ToolResult {
                            content: match &p["result"] {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            },
                            is_error: p["isError"].as_bool().unwrap_or(false),
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(ConversationView {
            id: self.meta.agent_id.clone(),
            started_at: created,
            last_activity: created,
            turns,
            provider_id: Some(AGENT_PROVIDER_ID.into()),
            producer: Some(ProducerInfo {
                name: AGENT_PROVIDER_ID.into(),
                version: None,
            }),
            ..Default::default()
        })
    }

    /// Write the chat to `db` (its directory is created), as the CLI lays it
    /// out. Rows already there are replaced.
    pub fn write(&self, db: &Path) -> Result<()> {
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut conn = rusqlite::Connection::open(db)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "user_version", STORE_USER_VERSION)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS blobs (id TEXT PRIMARY KEY, data BLOB);
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);",
        )?;
        let tx = conn.transaction()?;
        for (id, blob) in &self.blobs {
            tx.execute(
                "INSERT OR REPLACE INTO blobs (id, data) VALUES (?1, ?2)",
                rusqlite::params![id, blob.0],
            )?;
        }
        let meta = serde_json::to_string(&self.meta)?;
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            rusqlite::params![META_KEY, hex_lower(meta.as_bytes())],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Read a chat from `db`, read-only.
    pub fn read(db: &Path) -> Result<AgentStore> {
        let conn =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let hex: String = conn.query_row(
            "SELECT value FROM meta WHERE key = ?1",
            rusqlite::params![META_KEY],
            |r| r.get(0),
        )?;
        let meta: AgentMeta =
            serde_json::from_slice(&decode_hex(&hex).map_err(|e| malformed("meta", e))?)?;
        let mut stmt = conn.prepare("SELECT id, data FROM blobs")?;
        let blobs = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, Blob(r.get(1)?))))?
            .collect::<std::result::Result<BTreeMap<_, _>, _>>()?;
        Ok(AgentStore { meta, blobs })
    }
}

/// Builds an [`AgentStore`] from a view.
#[derive(Debug, Clone, Default)]
pub struct AgentStoreProjector {
    pub agent_id: Option<String>,
    pub name: Option<String>,
    pub blob_encryption_key: Option<String>,
}

impl AgentStoreProjector {
    pub fn new() -> Self {
        Self::default()
    }

    /// The chat id. Defaults to the view's id, UUID-shaped as the CLI's are.
    pub fn with_agent_id(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(id.into());
        self
    }

    /// The chat's name in `cursor-agent ls`. Defaults to the first line of
    /// the first user message.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Defaults to 32 random bytes, as the CLI makes it.
    pub fn with_blob_encryption_key(mut self, key: impl Into<String>) -> Self {
        self.blob_encryption_key = Some(key.into());
        self
    }
}

impl ConversationProjector for AgentStoreProjector {
    type Output = AgentStore;

    fn project(&self, view: &ConversationView) -> toolpath_convo::Result<AgentStore> {
        Ok(self.build(view))
    }
}

impl AgentStoreProjector {
    fn build(&self, view: &ConversationView) -> AgentStore {
        let mut blobs: BTreeMap<String, Blob> = BTreeMap::new();
        let mut put = |bytes: Vec<u8>| -> [u8; 32] {
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            blobs.insert(hex_lower(&digest), Blob(bytes));
            digest
        };

        let mut root = Vec::new();
        let ModelHistory {
            order,
            blobs: messages,
        } = model_messages(view);
        for digest in &order {
            let body = messages[&hex_lower(digest)].clone().into_bytes();
            put(body);
            put_bytes(&mut root, ROOT_MESSAGES, digest);
        }

        for turn in rendered_turns(view) {
            let mut agent = bytes_field(AGENT_TURN_USER, &put(turn.user));
            for step in turn.steps {
                put_bytes(&mut agent, AGENT_TURN_STEPS, &put(step));
            }
            put_bytes(&mut root, ROOT_TURNS, &put(bytes_field(TURN_AGENT, &agent)));
        }
        let root_id = hex_lower(&put(root));

        let meta = AgentMeta {
            agent_id: self
                .agent_id
                .clone()
                .unwrap_or_else(|| ensure_uuid_shape(&view.id)),
            latest_root_blob_id: root_id,
            name: self.name.clone().unwrap_or_else(|| default_name(view)),
            mode: "default".into(),
            is_run_everything: false,
            created_at: created_at(view),
            blob_encryption_key: Some(self.blob_encryption_key.clone().unwrap_or_else(random_key)),
            extra: serde_json::Map::new(),
        };
        AgentStore { meta, blobs }
    }
}

/// A turn as the CLI renders it: the user message and its steps, encoded.
struct Rendered {
    user: Vec<u8>,
    steps: Vec<Vec<u8>>,
}

/// One rendered turn per user message; the assistant turns after it become
/// its steps (thinking, then text). Tool calls are in the model history
/// only. Assistant turns before any user message get a turn with an empty
/// user message.
fn rendered_turns(view: &ConversationView) -> Vec<Rendered> {
    let user = |text: &str, id: &str| {
        let mut m = bytes_field(USER_TEXT, text.as_bytes());
        put_bytes(&mut m, USER_MESSAGE_ID, id.as_bytes());
        m
    };
    let mut out: Vec<Rendered> = Vec::new();
    for turn in &view.turns {
        match turn.role {
            Role::User if !turn.text.is_empty() => out.push(Rendered {
                user: user(&turn.text, &turn.id),
                steps: Vec::new(),
            }),
            Role::Assistant => {
                if out.is_empty() {
                    out.push(Rendered {
                        user: user("", ""),
                        steps: Vec::new(),
                    });
                }
                let steps = &mut out.last_mut().expect("pushed above").steps;
                if let Some(t) = turn.thinking.as_deref().filter(|t| !t.is_empty()) {
                    steps.push(bytes_field(
                        STEP_THINKING,
                        &bytes_field(MESSAGE_TEXT, t.as_bytes()),
                    ));
                }
                if !turn.text.is_empty() {
                    steps.push(bytes_field(
                        STEP_ASSISTANT,
                        &bytes_field(MESSAGE_TEXT, turn.text.as_bytes()),
                    ));
                }
            }
            _ => {}
        }
    }
    out
}

/// A system `ModelMessage`'s text: `content` is a string (the CLI's first
/// blob, the writers' system turn) or a list of text parts.
pub(crate) fn system_text(msg: &Value) -> String {
    match &msg["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p["type"] == "text")
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn new_turn(id: String, role: Role, timestamp: &str, text: String, before: &[Turn]) -> Turn {
    Turn {
        provider: None,
        parent_id: before.last().map(|t| t.id.clone()),
        id,
        group_id: None,
        role,
        timestamp: timestamp.to_string(),
        text,
        thinking: None,
        thinking_signature: None,
        tool_uses: Vec::new(),
        model: None,
        stop_reason: None,
        token_usage: None,
        attributed_token_usage: None,
        environment: None,
        delegations: Vec::new(),
        file_mutations: Vec::new(),
    }
}

fn default_name(view: &ConversationView) -> String {
    view.turns
        .iter()
        .find(|t| t.role == Role::User && !t.text.trim().is_empty())
        .and_then(|t| t.text.lines().map(str::trim).find(|l| !l.is_empty()))
        .map(|l| l.chars().take(NAME_MAX_CHARS).collect())
        .unwrap_or_else(|| DEFAULT_NAME.to_string())
}

fn created_at(view: &ConversationView) -> i64 {
    view.started_at
        .map(|t| t.timestamp_millis())
        .or_else(|| {
            view.turns
                .iter()
                .find_map(|t| parse_timestamp_ms(&t.timestamp))
        })
        .unwrap_or_else(|| Utc::now().timestamp_millis())
}

/// 64 hex characters from two v4 UUIDs' random bits.
fn random_key() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn ids(msg: &[(u32, Wire<'_>)], field: u32) -> Vec<String> {
    proto::repeated_bytes(msg, field)
        .into_iter()
        .map(hex_lower)
        .collect()
}

fn string(msg: &[(u32, Wire<'_>)], field: u32) -> Result<String> {
    proto::last_bytes(msg, field)
        .map(|b| {
            String::from_utf8(b.to_vec()).map_err(|e| malformed("string field", e.to_string()))
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn decode_hex(s: &str) -> std::result::Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            s.get(i..i + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
                .ok_or_else(|| format!("bad hex at {i}"))
        })
        .collect()
}

fn malformed(what: &str, detail: impl Into<String>) -> CursorError {
    CursorError::MalformedPayload {
        what: what.into(),
        detail: detail.into(),
    }
}

/// `<config>`: `$CURSOR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cursor`, else
/// `~/.cursor` (blank values are ignored, as the CLI ignores them).
pub fn config_dir_from_env() -> Result<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.to_string_lossy().trim().is_empty());
    if let Some(dir) = var("CURSOR_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(xdg) = var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("cursor"));
    }
    var("HOME")
        .or_else(|| var("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".cursor"))
        .ok_or(CursorError::NoHomeDirectory)
}

/// `<config>/chats/<md5(cwd)>`: the chats `cursor-agent` lists for `cwd`.
/// `cwd` should be canonical, as the CLI's `process.cwd()` is.
pub fn workspace_chats_dir(config_dir: &Path, cwd: &Path) -> PathBuf {
    let digest = md5::compute(cwd.to_string_lossy().as_bytes());
    config_dir.join("chats").join(format!("{digest:x}"))
}

/// `<config>/chats/<md5(cwd)>/<agentId>/store.db`.
pub fn store_path(config_dir: &Path, cwd: &Path, agent_id: &str) -> PathBuf {
    workspace_chats_dir(config_dir, cwd)
        .join(agent_id)
        .join(STORE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath_convo::ToolCategory;

    fn turn(id: &str, role: Role, text: &str) -> Turn {
        let mut t = new_turn(id.into(), role, "2026-10-19T12:00:00Z", text.into(), &[]);
        t.parent_id = None;
        t
    }

    fn view() -> ConversationView {
        let mut a = turn("a1", Role::Assistant, "Acknowledged.");
        a.thinking = Some("the user wants a codeword kept".into());
        let mut b = turn("a2", Role::Assistant, "");
        b.tool_uses = vec![ToolInvocation {
            id: "call-1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "ls"}),
            result: Some(ToolResult {
                content: "notes.txt".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::Shell),
        }];
        ConversationView {
            id: "chat-1".into(),
            turns: vec![
                turn(
                    "u1",
                    Role::User,
                    "Remember PELICAN-4417.\nJust acknowledge.",
                ),
                a,
                b,
            ],
            ..Default::default()
        }
    }

    fn project() -> AgentStore {
        AgentStoreProjector::new()
            .with_blob_encryption_key("ab".repeat(32))
            .project(&view())
            .unwrap()
    }

    /// The root blob decoded by hand, field by field, against what the
    /// spike's `write_store.py` wrote and `cursor-agent ls` listed.
    #[test]
    fn root_blob_decodes_to_the_history_and_the_turns() {
        let s = project();
        let root = proto::fields(&s.blobs[&s.meta.latest_root_blob_id].0).unwrap();
        let messages = proto::repeated_bytes(&root, 1);
        let turns = proto::repeated_bytes(&root, 8);
        assert_eq!(
            root.len(),
            messages.len() + turns.len(),
            "only fields 1 and 8"
        );
        assert_eq!(
            messages.len(),
            4,
            "user, assistant (reasoning + text), assistant (tool call), tool"
        );
        assert_eq!(turns.len(), 1);
        for id in messages.iter().chain(&turns) {
            let id = hex_lower(id);
            let digest: [u8; 32] = Sha256::digest(&s.blobs[&id].0).into();
            assert_eq!(hex_lower(&digest), id, "a blob's id is its sha256");
        }
        let first: Value = serde_json::from_slice(&s.blobs[&hex_lower(messages[0])].0).unwrap();
        assert_eq!(
            first,
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "Remember PELICAN-4417.\nJust acknowledge."}]})
        );

        let turn = proto::fields(&s.blobs[&hex_lower(turns[0])].0).unwrap();
        let agent = proto::fields(proto::last_bytes(&turn, 1).unwrap()).unwrap();
        let um =
            proto::fields(&s.blobs[&hex_lower(proto::last_bytes(&agent, 1).unwrap())].0).unwrap();
        assert_eq!(
            proto::last_bytes(&um, 1).unwrap(),
            b"Remember PELICAN-4417.\nJust acknowledge."
        );
        assert_eq!(proto::last_bytes(&um, 2).unwrap(), b"u1");
        let steps = proto::repeated_bytes(&agent, 2);
        assert_eq!(steps.len(), 2, "thinking, then text");
        let step = proto::fields(&s.blobs[&hex_lower(steps[1])].0).unwrap();
        let msg = proto::fields(proto::last_bytes(&step, 1).unwrap()).unwrap();
        assert_eq!(proto::last_bytes(&msg, 1).unwrap(), b"Acknowledged.");
    }

    #[test]
    fn the_cli_and_the_ide_list_the_same_history() {
        let s = project();
        let ide = crate::CursorProjector::new().project(&view()).unwrap();
        let root = s.root().unwrap();
        assert_eq!(root.messages.len(), ide.agent_blobs.len());
        for id in &root.messages {
            assert_eq!(
                std::str::from_utf8(&s.blobs[id].0).unwrap(),
                ide.agent_blobs[id]
            );
        }
    }

    #[test]
    fn meta_is_what_the_cli_writes() {
        let s = project();
        assert_eq!(s.meta.agent_id, ensure_uuid_shape("chat-1"));
        assert_eq!(s.meta.name, "Remember PELICAN-4417.");
        assert_eq!(s.meta.mode, "default");
        assert!(!s.meta.is_run_everything);
        assert_eq!(s.meta.created_at, 1_792_411_200_000);
        let v = serde_json::to_value(&s.meta).unwrap();
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        for k in [
            "agentId",
            "latestRootBlobId",
            "name",
            "mode",
            "isRunEverything",
            "createdAt",
            "blobEncryptionKey",
        ] {
            assert!(keys.contains(&k.to_string()), "{k}");
        }
        let key = random_key();
        assert_eq!(key.len(), 64);
        assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(key, random_key());
    }

    #[test]
    fn store_db_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let db = store_path(dir.path(), Path::new("/proj"), "abc");
        let s = project();
        s.write(&db).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        let (v, mode): (i32, String) = (
            conn.query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap(),
            conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap(),
        );
        assert_eq!((v, mode.as_str()), (1, "wal"));
        let meta_hex: String = conn
            .query_row("SELECT value FROM meta WHERE key = '0'", [], |r| r.get(0))
            .unwrap();
        let meta: Value = serde_json::from_slice(&decode_hex(&meta_hex).unwrap()).unwrap();
        assert_eq!(meta["latestRootBlobId"], s.meta.latest_root_blob_id);
        drop(conn);
        assert_eq!(AgentStore::read(&db).unwrap(), s);
    }

    #[test]
    fn the_view_reads_back() {
        let v = project().to_view().unwrap();
        assert_eq!(v.provider_id.as_deref(), Some("cursor-agent"));
        let roles: Vec<_> = v.turns.iter().map(|t| t.role.clone()).collect();
        assert_eq!(roles, [Role::User, Role::Assistant, Role::Assistant]);
        assert_eq!(v.turns[0].id, "u1");
        assert_eq!(v.turns[1].id, "a1");
        assert_eq!(
            v.turns[1].thinking.as_deref(),
            Some("the user wants a codeword kept")
        );
        assert_eq!(v.turns[2].parent_id.as_deref(), Some("a1"));
        let tu = &v.turns[2].tool_uses[0];
        assert_eq!((tu.id.as_str(), tu.name.as_str()), ("call-1", "Bash"));
        // The model history keeps the name, not the category; Cursor's
        // classifier does not know "Bash", so the category is lost.
        assert_eq!(tu.category, None);
        assert_eq!(tu.result.as_ref().unwrap().content, "notes.txt");
        let turns = project().turns().unwrap();
        assert_eq!(
            turns[0].steps,
            [
                StepRecord::Thinking("the user wants a codeword kept".into()),
                StepRecord::Assistant("Acknowledged.".into()),
            ]
        );
    }

    #[test]
    fn the_json_form_round_trips() {
        let s = project();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""hex":"#) && json.contains(r#""text":"#));
        assert_eq!(serde_json::from_str::<AgentStore>(&json).unwrap(), s);
    }

    #[test]
    fn store_path_hashes_the_cwd_like_the_cli() {
        // md5 of the path string (`printf %s /tmp/x | md5`); the spike's
        // chat for its project directory was listed from the directory named
        // by this hash of the realpath.
        let p = store_path(Path::new("/cfg"), Path::new("/tmp/x"), "id");
        assert_eq!(
            p,
            Path::new("/cfg/chats/7ae3976faedb45a92335f73e4d7bb9e5/id/store.db")
        );
    }
}
