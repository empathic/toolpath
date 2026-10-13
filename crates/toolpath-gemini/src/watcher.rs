//! Polling-based watcher for Gemini CLI conversation files.
//!
//! Gemini rewrites the chat JSON on every turn rather than appending,
//! so there's no byte-offset tail to follow. Instead, each `poll()`:
//!
//! 1. Re-parses the main chat file and every sibling sub-agent file.
//! 2. Diffs `messages[]` length against the last-seen per-file state,
//!    emitting `WatcherEvent::Turn` for any new messages.
//! 3. Checks `toolCalls[].status` transitions on previously-emitted
//!    messages and emits `WatcherEvent::TurnUpdated` when a status
//!    flips (e.g. `pending` → `success`).
//! 4. Detects new sub-agent chat files and emits
//!    `WatcherEvent::Progress { kind: "subagent_started" }`; when a
//!    sub-agent's `summary` appears, emits
//!    `WatcherEvent::Progress { kind: "subagent_complete" }`.
//!
//! The watcher is deliberately polling-based in v1 — consumers drive
//! the cadence. A push-based `notify`-powered async variant can layer
//! on top later.

use std::collections::HashMap;

use crate::GeminiConvo;
use crate::error::Result;
use crate::provider::{to_turn, to_view};
use crate::types::{ChatFile, GeminiMessage};
use toolpath_convo::WatcherEvent;

/// Per-file state: how many messages we've emitted and a snapshot of
/// tool-call statuses for cheap diff on the next poll.
#[derive(Debug, Clone, Default)]
struct FileState {
    seen_messages: usize,
    /// `message.id` → (tool_call.id → status)
    tool_statuses: HashMap<String, HashMap<String, String>>,
    summary_emitted: bool,
    existed: bool,
}

/// Watches a Gemini conversation for new turns and tool-call updates.
///
/// # Example
///
/// ```rust,no_run
/// use toolpath_gemini::{GeminiConvo, ConversationWatcher};
/// use toolpath_convo::WatcherEvent;
///
/// let manager = GeminiConvo::new();
/// let mut watcher = ConversationWatcher::new(
///     manager,
///     "/path/to/project".to_string(),
///     "session-uuid".to_string(),
/// );
/// let events = watcher.poll().unwrap();
/// for event in events {
///     match event {
///         WatcherEvent::Turn(t) => println!("new: {}", t.text),
///         WatcherEvent::TurnUpdated(t) => println!("updated: {}", t.id),
///         WatcherEvent::Progress { kind, data } => println!("{}: {}", kind, data),
///     }
/// }
/// ```
#[derive(Debug)]
pub struct ConversationWatcher {
    manager: GeminiConvo,
    project: String,
    session_uuid: String,
    /// Per-chat-file state, keyed by file stem.
    state: HashMap<String, FileState>,
    /// Total number of turns (across all files) we've emitted.
    seen_total: usize,
}

impl ConversationWatcher {
    pub fn new(manager: GeminiConvo, project: String, session_uuid: String) -> Self {
        Self {
            manager,
            project,
            session_uuid,
            state: HashMap::new(),
            seen_total: 0,
        }
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn session_uuid(&self) -> &str {
        &self.session_uuid
    }

    /// Total turns emitted so far across all chat files in this session.
    pub fn seen_count(&self) -> usize {
        self.seen_total
    }

    /// Reset state so the next poll returns all messages from scratch.
    pub fn reset(&mut self) {
        self.state.clear();
        self.seen_total = 0;
    }

    /// Poll for new events.
    ///
    /// On the first call, emits one `Turn` event per existing message,
    /// plus one `subagent_started`/`subagent_complete` per pre-existing
    /// sub-agent file. Subsequent calls emit only new messages, status
    /// updates, and new sub-agent files.
    pub fn poll(&mut self) -> Result<Vec<WatcherEvent>> {
        let mut events: Vec<WatcherEvent> = Vec::new();

        let resolver = self.manager.resolver();
        let chat_stems = resolver.list_chat_files(&self.project, &self.session_uuid)?;

        // If the session dir doesn't exist yet, emit nothing.
        if chat_stems.is_empty() {
            return Ok(events);
        }

        let io = self.manager.io();

        // Determine main vs sub-agent up-front so we can emit stable
        // sub-agent Progress events.
        let mut chats: Vec<(String, ChatFile)> = Vec::with_capacity(chat_stems.len());
        for stem in &chat_stems {
            match io.read_chat(&self.project, &self.session_uuid, stem) {
                Ok(chat) => chats.push((stem.clone(), chat)),
                Err(e) => {
                    eprintln!("Warning: failed to read chat {}: {}", stem, e);
                }
            }
        }

        let main_idx = chats
            .iter()
            .position(|(_, c)| c.kind.as_deref() != Some("subagent"))
            .unwrap_or(0);

        // Process the main chat first so its `Turn`s land before the
        // sub-agents' (matching the batch-load order).
        let mut order: Vec<usize> = (0..chats.len()).collect();
        if main_idx != 0 {
            order.remove(main_idx);
            order.insert(0, main_idx);
        }

        for idx in order {
            let (stem, chat) = &chats[idx];
            let is_subagent = chat.kind.as_deref() == Some("subagent");
            let state = self.state.entry(stem.clone()).or_default();
            let first_time = !state.existed;
            state.existed = true;

            // Emit subagent_started when we see a sub-agent file for
            // the first time.
            if is_subagent && first_time {
                events.push(WatcherEvent::Progress {
                    kind: "subagent_started".into(),
                    data: serde_json::json!({
                        "session_id": chat.session_id,
                        "chat_name": stem,
                    }),
                });
            }

            // Diff message count for new-turn events.
            for (i, msg) in chat.messages.iter().enumerate() {
                if i < state.seen_messages {
                    continue;
                }
                events.push(WatcherEvent::Turn(Box::new(to_turn(msg))));
                state
                    .tool_statuses
                    .insert(msg.id.clone(), snapshot_statuses(msg));
                self.seen_total += 1;
            }

            // Detect status transitions on messages we've already emitted.
            let limit = state.seen_messages.min(chat.messages.len());
            for msg in chat.messages.iter().take(limit) {
                let current = snapshot_statuses(msg);
                let prev = state
                    .tool_statuses
                    .get(&msg.id)
                    .cloned()
                    .unwrap_or_default();
                if current != prev {
                    events.push(WatcherEvent::TurnUpdated(Box::new(to_turn(msg))));
                    state.tool_statuses.insert(msg.id.clone(), current);
                }
            }

            state.seen_messages = chat.messages.len();

            // Sub-agent completion is signalled by `summary` appearing.
            if is_subagent && !state.summary_emitted && chat.summary.is_some() {
                events.push(WatcherEvent::Progress {
                    kind: "subagent_complete".into(),
                    data: serde_json::json!({
                        "session_id": chat.session_id,
                        "chat_name": stem,
                        "summary": chat.summary,
                    }),
                });
                state.summary_emitted = true;
            }
        }

        Ok(events)
    }

    /// Re-read the full session and return a fresh `ConversationView`
    /// along with the new events emitted since the last poll.
    ///
    /// Useful when consumers need both the delta and the full state.
    pub fn poll_with_view(
        &mut self,
    ) -> Result<(toolpath_convo::ConversationView, Vec<WatcherEvent>)> {
        let events = self.poll()?;
        let convo = self
            .manager
            .read_conversation(&self.project, &self.session_uuid)?;
        Ok((to_view(&convo), events))
    }
}

fn snapshot_statuses(msg: &GeminiMessage) -> HashMap<String, String> {
    msg.tool_calls()
        .iter()
        .map(|t| (t.id.clone(), t.status.clone()))
        .collect()
}

// ── toolpath_convo::ConversationWatcher impl ────────────────────────

impl toolpath_convo::ConversationWatcher for ConversationWatcher {
    fn poll(&mut self) -> toolpath_convo::Result<Vec<WatcherEvent>> {
        ConversationWatcher::poll(self)
            .map_err(|e| toolpath_convo::ConvoError::Provider(e.to_string()))
    }

    fn seen_count(&self) -> usize {
        ConversationWatcher::seen_count(self)
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PathResolver;
    use std::fs;
    use tempfile::TempDir;
    use toolpath_convo::{Role, WatcherEvent};

    fn setup() -> (TempDir, GeminiConvo, std::path::PathBuf) {
        let temp = TempDir::new().unwrap();
        let gemini = temp.path().join(".gemini");
        let session_dir = gemini.join("tmp/myrepo/chats/session-uuid");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            gemini.join("projects.json"),
            r#"{"projects":{"/abs/myrepo":"myrepo"}}"#,
        )
        .unwrap();
        let mgr = GeminiConvo::with_resolver(PathResolver::new().with_gemini_dir(&gemini));
        (temp, mgr, session_dir)
    }

    fn write_main(dir: &std::path::Path, body: &str) {
        fs::write(dir.join("main.json"), body).unwrap();
    }

    #[test]
    fn test_poll_empty_session() {
        let (_t, mgr, _dir) = setup();
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "missing".into());
        let events = w.poll().unwrap();
        assert!(events.is_empty());
        assert_eq!(w.seen_count(), 0);
    }

    #[test]
    fn test_poll_first_call_returns_all() {
        let (_t, mgr, dir) = setup();
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"user","content":[{"text":"hi"}]},
  {"id":"m2","timestamp":"ts","type":"gemini","content":"hello","model":"g"}
]}"#,
        );
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let events = w.poll().unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], WatcherEvent::Turn(_)));
        assert!(matches!(events[1], WatcherEvent::Turn(_)));
        assert_eq!(w.seen_count(), 2);
    }

    #[test]
    fn test_poll_second_call_returns_empty_when_idle() {
        let (_t, mgr, dir) = setup();
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"user","content":[{"text":"hi"}]}
]}"#,
        );
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let _ = w.poll().unwrap();
        let events = w.poll().unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_poll_detects_new_messages() {
        let (_t, mgr, dir) = setup();
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"user","content":[{"text":"hi"}]}
]}"#,
        );
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let first = w.poll().unwrap();
        assert_eq!(first.len(), 1);

        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"user","content":[{"text":"hi"}]},
  {"id":"m2","timestamp":"ts","type":"gemini","content":"hello","model":"g"}
]}"#,
        );
        let second = w.poll().unwrap();
        assert_eq!(second.len(), 1);
        match &second[0] {
            WatcherEvent::Turn(t) => assert_eq!(t.text, "hello"),
            other => panic!("expected Turn, got {:?}", std::mem::discriminant(other)),
        }
    }

    #[test]
    fn test_poll_detects_status_transition() {
        let (_t, mgr, dir) = setup();
        // First state: tool call pending.
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"gemini","content":"","toolCalls":[
    {"id":"t1","name":"read_file","args":{},"status":"pending","timestamp":"ts"}
  ]}
]}"#,
        );
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let first = w.poll().unwrap();
        assert_eq!(first.len(), 1);
        assert!(matches!(first[0], WatcherEvent::Turn(_)));

        // Second state: same message, tool call now successful.
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"gemini","content":"","toolCalls":[
    {"id":"t1","name":"read_file","args":{},"status":"success","timestamp":"ts","result":[{"functionResponse":{"id":"t1","name":"read_file","response":{"output":"ok"}}}]}
  ]}
]}"#,
        );
        let second = w.poll().unwrap();
        assert_eq!(second.len(), 1);
        match &second[0] {
            WatcherEvent::TurnUpdated(t) => {
                assert_eq!(t.id, "m1");
                assert_eq!(t.tool_uses[0].result.as_ref().unwrap().content, "ok");
            }
            other => panic!(
                "expected TurnUpdated, got {:?}",
                std::mem::discriminant(other)
            ),
        }
    }

    #[test]
    fn test_poll_emits_subagent_started_and_complete() {
        let (_t, mgr, dir) = setup();
        write_main(&dir, r#"{"sessionId":"m","projectHash":"","messages":[]}"#);
        // First poll — main only, no events (no messages).
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let e1 = w.poll().unwrap();
        assert!(e1.is_empty());

        // Add a sub-agent without summary.
        fs::write(
            dir.join("sub.json"),
            r#"{"sessionId":"subby","projectHash":"","kind":"subagent","messages":[
  {"id":"sx","timestamp":"ts","type":"user","content":[{"text":"go"}]}
]}"#,
        )
        .unwrap();
        let e2 = w.poll().unwrap();
        let kinds: Vec<&str> = e2
            .iter()
            .filter_map(|e| match e {
                WatcherEvent::Progress { kind, .. } => Some(kind.as_str()),
                _ => None,
            })
            .collect();
        assert!(kinds.contains(&"subagent_started"));
        assert!(!kinds.contains(&"subagent_complete"));

        // Now write a summary.
        fs::write(
            dir.join("sub.json"),
            r#"{"sessionId":"subby","projectHash":"","kind":"subagent","summary":"done","messages":[
  {"id":"sx","timestamp":"ts","type":"user","content":[{"text":"go"}]}
]}"#,
        )
        .unwrap();
        let e3 = w.poll().unwrap();
        let kinds: Vec<&str> = e3
            .iter()
            .filter_map(|e| match e {
                WatcherEvent::Progress { kind, .. } => Some(kind.as_str()),
                _ => None,
            })
            .collect();
        assert!(kinds.contains(&"subagent_complete"));
    }

    #[test]
    fn test_poll_preserves_role() {
        let (_t, mgr, dir) = setup();
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"u","timestamp":"ts","type":"user","content":[{"text":"hi"}]},
  {"id":"a","timestamp":"ts","type":"gemini","content":"hey"}
]}"#,
        );
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let events = w.poll().unwrap();
        let roles: Vec<&Role> = events
            .iter()
            .filter_map(|e| e.as_turn().map(|t| &t.role))
            .collect();
        assert_eq!(roles, vec![&Role::User, &Role::Assistant]);
    }

    #[test]
    fn test_reset_re_emits_all() {
        let (_t, mgr, dir) = setup();
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"user","content":[{"text":"hi"}]}
]}"#,
        );
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let _ = w.poll().unwrap();
        w.reset();
        let re = w.poll().unwrap();
        assert_eq!(re.len(), 1);
    }

    #[test]
    fn test_poll_with_view_returns_full_conversation() {
        let (_t, mgr, dir) = setup();
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"user","content":[{"text":"hi"}]}
]}"#,
        );
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let (view, events) = w.poll_with_view().unwrap();
        assert_eq!(view.turns().count(), 1);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_trait_impl() {
        let (_t, mgr, dir) = setup();
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"user","content":[{"text":"hi"}]}
]}"#,
        );
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let events = toolpath_convo::ConversationWatcher::poll(&mut w).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(toolpath_convo::ConversationWatcher::seen_count(&w), 1);
    }

    // ── Accessors ────────────────────────────────────────────────────

    #[test]
    fn test_project_and_session_accessors() {
        let (_t, mgr, _dir) = setup();
        let w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        assert_eq!(w.project(), "/abs/myrepo");
        assert_eq!(w.session_uuid(), "session-uuid");
    }

    // ── Additional status transitions ───────────────────────────────

    #[test]
    fn test_poll_detects_status_transition_to_cancelled() {
        let (_t, mgr, dir) = setup();
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"gemini","content":"","toolCalls":[
    {"id":"t1","name":"run_shell_command","args":{},"status":"pending","timestamp":"ts"}
  ]}
]}"#,
        );
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let _ = w.poll().unwrap();

        // Status flips to cancelled — must still produce TurnUpdated.
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"gemini","content":"","toolCalls":[
    {"id":"t1","name":"run_shell_command","args":{},"status":"cancelled","timestamp":"ts"}
  ]}
]}"#,
        );
        let events = w.poll().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], WatcherEvent::TurnUpdated(_)));
    }

    #[test]
    fn test_poll_no_event_when_status_unchanged() {
        let (_t, mgr, dir) = setup();
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"gemini","content":"done","toolCalls":[
    {"id":"t1","name":"read_file","args":{},"status":"success","timestamp":"ts"}
  ]}
]}"#,
        );
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let _ = w.poll().unwrap();

        // Same file, same content — should produce nothing.
        let events = w.poll().unwrap();
        assert!(events.is_empty());
    }

    // ── Sub-agent timing edge cases ─────────────────────────────────

    #[test]
    fn test_subagent_added_after_non_empty_main() {
        let (_t, mgr, dir) = setup();
        write_main(
            &dir,
            r#"{"sessionId":"m","projectHash":"","messages":[
  {"id":"u1","timestamp":"ts","type":"user","content":[{"text":"hi"}]}
]}"#,
        );
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let events = w.poll().unwrap();
        assert_eq!(events.len(), 1);
        // No progress events yet — no sub-agent file exists.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, WatcherEvent::Progress { .. }))
        );

        // Now a sub-agent appears.
        fs::write(
            dir.join("helper.json"),
            r#"{"sessionId":"helper","projectHash":"","kind":"subagent","messages":[
  {"id":"h1","timestamp":"ts","type":"user","content":[{"text":"search"}]}
]}"#,
        )
        .unwrap();
        let events = w.poll().unwrap();
        let has_started = events.iter().any(
            |e| matches!(e, WatcherEvent::Progress { kind, .. } if kind == "subagent_started"),
        );
        let has_turn = events.iter().any(|e| matches!(e, WatcherEvent::Turn(_)));
        assert!(
            has_started,
            "expected subagent_started, got {:?}",
            events.len()
        );
        assert!(has_turn, "expected the sub-agent's first turn");
    }

    #[test]
    fn test_subagent_complete_emitted_separately_from_started() {
        // When a sub-agent file is created without `summary` and then
        // `summary` is added, the two Progress events should fire in
        // separate polls.
        let (_t, mgr, dir) = setup();
        write_main(&dir, r#"{"sessionId":"m","projectHash":"","messages":[]}"#);
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let _ = w.poll().unwrap();

        fs::write(
            dir.join("sub.json"),
            r#"{"sessionId":"s","projectHash":"","kind":"subagent","messages":[]}"#,
        )
        .unwrap();
        let e1 = w.poll().unwrap();
        let started_count = e1
            .iter()
            .filter(
                |e| matches!(e, WatcherEvent::Progress { kind, .. } if kind == "subagent_started"),
            )
            .count();
        let complete_count = e1
            .iter()
            .filter(
                |e| matches!(e, WatcherEvent::Progress { kind, .. } if kind == "subagent_complete"),
            )
            .count();
        assert_eq!(started_count, 1);
        assert_eq!(complete_count, 0);

        // Add a summary.
        fs::write(
            dir.join("sub.json"),
            r#"{"sessionId":"s","projectHash":"","kind":"subagent","summary":"done","messages":[]}"#,
        )
        .unwrap();
        let e2 = w.poll().unwrap();
        // Complete fires, started does NOT fire again (we've seen the file).
        let started_count = e2
            .iter()
            .filter(
                |e| matches!(e, WatcherEvent::Progress { kind, .. } if kind == "subagent_started"),
            )
            .count();
        let complete_count = e2
            .iter()
            .filter(
                |e| matches!(e, WatcherEvent::Progress { kind, .. } if kind == "subagent_complete"),
            )
            .count();
        assert_eq!(started_count, 0);
        assert_eq!(complete_count, 1);
    }

    // ── Unknown role survives watcher path ─────────────────────────

    #[test]
    fn test_poll_preserves_unknown_role() {
        let (_t, mgr, dir) = setup();
        write_main(
            &dir,
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"plan","content":"planning..."}
]}"#,
        );
        let mut w = ConversationWatcher::new(mgr, "/abs/myrepo".into(), "session-uuid".into());
        let events = w.poll().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            WatcherEvent::Turn(t) => {
                // `plan` maps to Role::Other("plan")
                assert!(matches!(t.role, Role::Other(ref s) if s == "plan"));
            }
            other => panic!("expected Turn, got {:?}", std::mem::discriminant(other)),
        }
    }
}
