//! Claude Code's resume loader, modelled.
//!
//! The rules, each one a behaviour a real resume depended on:
//!
//! 1. **Leaf.** The history ends at the last conversation entry (`type` `user` or `assistant`,
//!    not a sidechain) in file order.
//! 2. **Walk.** From the leaf, follow `parentUuid` to the root. Only entries on that chain are
//!    sent; an entry the walk never reaches is lost history (pi → claude, 2026-10-15: 10 of 11
//!    assistant lines had `parentUuid: null`, so the model saw only the newest message).
//!    Non-conversation entries on the chain (`permission-mode`, unknown types) are walked through
//!    and not sent.
//! 3. **Roles.** A `user` entry is a user message whatever its `message.role` (a projected
//!    `"system"` turn is sent as user text).
//! 4. **Merge.** Consecutive user entries become one user message. Assistant entries that share a
//!    `message.id` become one assistant message even when tool results sit between them; inside
//!    it, `tool_use` blocks go last and the tool results follow as the next user message.
//! 5. **Parallel tool results.** When one assistant entry makes several tool calls, their result
//!    entries are siblings under it and the walk passes through only the last; the loader brings
//!    the others back right after the assistant.
//!
//! What the rules were checked against: rules 1–4, the committed cassette
//! `toolpath-webmock/cassettes/claude/9d1356…json`, Claude Code 2.1.285 resuming the codex
//! golden's projection (`tests/claude.rs::agrees_with_what_claude_code_sent`). Rule 5 is
//! inferred, not recorded: copilot → claude, whose projection has a two-call turn, resumed and
//! answered in the 2026-10-15 matrix, which the Messages API would refuse with a tool call left
//! unanswered. A cassette of that resume would make it a checked rule.

use serde_json::Value;

use crate::{Block, Message, Resumed, Role, content_text};

/// Resume a Claude Code session file (JSONL) the way Claude Code's loader does.
pub fn resume(jsonl: &str) -> Resumed {
    let entries: Vec<Value> = jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let is_convo = |e: &Value| {
        matches!(
            e.get("type").and_then(Value::as_str),
            Some("user" | "assistant")
        ) && !e
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let uuid = |e: &Value| e.get("uuid").and_then(Value::as_str).map(str::to_owned);

    let by_uuid: std::collections::HashMap<String, &Value> = entries
        .iter()
        .filter_map(|e| uuid(e).map(|u| (u, e)))
        .collect();

    // 1–2: leaf, then the parentUuid walk.
    let mut chain: Vec<&Value> = Vec::new();
    if let Some(leaf) = entries.iter().rev().find(|e| is_convo(e)) {
        let mut seen = std::collections::HashSet::new();
        let mut cur = Some(leaf);
        while let Some(e) = cur {
            if let Some(u) = uuid(e)
                && !seen.insert(u)
            {
                break; // a cycle: Claude Code would stop too
            }
            chain.push(e);
            cur = e
                .get("parentUuid")
                .and_then(Value::as_str)
                .and_then(|p| by_uuid.get(p).copied());
        }
        chain.reverse();
    }
    // 2b: parallel tool calls. Results of one assistant entry's several tool calls are siblings
    // (all children of that assistant); the walk passes through only the last one. The loader
    // brings the other results back right after the assistant, or the Messages API would refuse
    // the unanswered tool_use.
    let on_chain: std::collections::HashSet<String> =
        chain.iter().filter_map(|e| uuid(e)).collect();
    let mut with_siblings: Vec<&Value> = Vec::with_capacity(chain.len());
    for e in chain {
        with_siblings.push(e);
        if e.get("type").and_then(Value::as_str) == Some("assistant")
            && let Some(u) = uuid(e)
        {
            with_siblings.extend(entries.iter().filter(|s| {
                is_convo(s)
                    && s.get("parentUuid").and_then(Value::as_str) == Some(u.as_str())
                    && uuid(s).is_some_and(|su| !on_chain.contains(&su))
                    && is_tool_results(s)
            }));
        }
    }
    let chain = with_siblings;
    let reached: std::collections::HashSet<String> = chain.iter().filter_map(|e| uuid(e)).collect();
    let unreachable = entries
        .iter()
        .filter(|e| is_convo(e))
        .filter_map(uuid)
        .filter(|u| !reached.contains(u))
        .collect();

    // 3–4: messages.
    let mut messages: Vec<Message> = Vec::new();
    // index of the last assistant message and its message.id, while only tool results follow it
    let mut open_assistant: Option<(usize, String)> = None;
    for e in chain.into_iter().filter(|e| is_convo(e)) {
        let msg = e.get("message").cloned().unwrap_or(Value::Null);
        let blocks = blocks_of(&msg);
        if e.get("type").and_then(Value::as_str) == Some("assistant") {
            let mid = msg.get("id").and_then(Value::as_str).map(str::to_owned);
            if let (Some((ai, open_mid)), Some(mid)) = (&open_assistant, &mid)
                && open_mid == mid
            {
                let ai = *ai;
                messages[ai].blocks.extend(blocks);
                tool_uses_last(&mut messages[ai].blocks);
                continue;
            }
            match messages.last_mut() {
                Some(last) if last.role == Role::Assistant && mid.is_none() => {
                    last.blocks.extend(blocks);
                }
                _ => messages.push(Message {
                    role: Role::Assistant,
                    blocks,
                }),
            }
            let ai = messages.len() - 1;
            tool_uses_last(&mut messages[ai].blocks);
            open_assistant = mid.map(|m| (ai, m));
        } else {
            let only_results =
                !blocks.is_empty() && blocks.iter().all(|b| matches!(b, Block::ToolResult { .. }));
            if !only_results {
                open_assistant = None;
            }
            match messages.last_mut() {
                Some(last) if last.role == Role::User => last.blocks.extend(blocks),
                _ => messages.push(Message {
                    role: Role::User,
                    blocks,
                }),
            }
        }
    }

    Resumed {
        messages,
        unreachable,
    }
}

fn is_tool_results(e: &Value) -> bool {
    e.get("type").and_then(Value::as_str) == Some("user")
        && e.pointer("/message/content")
            .and_then(Value::as_array)
            .is_some_and(|a| {
                !a.is_empty()
                    && a.iter()
                        .all(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            })
}

fn tool_uses_last(blocks: &mut [Block]) {
    blocks.sort_by_key(|b| matches!(b, Block::ToolUse { .. }));
}

fn blocks_of(msg: &Value) -> Vec<Block> {
    match msg.get("content") {
        Some(Value::String(s)) => vec![Block::Text(s.clone())],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => Some(Block::Text(str_of(b, "text"))),
                Some("thinking") => Some(Block::Thinking(str_of(b, "thinking"))),
                Some("tool_use") => Some(Block::ToolUse {
                    id: str_of(b, "id"),
                    name: str_of(b, "name"),
                    input: b.get("input").cloned().unwrap_or(Value::Null),
                }),
                Some("tool_result") => Some(Block::ToolResult {
                    tool_use_id: str_of(b, "tool_use_id"),
                    is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    content: content_text(b.get("content").unwrap_or(&Value::Null)),
                }),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn str_of(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: Value) -> String {
        v.to_string()
    }

    #[test]
    fn walks_from_the_leaf_and_reports_what_it_never_reached() {
        let f = [
            line(serde_json::json!({"type":"user","uuid":"u1","message":{"role":"user","content":"first"}})),
            line(serde_json::json!({"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"role":"assistant","content":[{"type":"text","text":"one"}]}})),
            line(serde_json::json!({"type":"user","uuid":"u2","parentUuid":null,"message":{"role":"user","content":"second"}})),
        ]
        .join("\n");
        let r = resume(&f);
        assert_eq!(
            r.messages,
            vec![Message {
                role: Role::User,
                blocks: vec![Block::Text("second".into())]
            }]
        );
        assert_eq!(r.unreachable, vec!["u1".to_string(), "a1".to_string()]);
    }

    #[test]
    fn same_message_id_merges_across_tool_results_with_tool_use_last() {
        let f = [
            line(serde_json::json!({"type":"user","uuid":"u1","message":{"role":"user","content":"go"}})),
            line(serde_json::json!({"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"id":"m","role":"assistant","content":[{"type":"text","text":"t1"},{"type":"tool_use","id":"c","name":"Bash","input":{}}]}})),
            line(serde_json::json!({"type":"user","uuid":"r1","parentUuid":"a1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c","content":"ok"}]}})),
            line(serde_json::json!({"type":"assistant","uuid":"a2","parentUuid":"r1","message":{"id":"m","role":"assistant","content":[{"type":"text","text":"t2"}]}})),
        ]
        .join("\n");
        let r = resume(&f);
        assert!(r.unreachable.is_empty());
        assert_eq!(r.messages.len(), 3);
        assert_eq!(r.messages[1].blocks.len(), 3);
        assert!(matches!(r.messages[1].blocks[2], Block::ToolUse { .. }));
        assert_eq!(r.messages[1].blocks[1], Block::Text("t2".into()));
        assert!(matches!(r.messages[2].blocks[0], Block::ToolResult { .. }));
        assert!(crate::check_api(&r.messages).is_ok());
    }

    #[test]
    fn parallel_tool_results_come_back_with_their_assistant() {
        let f = [
            line(serde_json::json!({"type":"user","uuid":"u1","message":{"role":"user","content":"go"}})),
            line(serde_json::json!({"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"role":"assistant","content":[{"type":"tool_use","id":"c1","name":"Bash","input":{}},{"type":"tool_use","id":"c2","name":"Bash","input":{}}]}})),
            line(serde_json::json!({"type":"user","uuid":"r1","parentUuid":"a1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"one"}]}})),
            line(serde_json::json!({"type":"user","uuid":"r2","parentUuid":"a1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c2","content":"two"}]}})),
            line(serde_json::json!({"type":"assistant","uuid":"a2","parentUuid":"r2","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}})),
        ]
        .join("\n");
        let r = resume(&f);
        assert!(r.unreachable.is_empty(), "{:?}", r.unreachable);
        assert_eq!(r.messages[2].blocks.len(), 2);
        assert!(crate::check_api(&r.messages).is_ok());
    }
}
