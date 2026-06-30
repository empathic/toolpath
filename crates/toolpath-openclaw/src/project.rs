//! [`OpenClawProjector`] — maps a [`ConversationView`] back into an OpenClaw
//! [`OpenClawSession`], and writes it to disk as a resume-ready ("incepted")
//! session under `agents/<agentId>/sessions/` plus a `sessions.json` routing
//! entry so a running OpenClaw instance can pick it up.
//!
//! This is the inverse of [`crate::provider::session_to_view`]. Provider-
//! specific extras are not carried on the IR, so the projector synthesizes
//! OpenClaw fields from typed IR fields and sensible defaults (api/provider
//! `anthropic`, stop reason `stop`). Foreign tool names are remapped through
//! [`crate::provider::native_name`].

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use toolpath_convo::{
    ConversationProjector, ConversationView, Result, Role, ToolInvocation, Turn,
};

use crate::error::OpenClawError;
use crate::paths::{IndexEntry, SessionsIndex, normalize_agent_id};
use crate::reader::OpenClawSession;
use crate::types::{
    AgentMessage, ContentBlock, CostBreakdown, Entry, EntryBase, KnownStopReason, MessageContent,
    SessionHeader, StopReason, ToolResultContent, Usage,
};

/// Project a [`ConversationView`] into an OpenClaw [`OpenClawSession`] and
/// optionally write ("incept") it into a state directory.
#[derive(Debug, Clone, Default)]
pub struct OpenClawProjector {
    /// Override the session header's `cwd` (else pulled from the first turn's
    /// environment, falling back to `/`).
    pub cwd: Option<String>,
    /// Default `api` for assistant turns from a non-OpenClaw source.
    pub default_api: Option<String>,
    /// Default `provider` for assistant turns from a non-OpenClaw source.
    pub default_provider: Option<String>,
    /// Agent bucket the incepted session belongs to (default `main`).
    pub agent_id: Option<String>,
    /// Channel for the inception routing key (e.g. `whatsapp`).
    pub channel: Option<String>,
    /// Peer id for the inception routing key.
    pub peer_id: Option<String>,
}

impl OpenClawProjector {
    /// A projector with all defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the header `cwd`.
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Default `api` for assistant turns lacking one.
    pub fn with_default_api(mut self, api: impl Into<String>) -> Self {
        self.default_api = Some(api.into());
        self
    }

    /// Default `provider` for assistant turns lacking one.
    pub fn with_default_provider(mut self, provider: impl Into<String>) -> Self {
        self.default_provider = Some(provider.into());
        self
    }

    /// The agent bucket for inception.
    pub fn with_agent(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    /// The channel for the inception routing key.
    pub fn with_channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }

    /// The peer id for the inception routing key.
    pub fn with_peer(mut self, peer_id: impl Into<String>) -> Self {
        self.peer_id = Some(peer_id.into());
        self
    }

    /// Configure channel/peer/agent from a Path's `meta.extra["openclaw"]`
    /// object (as written by [`crate::derive::derive_path`]).
    pub fn with_meta_extra(mut self, extra: &Value) -> Self {
        if let Some(c) = extra.get("channel").and_then(Value::as_str) {
            self.channel = Some(c.to_string());
        }
        if let Some(p) = extra.get("peerId").and_then(Value::as_str) {
            self.peer_id = Some(p.to_string());
        }
        if let Some(a) = extra.get("agentId").and_then(Value::as_str) {
            self.agent_id = Some(a.to_string());
        }
        self
    }

    /// The effective agent id (default `main`).
    fn effective_agent(&self) -> String {
        normalize_agent_id(self.agent_id.as_deref().unwrap_or(crate::DEFAULT_AGENT_ID))
    }

    /// Write a projected session to `<state_dir>/agents/<agentId>/sessions/`
    /// and upsert a `sessions.json` routing entry. Returns the transcript path.
    pub fn write_session(
        &self,
        session: &OpenClawSession,
        state_dir: &Path,
    ) -> crate::error::Result<PathBuf> {
        let agent_id = self.effective_agent();
        let dir = state_dir.join("agents").join(&agent_id).join("sessions");
        std::fs::create_dir_all(&dir)?;

        let file_name = format!("{}.jsonl", session.header.id);
        let file = dir.join(&file_name);

        let mut out = String::new();
        // The projected session's first entry is the header; write entries
        // verbatim, one JSON object per line.
        for entry in &session.entries {
            out.push_str(&serde_json::to_string(entry).map_err(OpenClawError::Json)?);
            out.push('\n');
        }
        write_private(&file, out.as_bytes())?;

        self.upsert_routing(&dir, &agent_id, &session.header.id, &file_name)?;
        Ok(file)
    }

    fn upsert_routing(
        &self,
        dir: &Path,
        agent_id: &str,
        session_id: &str,
        file_name: &str,
    ) -> crate::error::Result<()> {
        let key = match (&self.channel, &self.peer_id) {
            (Some(ch), Some(peer)) => format!("agent:{agent_id}:{ch}:direct:{peer}"),
            _ => format!("agent:{agent_id}:main"),
        };

        let mut map: BTreeMap<String, IndexEntry> = SessionsIndex::load(dir).map(|i| i.0).unwrap_or_default();
        map.insert(
            key,
            IndexEntry {
                session_id: Some(session_id.to_string()),
                session_file: Some(file_name.to_string()),
                extra: HashMap::new(),
            },
        );
        let json = serde_json::to_vec_pretty(&map).map_err(OpenClawError::Json)?;
        write_private(&dir.join("sessions.json"), &json)?;
        Ok(())
    }
}

impl ConversationProjector for OpenClawProjector {
    type Output = OpenClawSession;

    fn project(&self, view: &ConversationView) -> Result<OpenClawSession> {
        Ok(project_view(self, view))
    }
}

// ── Projection logic ─────────────────────────────────────────────────

fn project_view(cfg: &OpenClawProjector, view: &ConversationView) -> OpenClawSession {
    let cwd = cfg
        .cwd
        .clone()
        .or_else(|| {
            view.turns
                .iter()
                .find_map(|t| t.environment.as_ref()?.working_dir.clone())
        })
        .unwrap_or_else(|| "/".to_string());

    let timestamp = view
        .started_at
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .or_else(|| view.turns.first().map(|t| t.timestamp.clone()))
        .unwrap_or_default();

    let header = SessionHeader {
        version: 3,
        id: view.id.clone(),
        timestamp,
        cwd,
        parent_session: None,
        extra: HashMap::new(),
    };

    let mut entries: Vec<Entry> = vec![Entry::Session(header.clone())];
    for turn in &view.turns {
        emit_turn_entries(cfg, turn, &mut entries);
    }

    // Append a trailing `leaf` row pointing at the last content entry so the
    // incepted session's visible head is well defined.
    if let Some(last_id) = entries
        .iter()
        .rev()
        .find_map(|e| e.base().map(|b| b.id.clone()))
    {
        entries.push(Entry::Leaf {
            base: EntryBase {
                id: format!("{last_id}-leaf"),
                parent_id: Some(last_id.clone()),
                timestamp: header.timestamp.clone(),
                append_mode: None,
            },
            target_id: Some(last_id),
            append_parent_id: None,
            extra: HashMap::new(),
        });
    }

    OpenClawSession {
        header,
        entries,
        file_path: PathBuf::new(),
        parent: None,
        session_key: None,
        parsed_key: None,
    }
}

fn emit_turn_entries(cfg: &OpenClawProjector, turn: &Turn, entries: &mut Vec<Entry>) {
    match &turn.role {
        Role::User => emit_user(turn, entries),
        Role::Assistant => emit_assistant(cfg, turn, entries),
        Role::System => emit_system(turn, entries),
        Role::Other(other) => match other.as_str() {
            "bash" => emit_bash(turn, entries),
            o if o.starts_with("custom:") => {
                emit_custom_message(turn, o.strip_prefix("custom:").unwrap_or(o), entries)
            }
            o => emit_custom_message(turn, o, entries),
        },
    }
}

fn emit_user(turn: &Turn, entries: &mut Vec<Entry>) {
    entries.push(Entry::Message {
        base: base_for(turn),
        message: AgentMessage::User {
            content: MessageContent::Text(turn.text.clone()),
            timestamp: ts_millis(&turn.timestamp),
            extra: HashMap::new(),
        },
        extra: HashMap::new(),
    });
}

fn emit_assistant(cfg: &OpenClawProjector, turn: &Turn, entries: &mut Vec<Entry>) {
    let mut blocks: Vec<ContentBlock> = Vec::new();
    if let Some(t) = &turn.thinking
        && !t.is_empty()
    {
        blocks.push(ContentBlock::Thinking {
            thinking: t.clone(),
            extra: HashMap::new(),
        });
    }
    if !turn.text.is_empty() {
        blocks.push(ContentBlock::Text {
            text: turn.text.clone(),
            extra: HashMap::new(),
        });
    }
    for tu in &turn.tool_uses {
        blocks.push(ContentBlock::ToolCall {
            id: tu.id.clone(),
            name: tool_native_name(tu),
            arguments: tu.input.clone(),
            extra: HashMap::new(),
        });
    }

    let api = cfg
        .default_api
        .clone()
        .unwrap_or_else(|| "anthropic-messages".to_string());
    let provider = cfg
        .default_provider
        .clone()
        .unwrap_or_else(|| "anthropic".to_string());

    entries.push(Entry::Message {
        base: base_for(turn),
        message: AgentMessage::Assistant {
            content: blocks,
            api,
            provider,
            model: turn.model.clone().unwrap_or_default(),
            usage: build_usage(turn),
            stop_reason: parse_stop_reason(turn.stop_reason.as_deref()),
            error_message: None,
            timestamp: ts_millis(&turn.timestamp),
            extra: HashMap::new(),
        },
        extra: HashMap::new(),
    });

    // One separate toolResult entry per invocation with a result.
    let mut prev_id = turn.id.clone();
    let mut suffix = 0usize;
    for tu in &turn.tool_uses {
        let Some(result) = &tu.result else { continue };
        suffix += 1;
        let tr_id = format!("{}-tr-{}", turn.id, suffix);
        entries.push(Entry::Message {
            base: EntryBase {
                id: tr_id.clone(),
                parent_id: Some(prev_id.clone()),
                timestamp: turn.timestamp.clone(),
                append_mode: None,
            },
            message: AgentMessage::ToolResult {
                tool_call_id: tu.id.clone(),
                tool_name: tool_native_name(tu),
                content: vec![ToolResultContent::Text {
                    text: result.content.clone(),
                    extra: HashMap::new(),
                }],
                details: None,
                is_error: result.is_error,
                timestamp: ts_millis(&turn.timestamp),
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        });
        prev_id = tr_id;
    }
}

fn emit_system(turn: &Turn, entries: &mut Vec<Entry>) {
    if let Some(rest) = turn.text.strip_prefix("Compacted (summary): ") {
        entries.push(Entry::Compaction {
            base: base_for(turn),
            summary: rest.to_string(),
            first_kept_entry_id: String::new(),
            tokens_before: 0,
            details: None,
            from_hook: None,
            extra: HashMap::new(),
        });
    } else if let Some(rest) = turn.text.strip_prefix("Branch summary: ") {
        entries.push(Entry::BranchSummary {
            base: base_for(turn),
            from_id: "root".to_string(),
            summary: rest.to_string(),
            details: None,
            from_hook: None,
            extra: HashMap::new(),
        });
    } else {
        emit_custom_message(turn, "system", entries);
    }
}

fn emit_bash(turn: &Turn, entries: &mut Vec<Entry>) {
    let (command, output) = match turn.text.strip_prefix("$ ") {
        Some(rest) => match rest.split_once('\n') {
            Some((cmd, out)) => (cmd.to_string(), out.to_string()),
            None => (rest.to_string(), String::new()),
        },
        None => (String::new(), turn.text.clone()),
    };
    entries.push(Entry::Message {
        base: base_for(turn),
        message: AgentMessage::BashExecution {
            command,
            output,
            exit_code: Some(0),
            timestamp: ts_millis(&turn.timestamp),
            extra: HashMap::new(),
        },
        extra: HashMap::new(),
    });
}

fn emit_custom_message(turn: &Turn, custom_type: &str, entries: &mut Vec<Entry>) {
    entries.push(Entry::CustomMessage {
        base: base_for(turn),
        custom_type: custom_type.to_string(),
        content: MessageContent::Text(turn.text.clone()),
        display: true,
        details: None,
        extra: HashMap::new(),
    });
}

// ── Helpers ──────────────────────────────────────────────────────────

fn base_for(turn: &Turn) -> EntryBase {
    EntryBase {
        id: turn.id.clone(),
        parent_id: turn.parent_id.clone(),
        timestamp: turn.timestamp.clone(),
        append_mode: None,
    }
}

fn ts_millis(rfc3339: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|dt| dt.timestamp_millis().max(0) as u64)
        .unwrap_or(0)
}

fn build_usage(turn: &Turn) -> Usage {
    let (input, output, cache_read, cache_write) = turn
        .token_usage
        .as_ref()
        .map(|u| {
            (
                u.input_tokens.unwrap_or(0) as u64,
                u.output_tokens.unwrap_or(0) as u64,
                u.cache_read_tokens.unwrap_or(0) as u64,
                u.cache_write_tokens.unwrap_or(0) as u64,
            )
        })
        .unwrap_or((0, 0, 0, 0));
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input + output,
        cost: CostBreakdown::default(),
    }
}

fn parse_stop_reason(turn_stop: Option<&str>) -> StopReason {
    let s = turn_stop.unwrap_or("stop");
    serde_json::from_value::<StopReason>(json!(s))
        .unwrap_or(StopReason::Known(KnownStopReason::Stop))
}

/// Route a tool name through `native_name` when its category is known; else
/// pass through verbatim (OpenClaw's format accepts any tool name).
fn tool_native_name(tu: &ToolInvocation) -> String {
    if let Some(cat) = tu.category
        && let Some(remap) = crate::provider::native_name(cat, &tu.input)
    {
        return remap.to_string();
    }
    tu.name.clone()
}

fn write_private(path: &Path, bytes: &[u8]) -> crate::error::Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::session_to_view;
    use crate::reader::read_session_from_file;
    use std::path::Path as FsPath;
    use toolpath_convo::{TokenUsage, ToolCategory, ToolInvocation, ToolResult};

    fn user_turn(id: &str, text: &str) -> Turn {
        Turn {
            id: id.into(),
            parent_id: None,
            group_id: None,
            role: Role::User,
            timestamp: "2026-06-30T10:00:00Z".into(),
            text: text.into(),
            thinking: None,
            tool_uses: vec![],
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: Vec::new(),
        }
    }

    fn view_with(turns: Vec<Turn>) -> ConversationView {
        ConversationView {
            id: "sess-1".into(),
            turns,
            provider_id: Some("openclaw".into()),
            ..Default::default()
        }
    }

    #[test]
    fn empty_view_projects_header_only() {
        let s = OpenClawProjector::default().project(&view_with(vec![])).unwrap();
        assert_eq!(s.header.id, "sess-1");
        assert_eq!(s.entries.len(), 1);
        assert!(matches!(s.entries[0], Entry::Session(_)));
    }

    #[test]
    fn user_turn_becomes_user_message() {
        let s = OpenClawProjector::default()
            .project(&view_with(vec![user_turn("u1", "hello")]))
            .unwrap();
        // header + user + trailing leaf
        assert!(matches!(&s.entries[1], Entry::Message { message: AgentMessage::User { .. }, .. }));
        assert!(matches!(s.entries.last().unwrap(), Entry::Leaf { .. }));
    }

    #[test]
    fn assistant_tool_call_and_result_split() {
        let mut t = user_turn("a1", "Reading.");
        t.role = Role::Assistant;
        t.model = Some("claude-x".into());
        t.token_usage = Some(TokenUsage {
            input_tokens: Some(10),
            output_tokens: Some(5),
            ..Default::default()
        });
        t.tool_uses = vec![ToolInvocation {
            id: "tc1".into(),
            name: "Read".into(),
            input: json!({"path": "x"}),
            result: Some(ToolResult {
                content: "body".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileRead),
        }];
        let s = OpenClawProjector::default().project(&view_with(vec![t])).unwrap();
        // assistant: foreign "Read" → native "read_file"
        match &s.entries[1] {
            Entry::Message { message: AgentMessage::Assistant { content, usage, .. }, .. } => {
                assert!(matches!(&content[1], ContentBlock::ToolCall { name, .. } if name == "read_file"));
                assert_eq!(usage.total_tokens, 15);
            }
            other => panic!("expected assistant, got {other:?}"),
        }
        // separate toolResult linked by id
        match &s.entries[2] {
            Entry::Message { message: AgentMessage::ToolResult { tool_call_id, tool_name, .. }, .. } => {
                assert_eq!(tool_call_id, "tc1");
                assert_eq!(tool_name, "read_file");
            }
            other => panic!("expected toolResult, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_preserves_messages_tools_usage() {
        let src = read_session_from_file(FsPath::new("tests/fixtures/dm_session.jsonl")).unwrap();
        let view = session_to_view(&src);
        let projected = OpenClawProjector::default().project(&view).unwrap();
        assert_eq!(projected.header.version, 3);

        // Every entry serializes to a single line and re-parses as an Entry.
        for entry in &projected.entries {
            let line = serde_json::to_string(entry).unwrap();
            assert!(!line.contains('\n'));
            let _: Entry = serde_json::from_str(&line).unwrap();
        }

        // A toolCall and its matching toolResult both survive with the same id.
        let mut call_ids = std::collections::HashSet::new();
        let mut result_ids = std::collections::HashSet::new();
        for e in &projected.entries {
            if let Entry::Message { message, .. } = e {
                match message {
                    AgentMessage::Assistant { content, .. } => {
                        for b in content {
                            if let ContentBlock::ToolCall { id, .. } = b {
                                call_ids.insert(id.clone());
                            }
                        }
                    }
                    AgentMessage::ToolResult { tool_call_id, .. } => {
                        result_ids.insert(tool_call_id.clone());
                    }
                    _ => {}
                }
            }
        }
        assert!(call_ids.contains("call_1") && result_ids.contains("call_1"));
        assert!(call_ids.contains("call_2") && result_ids.contains("call_2"));
    }

    #[test]
    fn inception_writes_session_and_routing_entry() {
        let src = read_session_from_file(FsPath::new("tests/fixtures/dm_session.jsonl")).unwrap();
        let view = session_to_view(&src);
        let tmp = tempfile::tempdir().unwrap();
        let proj = OpenClawProjector::default()
            .with_channel("whatsapp")
            .with_peer("15555550123");
        let session = proj.project(&view).unwrap();
        let path = proj.write_session(&session, tmp.path()).unwrap();
        assert!(path.exists());
        assert_eq!(
            path,
            tmp.path().join("agents/main/sessions/sess-abc.jsonl")
        );

        // sessions.json got a whatsapp routing entry pointing at the file.
        let idx = SessionsIndex::load(&tmp.path().join("agents/main/sessions")).unwrap();
        let (key, parsed) = idx.routing_key_for("sess-abc").unwrap();
        assert!(key.contains("whatsapp"));
        assert_eq!(parsed.peer_id.as_deref(), Some("15555550123"));

        // The written transcript re-reads cleanly.
        let reread = read_session_from_file(&path).unwrap();
        assert_eq!(reread.header.id, "sess-abc");
    }
}
