//! A mock harness: what a harness's own loader would send to the model as history when it
//! resumes a session file, computed without the harness binary.
//!
//! `toolpath-webmock` stands in for the model API; this crate stands in for the agent. A test can
//! then say "projected for Claude Code, Claude Code would send these messages" in plain
//! `cargo test`, where today only a live resume (the resume matrix) can say it.
//!
//! Each harness module models the loader's rules that resume bugs have actually depended on, and
//! nothing more. The model is kept honest by a contract test: for a committed webmock cassette
//! recorded from the real binary resuming a projected session, the mock's messages must be a
//! prefix of the request body the binary really sent (`tests/claude.rs`). See
//! `docs/mock-harness.md`.

pub mod claude;

use serde_json::Value;

/// Who a message is from, as the model API sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One content block of a message, reduced to what the comparison needs.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Text(String),
    Thinking(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        is_error: bool,
        content: String,
    },
}

/// One message of the history the harness would send.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
}

/// What resuming a session file gives: the history sent, and the conversation entries in the file
/// the loader never reached (a non-empty list is a lost-history resume).
#[derive(Debug, Clone, PartialEq)]
pub struct Resumed {
    pub messages: Vec<Message>,
    pub unreachable: Vec<String>,
}

/// Why the model API would refuse the resumed history (the copilot → claude 400).
#[derive(Debug, Clone, PartialEq)]
pub struct Rejected {
    pub message_index: usize,
    pub reason: String,
}

/// The model API's own rules on a history, independent of any harness.
pub fn check_api(messages: &[Message]) -> Result<(), Rejected> {
    for (i, m) in messages.iter().enumerate() {
        // Every tool call is answered by a tool_result in the next message (the last message may
        // end on a tool call: the harness answers it before sending).
        if m.role == Role::Assistant
            && let Some(next) = messages.get(i + 1)
        {
            for b in &m.blocks {
                if let Block::ToolUse { id, .. } = b {
                    let answered = next.blocks.iter().any(
                        |nb| matches!(nb, Block::ToolResult { tool_use_id, .. } if tool_use_id == id),
                    );
                    if !answered {
                        return Err(Rejected {
                            message_index: i,
                            reason: format!(
                                "messages.{i}: tool_use {id} has no tool_result in the next message"
                            ),
                        });
                    }
                }
            }
        }
        for b in &m.blocks {
            if let Block::ToolResult {
                tool_use_id,
                is_error,
                content,
            } = b
            {
                if *is_error && content.trim().is_empty() {
                    return Err(Rejected {
                        message_index: i,
                        reason: format!(
                            "messages.{i}: tool_result {tool_use_id}: content cannot be empty if is_error is true"
                        ),
                    });
                }
                let prev_uses = i
                    .checked_sub(1)
                    .and_then(|p| messages.get(p))
                    .filter(|p| p.role == Role::Assistant)
                    .map(|p| {
                        p.blocks
                            .iter()
                            .any(|pb| matches!(pb, Block::ToolUse { id, .. } if id == tool_use_id))
                    })
                    .unwrap_or(false);
                if !prev_uses {
                    return Err(Rejected {
                        message_index: i,
                        reason: format!(
                            "messages.{i}: tool_result {tool_use_id} has no matching tool_use in the previous message"
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

/// The text of a tool_result's `content`, string or block array.
pub(crate) fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(id: &str, err: bool, c: &str) -> Block {
        Block::ToolResult {
            tool_use_id: id.into(),
            is_error: err,
            content: c.into(),
        }
    }
    fn tu(id: &str) -> Block {
        Block::ToolUse {
            id: id.into(),
            name: "Bash".into(),
            input: Value::Null,
        }
    }

    #[test]
    fn an_errored_tool_result_must_not_be_empty() {
        let h = vec![
            Message {
                role: Role::Assistant,
                blocks: vec![tu("a")],
            },
            Message {
                role: Role::User,
                blocks: vec![tr("a", true, "")],
            },
        ];
        let e = check_api(&h).unwrap_err();
        assert_eq!(e.message_index, 1);
        assert!(e.reason.contains("cannot be empty"));
    }

    #[test]
    fn a_tool_result_needs_its_tool_use_in_the_previous_message() {
        let h = vec![
            Message {
                role: Role::Assistant,
                blocks: vec![tu("a")],
            },
            Message {
                role: Role::User,
                blocks: vec![tr("b", false, "x")],
            },
        ];
        let e = check_api(&h).unwrap_err();
        assert!(
            e.reason.contains("tool_use a has no tool_result"),
            "{}",
            e.reason
        );
        let orphan = vec![Message {
            role: Role::User,
            blocks: vec![tr("b", false, "x")],
        }];
        assert!(
            check_api(&orphan)
                .unwrap_err()
                .reason
                .contains("no matching tool_use")
        );
        let ok = vec![
            Message {
                role: Role::Assistant,
                blocks: vec![tu("a")],
            },
            Message {
                role: Role::User,
                blocks: vec![tr("a", false, "x")],
            },
        ];
        assert!(check_api(&ok).is_ok());
    }
}
