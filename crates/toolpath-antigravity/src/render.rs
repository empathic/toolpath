//! Route 1 of moving a thread into Antigravity (per
//! `docs/agents/formats/antigravity.md`'s "cheaper routes" note and
//! Bobby's reordering: try this and the SDK's `initial_trajectory`
//! *before* building a full store projector): render a
//! [`ConversationView`] as plain text and seed it as the first
//! `--input-format stream-json` user event, so `agy` starts a brand new,
//! self-written conversation whose first turn happens to recap an
//! earlier one — no store-schema dependency, so this is version-proof by
//! construction (every byte `agy` holds afterwards is one `agy` itself
//! wrote).
//!
//! Lossy on purpose: tool calls become readable lines, not replayable
//! records, and nothing here lets the resuming model actually re-invoke
//! a tool from the old thread. Whether that's "enough" context for
//! Bryan's use case ("move a thread into Antigravity") is exactly what
//! the live check (step 7 of `docs/agents/adding-a-projector.md`, not run
//! before 13:15 UTC) decides — see the task report.

use serde_json::json;
use toolpath_convo::{ConversationView, Item, Role};

/// Render every turn of `view` as a flat, human-readable transcript:
/// `"User: …"` / `"Assistant: …"`, with tool calls as a nested `  ->`
/// line (name, a short arg summary, and a truncated result). No attempt
/// to preserve exact formatting — this is a recap for the model to read,
/// not data to parse back.
pub fn render_transcript_text(view: &ConversationView) -> String {
    let mut out = String::new();
    for item in &view.items {
        let Item::Turn(turn) = item else { continue };
        let who = match turn.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
            Role::Other(ref s) => s.as_str(),
        };
        if !turn.text.trim().is_empty() {
            out.push_str(who);
            out.push_str(": ");
            out.push_str(turn.text.trim());
            out.push('\n');
        }
        for tool in &turn.tool_uses {
            out.push_str("  -> ");
            out.push_str(&tool.name);
            out.push('(');
            out.push_str(&summarize_args(&tool.input));
            out.push(')');
            if let Some(result) = &tool.result {
                out.push_str(" => ");
                out.push_str(&truncate(&result.content, 200));
                if result.is_error {
                    out.push_str(" [error]");
                }
            }
            out.push('\n');
        }
    }
    out
}

fn summarize_args(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => map
            .iter()
            .take(3)
            .map(|(k, v)| format!("{k}={}", truncate(&v.to_string(), 60)))
            .collect::<Vec<_>>()
            .join(", "),
        other => truncate(&other.to_string(), 60),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// NDJSON lines for `agy --input-format stream-json --output-format
/// stream-json`: the rendered transcript as the first user event's
/// content, then `continuation_prompt` as a second, real turn. Each
/// returned string is one line (no trailing newline — join with `\n`
/// when writing, or write each with `println!`).
pub fn stream_json_seed_events(view: &ConversationView, continuation_prompt: &str) -> Vec<String> {
    let recap = render_transcript_text(view);
    let seed = format!(
        "Here is a recap of an earlier conversation. Treat it as prior \
         context, not as something to act on again:\n\n{recap}"
    );
    vec![
        json!({"event": "user", "message": {"content": seed}}).to_string(),
        json!({"event": "user", "message": {"content": continuation_prompt}}).to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use toolpath_convo::{ProducerInfo, Role, ToolCategory, ToolInvocation, ToolResult, Turn};

    fn turn(role: Role, text: &str, tool_uses: Vec<ToolInvocation>) -> Item {
        Item::Turn(Turn {
            id: "t".into(),
            parent_id: None,
            group_id: None,
            role,
            timestamp: String::new(),
            text: text.to_string(),
            thinking: None,
            thinking_signature: None,
            tool_uses,
            model: None,
            provider: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: vec![],
        })
    }

    fn view(items: Vec<Item>) -> ConversationView {
        ConversationView {
            id: "v".into(),
            started_at: None,
            last_activity: None,
            items,
            total_usage: None,
            provider_id: None,
            files_changed: vec![],
            session_ids: vec![],
            base: None,
            producer: Some(ProducerInfo {
                name: "test".into(),
                version: None,
            }),
        }
    }

    #[test]
    fn renders_user_and_assistant_lines() {
        let v = view(vec![
            turn(Role::User, "hi there", vec![]),
            turn(Role::Assistant, "hello back", vec![]),
        ]);
        let text = render_transcript_text(&v);
        assert_eq!(text, "User: hi there\nAssistant: hello back\n");
    }

    #[test]
    fn renders_a_tool_call_with_args_and_result() {
        let tool = ToolInvocation {
            id: "c1".into(),
            name: "read_file".into(),
            input: json!({"Path": "/tmp/notes.txt"}),
            result: Some(ToolResult {
                content: "the magic number is 42".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileRead),
        };
        let v = view(vec![turn(Role::Assistant, "42", vec![tool])]);
        let text = render_transcript_text(&v);
        assert!(text.contains("-> read_file(Path=\"/tmp/notes.txt\")"));
        assert!(text.contains("=> the magic number is 42"));
    }

    #[test]
    fn marks_an_error_result() {
        let tool = ToolInvocation {
            id: "c1".into(),
            name: "read_file".into(),
            input: json!({}),
            result: Some(ToolResult {
                content: "not found".into(),
                is_error: true,
            }),
            category: Some(ToolCategory::FileRead),
        };
        let v = view(vec![turn(Role::Assistant, "", vec![tool])]);
        assert!(render_transcript_text(&v).contains("[error]"));
    }

    #[test]
    fn stream_json_seed_produces_two_valid_ndjson_lines() {
        let v = view(vec![turn(Role::User, "hi", vec![])]);
        let lines = stream_json_seed_events(&v, "continue where we left off");
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(parsed["event"], "user");
            assert!(parsed["message"]["content"].as_str().is_some());
        }
        assert!(lines[0].contains("recap"));
        assert!(lines[1].contains("continue where we left off"));
    }

    #[test]
    fn empty_view_renders_empty_transcript() {
        assert_eq!(render_transcript_text(&view(vec![])), "");
    }
}
