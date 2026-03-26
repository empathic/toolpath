//! Data types and summary generation for the trace TUI.

use std::collections::{HashMap, HashSet};

use toolpath::v1::{self, query};

/// Display-oriented wrapper around a toolpath Step.
pub struct StepEntry {
    pub index: usize,
    pub step: v1::Step,
    /// One-line collapsed display.
    pub summary: String,
    /// Actor label: "user", "assistant", "policy", or the raw actor string.
    pub actor_label: String,
    /// Whether this step is included in output (default: true).
    pub included: bool,
    /// Whether the step detail is expanded.
    pub expanded: bool,
    /// Per-field text redactions within this step.
    pub text_redactions: Vec<TextRedaction>,
}

/// A text redaction within a step's JSON.
pub struct TextRedaction {
    /// JSON Pointer (RFC 6901) to the string value within the step JSON.
    pub json_pointer: String,
    /// Byte offset start within the string value.
    pub start: usize,
    /// Byte offset end within the string value.
    pub end: usize,
    /// The original text that was replaced (for undo).
    pub original: String,
}

/// Selection mode for the TUI.
#[derive(Clone)]
pub enum SelectionMode {
    Normal,
    /// Visual line mode — selects whole steps. Range is min..=max of anchor,cursor.
    VisualLine {
        anchor: usize,
        cursor: usize,
    },
    /// Visual character mode — selects text within an expanded step's value.
    Visual {
        step_index: usize,
        /// JSON Pointer (RFC 6901) to the string value being edited.
        json_pointer: String,
        /// Selection start (char offset in rendered value).
        anchor: usize,
        /// Current cursor position.
        cursor: usize,
    },
}

/// UI mode.
pub enum Mode {
    Normal,
    Help,
    Search {
        query: String,
        cursor: usize,
        matches: Vec<usize>,
    },
    Confirm,
}

/// A single line of the expanded field view, mapped back to a JSON Pointer.
#[derive(Clone, Debug)]
pub struct FieldLine {
    /// JSON Pointer to the string value this line belongs to, or None for structural labels.
    pub json_pointer: Option<String>,
    /// The display text for this line.
    pub text: String,
    /// Character offset within the full string value where this line starts.
    /// Only meaningful when json_pointer is Some.
    pub value_offset: usize,
    /// Length of the value content on this line (excluding label prefix).
    pub value_len: usize,
}

/// Build a flat list of FieldLines from a step's JSON for the expanded view.
/// `wrap_width` is the max character width for value content before wrapping.
pub fn build_field_lines(step: &v1::Step, wrap_width: usize) -> Vec<FieldLine> {
    let value = match serde_json::to_value(step) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut lines = Vec::new();
    walk_value(&value, "", "", wrap_width, &mut lines);
    lines
}

fn walk_value(
    value: &serde_json::Value,
    pointer: &str,
    label: &str,
    wrap_width: usize,
    lines: &mut Vec<FieldLine>,
) {
    match value {
        serde_json::Value::Object(map) => {
            if !label.is_empty() {
                lines.push(FieldLine {
                    json_pointer: None,
                    text: format!("{label}:"),
                    value_offset: 0,
                    value_len: 0,
                });
            }
            // Sort keys for deterministic ordering.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                let val = &map[key];
                let child_pointer = format!("{pointer}/{}", escape_json_pointer(key));
                let child_label = if label.is_empty() {
                    key.clone()
                } else {
                    format!("  {key}")
                };
                walk_value(val, &child_pointer, &child_label, wrap_width, lines);
            }
        }
        serde_json::Value::Array(arr) => {
            if !label.is_empty() {
                lines.push(FieldLine {
                    json_pointer: None,
                    text: format!("{label}:"),
                    value_offset: 0,
                    value_len: 0,
                });
            }
            for (i, val) in arr.iter().enumerate() {
                let child_pointer = format!("{pointer}/{i}");
                let child_label = format!("  [{i}]");
                walk_value(val, &child_pointer, &child_label, wrap_width, lines);
            }
        }
        serde_json::Value::String(s) => {
            // Split into lines first (handles actual newlines in the string).
            let s_lines: Vec<&str> = s.lines().collect();
            let is_multiline = s_lines.len() > 1;

            // Check if any line needs wrapping.
            let label_overhead = label.len() + 2; // ": "
            let first_line_width = wrap_width.saturating_sub(label_overhead);
            let continuation_width = wrap_width.saturating_sub(4); // "    " indent
            let needs_wrap = if is_multiline {
                true
            } else {
                s.len() > first_line_width
            };

            if !needs_wrap {
                // Fits on one line with label.
                let display = s.replace('\t', "  ");
                let value_len = display.len();
                lines.push(FieldLine {
                    json_pointer: Some(pointer.to_string()),
                    text: format!("{label}: {display}"),
                    value_offset: 0,
                    value_len,
                });
            } else {
                // Show label with |, then wrapped content lines.
                lines.push(FieldLine {
                    json_pointer: None,
                    text: format!("{label}: |"),
                    value_offset: 0,
                    value_len: 0,
                });
                let mut offset = 0;
                for (line_idx, line) in s_lines.iter().enumerate() {
                    let display = line.replace('\t', "  ");
                    // Wrap this line if it's too long.
                    if display.len() <= continuation_width {
                        let value_len = display.len();
                        lines.push(FieldLine {
                            json_pointer: Some(pointer.to_string()),
                            text: format!("    {display}"),
                            value_offset: offset,
                            value_len,
                        });
                    } else {
                        // Wrap at continuation_width.
                        let mut chunk_start = 0;
                        while chunk_start < display.len() {
                            let chunk_end = (chunk_start + continuation_width).min(display.len());
                            let chunk = &display[chunk_start..chunk_end];
                            lines.push(FieldLine {
                                json_pointer: Some(pointer.to_string()),
                                text: format!("    {chunk}"),
                                value_offset: offset + chunk_start,
                                value_len: chunk.len(),
                            });
                            chunk_start = chunk_end;
                        }
                    }
                    offset += line.len();
                    if line_idx < s_lines.len() - 1 {
                        offset += 1; // +1 for the newline between lines
                    }
                }
            }
        }
        serde_json::Value::Number(n) => {
            let display = n.to_string();
            lines.push(FieldLine {
                json_pointer: Some(pointer.to_string()),
                text: format!("{label}: {display}"),
                value_offset: 0,
                value_len: display.len(),
            });
        }
        serde_json::Value::Bool(b) => {
            let display = b.to_string();
            lines.push(FieldLine {
                json_pointer: Some(pointer.to_string()),
                text: format!("{label}: {display}"),
                value_offset: 0,
                value_len: display.len(),
            });
        }
        serde_json::Value::Null => {
            lines.push(FieldLine {
                json_pointer: Some(pointer.to_string()),
                text: format!("{label}: null"),
                value_offset: 0,
                value_len: 4,
            });
        }
    }
}

fn escape_json_pointer(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// Characters for a single row's DAG graph column.
pub struct GraphLine {
    /// The characters to render for this row's graph column.
    pub segments: Vec<GraphSegment>,
}

/// A single segment in a graph line.
pub struct GraphSegment {
    pub ch: &'static str,
    pub is_dead_end: bool,
}

/// Build step entries from a path's steps.
pub fn build_entries(steps: &[v1::Step]) -> Vec<StepEntry> {
    steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let actor_label = actor_label(&step.step.actor);
            let summary = build_summary(step);
            StepEntry {
                index: i,
                step: step.clone(),
                summary,
                actor_label,
                included: true,
                expanded: false,
                text_redactions: Vec::new(),
            }
        })
        .collect()
}

/// Derive a short actor label from the actor string.
fn actor_label(actor: &str) -> String {
    if actor.starts_with("human:") {
        "user".to_string()
    } else if actor.starts_with("agent:clash-policy") {
        "policy".to_string()
    } else if actor.starts_with("agent:") {
        "assistant".to_string()
    } else if actor.starts_with("tool:") {
        actor.strip_prefix("tool:").unwrap_or(actor).to_string()
    } else {
        actor.to_string()
    }
}

/// Build a one-line summary from a step.
fn build_summary(step: &v1::Step) -> String {
    let actor = &step.step.actor;

    // Try to extract text from conversation-style structural changes.
    if actor.starts_with("human:")
        || actor.starts_with("agent:claude")
        || actor.starts_with("agent:cc")
    {
        // Look for conversation.append structural changes.
        for change in step.change.values() {
            if let Some(ref structural) = change.structural
                && structural.change_type == "conversation.append"
            {
                return build_conversation_summary(structural, actor);
            }
        }
        // Fallback: try raw text from any change.
        for change in step.change.values() {
            if let Some(ref raw) = change.raw {
                return truncate_first_line(raw, 80);
            }
        }
    }

    // Policy steps.
    if actor == "agent:clash-policy" {
        for change in step.change.values() {
            if let Some(ref structural) = change.structural
                && structural.change_type == "policy_evaluation"
            {
                let effect = structural
                    .extra
                    .get("effect")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let tool = structural
                    .extra
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                return format!("{effect} {tool}").trim().to_string();
            }
        }
    }

    // Fallback.
    if let Some(ref meta) = step.meta
        && let Some(ref intent) = meta.intent
    {
        return truncate_first_line(intent, 80);
    }
    format!("{} — {}", actor, step.step.id)
}

/// Build summary from a conversation.append structural change.
fn build_conversation_summary(structural: &v1::StructuralChange, actor: &str) -> String {
    let text = structural
        .extra
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let tool_uses: Vec<&str> = structural
        .extra
        .get("tool_uses")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    if !tool_uses.is_empty() && actor.starts_with("agent:") {
        let tools = format!("[{}]", tool_uses.join(", "));
        let text_part = truncate_first_line(text, 80 - tools.len().min(60));
        if text_part.is_empty() {
            tools
        } else {
            format!("{tools} {text_part}")
        }
    } else {
        truncate_first_line(text, 80)
    }
}

fn truncate_first_line(s: &str, max: usize) -> String {
    let first_line = s.lines().next().unwrap_or("").trim();
    if first_line.len() <= max {
        first_line.to_string()
    } else {
        let truncated: String = first_line.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

/// Compute the DAG graph layout for a list of steps.
///
/// Returns one `GraphLine` per step with multi-column segments showing the DAG structure.
/// Main path stays in column 0; dead-end branches get offset columns.
pub fn compute_graph_layout(steps: &[v1::Step], head_id: &str) -> Vec<GraphLine> {
    if steps.is_empty() {
        return Vec::new();
    }

    let ancestor_ids = query::ancestors(steps, head_id);

    // Build children map: parent_id -> vec of child step indices.
    let mut children_of: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, step) in steps.iter().enumerate() {
        for parent in &step.step.parents {
            children_of.entry(parent.as_str()).or_default().push(i);
        }
    }
    let has_children: HashSet<&str> = children_of.keys().copied().collect();

    // Column assignment + rendering in a single pass.
    // Freed columns are reused by subsequent branches.
    let mut col_of: HashMap<&str, usize> = HashMap::new();
    let mut active: HashSet<usize> = HashSet::new();
    let mut free_cols: Vec<usize> = Vec::new(); // available for reuse
    let mut next_col: usize = 1;
    let mut result = Vec::with_capacity(steps.len());

    // Root gets column 0.
    if let Some(first) = steps.first() {
        col_of.insert(&first.step.id, 0);
    }

    for step in steps {
        let id = step.step.id.as_str();

        // Ensure this step has a column assigned.
        if !col_of.contains_key(id) {
            let parent_col = step
                .step
                .parents
                .first()
                .and_then(|p| col_of.get(p.as_str()).copied())
                .unwrap_or(0);
            col_of.insert(id, parent_col);
        }

        let my_col = col_of[id];
        let is_on_main = ancestor_ids.contains(id);
        let is_dead = !is_on_main;
        let is_leaf = !has_children.contains(id);
        let has_branch = children_of.get(id).is_some_and(|c| c.len() > 1);

        // If this step merges AND branches, pre-free merged columns so
        // branch children can reuse them immediately.
        if step.step.parents.len() > 1 && children_of.get(id).is_some_and(|k| k.len() > 1) {
            for parent in &step.step.parents {
                let parent_col = col_of.get(parent.as_str()).copied().unwrap_or(0);
                if parent_col != my_col && !free_cols.contains(&parent_col) {
                    free_cols.push(parent_col);
                }
            }
        }

        // Assign columns to children — reuse freed columns.
        if let Some(kids) = children_of.get(id) {
            if kids.len() == 1 {
                let kid_id = steps[kids[0]].step.id.as_str();
                col_of.entry(kid_id).or_insert(my_col);
            } else {
                let main_child_idx = kids
                    .iter()
                    .find(|&&i| ancestor_ids.contains(&steps[i].step.id));
                for &kid_idx in kids {
                    let kid_id = steps[kid_idx].step.id.as_str();
                    if Some(&kid_idx) == main_child_idx {
                        col_of.entry(kid_id).or_insert(my_col);
                    } else if !col_of.contains_key(kid_id) {
                        // Reuse a freed column or allocate a new one.
                        let col = if let Some(c) = free_cols.pop() {
                            c
                        } else {
                            let c = next_col;
                            next_col += 1;
                            c
                        };
                        col_of.insert(kid_id, col);
                    }
                }
            }
        }

        // This step's column becomes active.
        active.insert(my_col);

        // Columns being merged into this step (will be deactivated after this row).
        let merging_cols: HashSet<usize> = if step.step.parents.len() > 1 {
            step.step
                .parents
                .iter()
                .filter_map(|p| col_of.get(p.as_str()).copied())
                .filter(|&c| c != my_col)
                .collect()
        } else {
            HashSet::new()
        };

        // For branches, find new child columns so we can draw horizontal connectors.
        let branch_targets: Vec<usize> = if has_branch {
            let mut targets: Vec<usize> = children_of
                .get(id)
                .into_iter()
                .flatten()
                .map(|&kid_idx| col_of[steps[kid_idx].step.id.as_str()])
                .filter(|&c| c != my_col)
                .collect();
            targets.sort();
            targets.dedup();
            targets
        } else {
            Vec::new()
        };
        let rightmost_branch = branch_targets.last().copied();
        let branch_target_set: HashSet<usize> = branch_targets.iter().copied().collect();

        // For merges, find the rightmost merging column for horizontal connectors.
        let rightmost_merge = if !merging_cols.is_empty() {
            merging_cols.iter().copied().max()
        } else {
            None
        };

        // Max column to render (include branch/merge targets not yet active).
        let max_col = active
            .iter()
            .copied()
            .chain(branch_targets.iter().copied())
            .chain(merging_cols.iter().copied())
            .max()
            .unwrap_or(0);

        // Solid │ for the step's column, dotted ┆ for pass-through columns.
        let mut segments = Vec::new();
        for col in 0..=max_col {
            let ch = if col == my_col {
                // This step's own column — solid line.
                if has_branch {
                    "├"
                } else if is_leaf && is_dead {
                    "╵"
                } else if step.step.parents.len() > 1 {
                    "├"
                } else {
                    "│"
                }
            } else if let Some(rb) = rightmost_branch {
                // Branch row: horizontal connectors from my_col to rightmost branch.
                if col > my_col && col <= rb {
                    if branch_target_set.contains(&col) && merging_cols.contains(&col) {
                        // Merge+split reuse: column absorbed and reused for new branch.
                        if col == rb { "┤" } else { "┼" }
                    } else if branch_target_set.contains(&col) {
                        if col == rb { "╮" } else { "┬" }
                    } else if active.contains(&col) && merging_cols.contains(&col) {
                        "┼" // merge+split: column being merged AND branch crosses it
                    } else if active.contains(&col) {
                        "│" // vertical passes through, not a junction
                    } else {
                        "─"
                    }
                } else if merging_cols.contains(&col) {
                    " "
                } else if active.contains(&col) {
                    "┆"
                } else {
                    " "
                }
            } else if let Some(rm) = rightmost_merge {
                // Merge row: horizontal connectors from my_col to rightmost merge.
                if col > my_col && col <= rm {
                    if merging_cols.contains(&col) {
                        if col == rm { "╯" } else { "┴" }
                    } else if active.contains(&col) {
                        "┼"
                    } else {
                        "─"
                    }
                } else if merging_cols.contains(&col) {
                    " "
                } else if active.contains(&col) {
                    "┆"
                } else {
                    " "
                }
            } else if merging_cols.contains(&col) {
                " "
            } else if active.contains(&col) {
                "┆" // pass-through: dotted
            } else {
                " "
            };

            segments.push(GraphSegment {
                ch,
                is_dead_end: col == my_col && is_dead && is_leaf,
            });
        }

        result.push(GraphLine { segments });

        // If this step branches, activate all child columns immediately
        // so continuation lines appear between branch point and child.
        if has_branch && let Some(kids) = children_of.get(id) {
            for &kid_idx in kids {
                let kid_col = col_of[steps[kid_idx].step.id.as_str()];
                active.insert(kid_col);
            }
        }

        // If this step merges (multiple parents), deactivate and free the merged columns.
        // Don't deactivate columns reused as branch targets (merge+split reuse).
        if step.step.parents.len() > 1 {
            let branch_child_cols: HashSet<usize> = if has_branch {
                children_of
                    .get(id)
                    .into_iter()
                    .flatten()
                    .map(|&kid_idx| col_of[steps[kid_idx].step.id.as_str()])
                    .collect()
            } else {
                HashSet::new()
            };
            for parent in &step.step.parents {
                let parent_col = col_of.get(parent.as_str()).copied().unwrap_or(0);
                if parent_col != my_col && !branch_child_cols.contains(&parent_col) {
                    active.remove(&parent_col);
                    if parent_col != 0 && !free_cols.contains(&parent_col) {
                        free_cols.push(parent_col);
                    }
                }
            }
        }

        // If this step is a dead-end leaf, deactivate and free its column.
        if is_leaf {
            active.remove(&my_col);
            if my_col != 0 {
                free_cols.push(my_col);
            }
        }
    }

    // Pad all rows to the same width so columns align vertically.
    let max_width = result.iter().map(|gl| gl.segments.len()).max().unwrap_or(0);
    for gl in &mut result {
        while gl.segments.len() < max_width {
            gl.segments.push(GraphSegment {
                ch: " ",
                is_dead_end: false,
            });
        }
    }

    result
}

/// Render a graph layout as a vector of strings (for testing).
/// Trailing spaces are trimmed.
#[cfg(test)]
fn graph_to_strings(layout: &[GraphLine]) -> Vec<String> {
    layout
        .iter()
        .map(|gl| {
            gl.segments
                .iter()
                .map(|s| s.ch)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use toolpath::v1::{ArtifactChange, Step, StructuralChange};

    fn make_step(id: &str, actor: &str, parents: &[&str]) -> Step {
        let mut step = Step::new(id, actor, "2026-01-29T10:00:00Z");
        for p in parents {
            step = step.with_parent(*p);
        }
        step
    }

    fn make_conversation_step(
        id: &str,
        actor: &str,
        parents: &[&str],
        text: &str,
        tool_uses: &[&str],
    ) -> Step {
        let mut step = make_step(id, actor, parents);
        let mut extra = HashMap::new();
        extra.insert("text".to_string(), serde_json::json!(text));
        extra.insert("role".to_string(), serde_json::json!("user"));
        if !tool_uses.is_empty() {
            extra.insert("tool_uses".to_string(), serde_json::json!(tool_uses));
        }
        step.change.insert(
            "claude://session".to_string(),
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "conversation.append".to_string(),
                    extra,
                }),
            },
        );
        step
    }

    fn make_policy_step(id: &str, parents: &[&str], effect: &str, tool_name: &str) -> Step {
        let mut step = make_step(id, "agent:clash-policy", parents);
        let mut extra = HashMap::new();
        extra.insert("effect".to_string(), serde_json::json!(effect));
        extra.insert("tool_name".to_string(), serde_json::json!(tool_name));
        step.change.insert(
            "clash://policy/evaluations".to_string(),
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "policy_evaluation".to_string(),
                    extra,
                }),
            },
        );
        step
    }

    // ── Summary generation tests ──────────────────────────────────────

    #[test]
    fn summary_from_user_message() {
        let step =
            make_conversation_step("s1", "human:user", &[], "Fix the bug in auth handler", &[]);
        let summary = build_summary(&step);
        assert_eq!(summary, "Fix the bug in auth handler");
    }

    #[test]
    fn summary_from_assistant_with_tools() {
        let step = make_conversation_step(
            "s2",
            "agent:claude-code",
            &["s1"],
            "Looking at the file",
            &["Read", "Edit", "Bash"],
        );
        let summary = build_summary(&step);
        assert!(summary.starts_with("[Read, Edit, Bash]"));
        assert!(summary.contains("Looking at the file"));
    }

    #[test]
    fn summary_from_assistant_message_only() {
        let step = make_conversation_step(
            "s2",
            "agent:claude-code",
            &["s1"],
            "I'll fix the authentication handler",
            &[],
        );
        let summary = build_summary(&step);
        assert_eq!(summary, "I'll fix the authentication handler");
    }

    #[test]
    fn summary_from_policy_step() {
        let step = make_policy_step("s3", &["s2"], "allow", "Read");
        let summary = build_summary(&step);
        assert_eq!(summary, "allow Read");
    }

    #[test]
    fn summary_fallback() {
        let step = make_step("s1", "ci:github-actions", &[]);
        let summary = build_summary(&step);
        assert_eq!(summary, "ci:github-actions — s1");
    }

    #[test]
    fn summary_truncation() {
        let long_text = "a".repeat(200);
        let step = make_conversation_step("s1", "human:user", &[], &long_text, &[]);
        let summary = build_summary(&step);
        // 79 'a' chars + '…' (3 bytes in UTF-8) = 82 bytes, but 80 chars.
        assert!(summary.chars().count() <= 80);
        assert!(summary.ends_with('…'));
    }

    // ═══════════════════════════════════════════════════════════════════
    // DAG graph layout — full visual tests
    // ═══════════════════════════════════════════════════════════════════
    //
    // Each test shows the expected graph as an ASCII art comment, then
    // asserts the full graph output. Character reference:
    //
    //   │  solid    — this step belongs to this column
    //   ┆  dotted   — another column passing through
    //   ├  branch/merge source — horizontal connector goes right
    //   ╮  branch target (rightmost)
    //   ╯  merge source (rightmost)
    //   ┬  branch intermediate target
    //   ┴  merge intermediate source
    //   ┤  merge+split reuse — column absorbed and immediately reused for new branch
    //   ┼  merge+multi-split — column being merged AND multiple branches cross it
    //   ─  horizontal over inactive gap
    //   ╵  dead end — leaf not on main path

    #[test]
    fn graph_linear() {
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("c", "x", &["b"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "c"));
        assert_eq!(
            g,
            vec![
                "│", // a
                "│", // b
                "│", // c
            ]
        );
    }

    #[test]
    fn graph_two_way_split_with_dead_end() {
        // a splits → b (main), c (dead end)
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("c", "x", &["a"]),
            make_step("d", "x", &["b"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "d"));
        assert_eq!(
            g,
            vec![
                "├╮", // a: split
                "│┆", // b: solid col 0, pass-through col 1
                "┆╵", // c: pass-through col 0, dead end col 1
                "│",  // d
            ]
        );
    }

    #[test]
    fn graph_three_way_split() {
        // a splits → b, c (both dead ends), d (main)
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("c", "x", &["a"]),
            make_step("d", "x", &["a"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "d"));
        assert_eq!(
            g,
            vec![
                "├┬╮", // a: ├(col0) ┬(col1 intermediate) ╮(col2)
                "┆╵┆", // b: dead end col 1
                "┆ ╵", // c: dead end col 2 (col 1 freed)
                "│",   // d
            ]
        );
    }

    #[test]
    fn graph_dead_end_multi_step_branch() {
        // a → b → c (dead branch), a → d (main)
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("c", "x", &["b"]),
            make_step("d", "x", &["a"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "d"));
        assert_eq!(
            g,
            vec![
                "├╮", // a: split
                "┆│", // b: dead branch, solid col 1
                "┆╵", // c: dead end col 1
                "│",  // d: main col 0
            ]
        );
    }

    #[test]
    fn graph_diamond_split_then_merge() {
        // a splits → b, c; both merge at d
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("c", "x", &["a"]),
            make_step("d", "x", &["b", "c"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "d"));
        assert_eq!(
            g,
            vec![
                "├╮", // a: split
                "│┆", // b: solid col 0
                "┆│", // c: solid col 1
                "├╯", // d: merge — ╯ absorbs col 1
            ]
        );
    }

    #[test]
    fn graph_three_way_merge() {
        // a splits 3 ways → b, c, e; all merge at f
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("c", "x", &["a"]),
            make_step("e", "x", &["a"]),
            make_step("f", "x", &["b", "c", "e"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "f"));
        assert_eq!(
            g,
            vec![
                "├┬╮", // a: 3-way split
                "│┆┆", // b: solid col 0
                "┆│┆", // c: solid col 1
                "┆┆│", // e: solid col 2
                "├┴╯", // f: 3-way merge — ┴ intermediate, ╯ rightmost
            ]
        );
    }

    #[test]
    fn graph_cross_branch_over_active() {
        // a→b(main)+d1(dead). b→c(main)+d2(dead), branch crosses col 1.
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("d1", "x", &["a"]),
            make_step("c", "x", &["b"]),
            make_step("d2", "x", &["b"]),
            make_step("e", "x", &["c"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "e"));
        assert_eq!(
            g,
            vec![
                "├╮",  // a: split → col 1
                "├│╮", // b: split → col 2, │ passes through col 1
                "┆╵┆", // d1: dead end col 1
                "│ ┆", // c: solid col 0, gap (col 1 freed)
                "┆ ╵", // d2: dead end col 2
                "│",   // e
            ]
        );
    }

    #[test]
    fn graph_merge_and_split_simultaneous() {
        // d merges (parents b,c) AND splits (children e,f).
        // Single split reuses the merged column → ┤ instead of new column.
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("c", "x", &["a"]),
            make_step("d", "x", &["b", "c"]),
            make_step("e", "x", &["d"]),
            make_step("f", "x", &["d"]),
            make_step("g", "x", &["e"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "g"));
        assert_eq!(
            g,
            vec![
                "├╮", // a: split
                "│┆", // b
                "┆│", // c
                "├┤", // d: merge+split — ┤ = col 1 absorbed and reused
                "│┆", // e
                "┆╵", // f: dead end col 1 (reused)
                "│",  // g
            ]
        );
    }

    #[test]
    fn graph_long_parallel_branches() {
        // Two branches interleave for 3 steps then merge.
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b1", "x", &["a"]),
            make_step("c1", "x", &["a"]),
            make_step("b2", "x", &["b1"]),
            make_step("c2", "x", &["c1"]),
            make_step("b3", "x", &["b2"]),
            make_step("c3", "x", &["c2"]),
            make_step("m", "x", &["b3", "c3"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "m"));
        assert_eq!(
            g,
            vec![
                "├╮", // a: split
                "│┆", // b1: solid col 0, dotted col 1
                "┆│", // c1: dotted col 0, solid col 1
                "│┆", // b2
                "┆│", // c2
                "│┆", // b3
                "┆│", // c3
                "├╯", // m: merge
            ]
        );
    }

    #[test]
    fn graph_column_reuse_after_dead_end() {
        // d1 dies before c branches → col 1 reused.
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("d1", "x", &["a"]),
            make_step("c", "x", &["b"]),
            make_step("d2", "x", &["c"]),
            make_step("d3", "x", &["c"]),
            make_step("e", "x", &["d2"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "e"));
        assert_eq!(
            g,
            vec![
                "├╮", // a: split → col 1
                "│┆", // b
                "┆╵", // d1: dead end, col 1 freed
                "├╮", // c: split reuses col 1
                "│┆", // d2
                "┆╵", // d3: dead end col 1 (reused)
                "│",  // e
            ]
        );
    }

    #[test]
    fn graph_column_reuse_after_merge() {
        // Merge frees col 1, next branch reuses it.
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("c", "x", &["a"]),
            make_step("d", "x", &["b", "c"]),
            make_step("e", "x", &["d"]),
            make_step("f", "x", &["e"]),
            make_step("h", "x", &["e"]),
            make_step("g", "x", &["f"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "g"));
        assert_eq!(
            g,
            vec![
                "├╮", // a: split
                "│┆", // b
                "┆│", // c
                "├╯", // d: merge, col 1 freed
                "├╮", // e: split, col 1 reused
                "│┆", // f
                "┆╵", // h: dead end col 1
                "│",  // g
            ]
        );
    }

    #[test]
    fn graph_double_diamond() {
        // Two consecutive diamonds with column reuse.
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("c", "x", &["a"]),
            make_step("d", "x", &["b", "c"]),
            make_step("mid", "x", &["d"]),
            make_step("e", "x", &["mid"]),
            make_step("f", "x", &["mid"]),
            make_step("g", "x", &["e", "f"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "g"));
        assert_eq!(
            g,
            vec![
                "├╮", // a: split
                "│┆", // b
                "┆│", // c
                "├╯", // d: merge
                "├╮", // mid: split (col 1 reused)
                "│┆", // e
                "┆│", // f
                "├╯", // g: merge
            ]
        );
    }

    #[test]
    fn graph_merge_with_parallel_dead_end() {
        // 3-way split: b+c merge at e, d is dead end.
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("c", "x", &["a"]),
            make_step("d", "x", &["a"]),
            make_step("e", "x", &["b", "c"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "e"));
        assert_eq!(
            g,
            vec![
                "├┬╮", // a: 3-way split
                "│┆┆", // b: solid col 0
                "┆│┆", // c: solid col 1
                "┆┆╵", // d: dead end col 2
                "├╯",  // e: merge cols 0+1
            ]
        );
    }

    #[test]
    fn graph_nested_split() {
        // a→b(main)+c(branch). c itself splits to d+e.
        let steps = vec![
            make_step("a", "x", &[]),
            make_step("b", "x", &["a"]),
            make_step("c", "x", &["a"]),
            make_step("d", "x", &["c"]),
            make_step("e", "x", &["c"]),
            make_step("f", "x", &["b"]),
        ];
        let g = graph_to_strings(&compute_graph_layout(&steps, "f"));
        let c_row = &g[2];
        assert!(c_row.contains('├'), "c should branch: {c_row}");
        assert!(c_row.contains('╮'), "c should have branch target: {c_row}");
    }
}
