//! Tag lines: a message in the conversation that labels the message before it.
//!
//! A user turn whose whole text is one line of the form
//!
//! ```text
//! ptag: <tag> [<tag>…]
//! ```
//!
//! is a *tag step*. Its tags are attached to the nearest preceding
//! `conversation.append` step on its ancestry — the final step of the message
//! the person had just read — under `meta.tags`, and the tag step itself
//! records what it did under `meta.extra["tag"]` (`{"tags": […], "target":
//! "<step id>"}`) so a consumer can hide or follow it.
//!
//! Tags are separated by whitespace or commas and are otherwise opaque:
//! `bug:auth` is one tag. Duplicates within a line, or already present on
//! the target, are dropped; order is kept.
//!
//! The same line is recognised inside a `conversation.event` step's text —
//! the shape a harness leaves when a hook intercepts the prompt before the
//! model sees it (Claude Code's `UserPromptSubmit` hook, for example, records
//! a system entry whose content carries the hook's reason and the original
//! prompt). The event's `text` / `content` (top-level or under `entry_extra`)
//! is scanned line by line for a tag line. Nothing in this module is
//! provider-specific; every provider that goes through [`derive_path`]
//! gets it.
//!
//! [`derive_path`]: crate::derive_path

use std::collections::HashMap;

use toolpath::v1::Step;

/// The prefix that makes a line a tag line.
pub const TAG_PREFIX: &str = "ptag:";

/// Parse `text` as a tag line. `Some(tags)` when the trimmed text is a single
/// line starting with [`TAG_PREFIX`] and naming at least one tag; the tags
/// come back in order, deduplicated. `None` for anything else, including a
/// multi-line message that happens to start with `ptag:` — a tag line is the
/// whole message, never its first line.
pub fn parse_tag_line(text: &str) -> Option<Vec<String>> {
    let line = text.trim();
    if line.contains('\n') {
        return None;
    }
    let rest = line.strip_prefix(TAG_PREFIX)?;
    let mut tags: Vec<String> = Vec::new();
    for tok in rest.split(|c: char| c.is_whitespace() || c == ',') {
        if tok.is_empty() || tags.iter().any(|t| t == tok) {
            continue;
        }
        tags.push(tok.to_string());
    }
    if tags.is_empty() { None } else { Some(tags) }
}

/// The tags a step carries *as a tag step* — `None` when the step is not one.
pub fn tags_of_tag_step(step: &Step) -> Option<Vec<String>> {
    for change in step.change.values() {
        let Some(s) = &change.structural else {
            continue;
        };
        match s.change_type.as_str() {
            "conversation.append" => {
                if s.extra.get("role").and_then(|v| v.as_str()) != Some("user") {
                    continue;
                }
                if let Some(text) = s.extra.get("text").and_then(|v| v.as_str())
                    && let Some(tags) = parse_tag_line(text)
                {
                    return Some(tags);
                }
            }
            "conversation.event" => {
                for text in event_texts(&s.extra) {
                    for line in text.lines() {
                        if let Some(tags) = parse_tag_line(line) {
                            return Some(tags);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The string fields of an event that may carry a prompt: `text` and
/// `content`, at the top level or under the provider's `entry_extra`.
fn event_texts(extra: &HashMap<String, serde_json::Value>) -> Vec<&str> {
    let mut out = Vec::new();
    for key in ["text", "content"] {
        if let Some(s) = extra.get(key).and_then(|v| v.as_str()) {
            out.push(s);
        }
        if let Some(s) = extra
            .get("entry_extra")
            .and_then(|v| v.get(key))
            .and_then(|v| v.as_str())
        {
            out.push(s);
        }
    }
    out
}

fn is_conversation_append(step: &Step) -> bool {
    step.change.values().any(|c| {
        c.structural
            .as_ref()
            .is_some_and(|s| s.change_type == "conversation.append")
    })
}

/// Attach every tag step's tags to its target. `steps` is a path's step list
/// with parent references already resolved to step ids; the walk follows the
/// first parent and skips events and other tag steps, so two tag lines in a
/// row both land on the same message.
pub fn apply_tags(steps: &mut [Step]) {
    let index: HashMap<String, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.step.id.clone(), i))
        .collect();

    let mut found: Vec<(usize, Option<usize>, Vec<String>)> = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let Some(tags) = tags_of_tag_step(step) else {
            continue;
        };
        found.push((i, find_target(steps, &index, i), tags));
    }

    for (i, target, tags) in found {
        let mut marker = serde_json::Map::new();
        marker.insert("tags".to_string(), serde_json::json!(tags));
        if let Some(t) = target {
            marker.insert(
                "target".to_string(),
                serde_json::Value::String(steps[t].step.id.clone()),
            );
        }
        steps[i]
            .meta
            .get_or_insert_with(Default::default)
            .extra
            .insert("tag".to_string(), serde_json::Value::Object(marker));

        if let Some(t) = target {
            let meta = steps[t].meta.get_or_insert_with(Default::default);
            for tag in tags {
                if !meta.tags.contains(&tag) {
                    meta.tags.push(tag);
                }
            }
        }
    }
}

fn find_target(steps: &[Step], index: &HashMap<String, usize>, from: usize) -> Option<usize> {
    let mut cur = from;
    for _ in 0..steps.len() {
        let parent = steps[cur].step.parents.first()?;
        let &pi = index.get(parent)?;
        if is_conversation_append(&steps[pi]) && tags_of_tag_step(&steps[pi]).is_none() {
            return Some(pi);
        }
        cur = pi;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_space_and_comma_separated_tags() {
        assert_eq!(
            parse_tag_line("ptag: decision auth"),
            Some(vec!["decision".to_string(), "auth".to_string()])
        );
        assert_eq!(
            parse_tag_line("ptag:decision,auth, later"),
            Some(vec![
                "decision".to_string(),
                "auth".to_string(),
                "later".to_string()
            ])
        );
        assert_eq!(
            parse_tag_line("  ptag: bug:auth \n"),
            Some(vec!["bug:auth".to_string()])
        );
    }

    #[test]
    fn dedupes_within_a_line() {
        assert_eq!(
            parse_tag_line("ptag: a b a"),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn rejects_non_tag_lines() {
        assert_eq!(parse_tag_line("ptag:"), None);
        assert_eq!(parse_tag_line("ptag: , ,"), None);
        assert_eq!(parse_tag_line("tag: a"), None);
        assert_eq!(parse_tag_line("Ptag: a"), None);
        assert_eq!(parse_tag_line("please ptag: a"), None);
        assert_eq!(parse_tag_line("ptag: a\nand some prose"), None);
        assert_eq!(parse_tag_line(""), None);
    }
}
