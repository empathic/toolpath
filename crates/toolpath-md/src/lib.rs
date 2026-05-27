#![doc = include_str!("../README.md")]

mod source;

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use toolpath::v1::{ArtifactChange, Graph, Path, PathOrRef, Step, query};

/// Detail level for the rendered markdown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Detail {
    /// File-level change summaries, no diffs.
    #[default]
    Summary,
    /// Full inline diffs as fenced code blocks.
    Full,
}

/// Options controlling the markdown output.
pub struct RenderOptions {
    /// How much detail to include for each step's changes.
    pub detail: Detail,
    /// Emit YAML front matter with machine-readable metadata.
    pub front_matter: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            detail: Detail::Summary,
            front_matter: false,
        }
    }
}

/// Render a Toolpath [`Graph`] as a Markdown string. Single-path graphs use
/// the path-focused layout, multi-path graphs use the cross-path layout.
///
/// # Examples
///
/// ```
/// use toolpath::v1::{Graph, Path, PathIdentity, Step};
/// use toolpath_md::{render, RenderOptions};
///
/// let step = Step::new("s1", "human:alex", "2026-01-29T10:00:00Z")
///     .with_raw_change("src/main.rs", "@@ -1 +1 @@\n-old\n+new")
///     .with_intent("Fix greeting");
/// let path = Path {
///     path: PathIdentity {
///         id: "p1".into(),
///         base: None,
///         head: "s1".into(),
///         graph_ref: None,
///     },
///     steps: vec![step],
///     meta: None,
/// };
/// let graph = Graph::from_path(path);
/// let md = render(&graph, &RenderOptions::default());
/// assert!(md.contains("human:alex"));
/// ```
pub fn render(graph: &Graph, options: &RenderOptions) -> String {
    if graph.paths.len() == 1
        && let PathOrRef::Path(p) = &graph.paths[0]
    {
        return render_path(p, options);
    }
    render_graph(graph, options)
}

/// Render a single [`Step`] as Markdown.
pub fn render_step(step: &Step, options: &RenderOptions) -> String {
    let mut out = String::new();

    if options.front_matter {
        write_step_front_matter(&mut out, step);
    }

    writeln!(out, "# {}", step.step.id).unwrap();
    writeln!(out).unwrap();
    write_step_body(&mut out, step, options, false);

    out
}

/// Render a [`Path`] as Markdown.
pub fn render_path(path: &Path, options: &RenderOptions) -> String {
    if is_agent_coding_session(path) {
        return render_conversation_transcript(path, options);
    }

    let mut out = String::new();

    if options.front_matter {
        write_path_front_matter(&mut out, path);
    }

    // Title
    let title = path
        .meta
        .as_ref()
        .and_then(|m| m.title.as_deref())
        .unwrap_or(&path.path.id);
    writeln!(out, "# {title}").unwrap();
    writeln!(out).unwrap();

    // Context block
    write_path_context(&mut out, path);

    // Topological sort for readable ordering
    let sorted = topo_sort(&path.steps);
    let active = query::ancestors(&path.steps, &path.path.head);
    let dead_end_set: HashSet<&str> = path
        .steps
        .iter()
        .filter(|s| !active.contains(&s.step.id))
        .map(|s| s.step.id.as_str())
        .collect();

    // Timeline
    writeln!(out, "## Timeline").unwrap();
    writeln!(out).unwrap();

    for step in &sorted {
        let is_dead = dead_end_set.contains(step.step.id.as_str());
        let is_head = step.step.id == path.path.head;
        write_path_step(&mut out, step, options, is_dead, is_head);
    }

    // Dead ends section (if any)
    if !dead_end_set.is_empty() {
        write_dead_ends_section(&mut out, &sorted, &dead_end_set);
    }

    // Review summary section
    write_review_section(&mut out, &sorted);

    // Actors section (if defined in meta)
    if let Some(meta) = &path.meta
        && let Some(actors) = &meta.actors
    {
        write_actors_section(&mut out, actors);
    }

    out
}

/// Render a [`Graph`] as Markdown.
pub fn render_graph(graph: &Graph, options: &RenderOptions) -> String {
    let mut out = String::new();

    if options.front_matter {
        write_graph_front_matter(&mut out, graph);
    }

    // Title
    let title = graph
        .meta
        .as_ref()
        .and_then(|m| m.title.as_deref())
        .unwrap_or(&graph.graph.id);
    writeln!(out, "# {title}").unwrap();
    writeln!(out).unwrap();

    // Intent
    if let Some(meta) = &graph.meta
        && let Some(intent) = &meta.intent
    {
        writeln!(out, "> {intent}").unwrap();
        writeln!(out).unwrap();
    }

    // Refs
    if let Some(meta) = &graph.meta
        && !meta.refs.is_empty()
    {
        for r in &meta.refs {
            writeln!(out, "- **{}:** `{}`", r.rel, r.href).unwrap();
        }
        writeln!(out).unwrap();
    }

    // Summary table
    let inline_paths: Vec<&Path> = graph
        .paths
        .iter()
        .filter_map(|por| match por {
            PathOrRef::Path(p) => Some(p.as_ref()),
            PathOrRef::Ref(_) => None,
        })
        .collect();

    let ref_urls: Vec<&str> = graph
        .paths
        .iter()
        .filter_map(|por| match por {
            PathOrRef::Ref(r) => Some(r.ref_url.as_str()),
            PathOrRef::Path(_) => None,
        })
        .collect();

    if !inline_paths.is_empty() {
        writeln!(out, "| Path | Steps | Actors | Head |").unwrap();
        writeln!(out, "|------|-------|--------|------|").unwrap();
        for path in &inline_paths {
            let path_title = path
                .meta
                .as_ref()
                .and_then(|m| m.title.as_deref())
                .unwrap_or(&path.path.id);
            let step_count = path.steps.len();
            let actors = query::all_actors(&path.steps);
            let actors_str = format_actor_list(&actors);
            writeln!(
                out,
                "| {path_title} | {step_count} | {actors_str} | `{}` |",
                path.path.head
            )
            .unwrap();
        }
        writeln!(out).unwrap();
    }

    if !ref_urls.is_empty() {
        writeln!(out, "**External references:**").unwrap();
        for url in &ref_urls {
            writeln!(out, "- `{url}`").unwrap();
        }
        writeln!(out).unwrap();
    }

    // Each path as a section
    for path in &inline_paths {
        let path_title = path
            .meta
            .as_ref()
            .and_then(|m| m.title.as_deref())
            .unwrap_or(&path.path.id);
        writeln!(out, "---").unwrap();
        writeln!(out).unwrap();
        writeln!(out, "## {path_title}").unwrap();
        writeln!(out).unwrap();

        if is_agent_coding_session(path) {
            write_path_context(&mut out, path);
            write_conversation_transcript_body(&mut out, path, options);
            continue;
        }

        write_path_context(&mut out, path);

        let sorted = topo_sort(&path.steps);
        let active = query::ancestors(&path.steps, &path.path.head);
        let dead_end_set: HashSet<&str> = path
            .steps
            .iter()
            .filter(|s| !active.contains(&s.step.id))
            .map(|s| s.step.id.as_str())
            .collect();

        for step in &sorted {
            let is_dead = dead_end_set.contains(step.step.id.as_str());
            let is_head = step.step.id == path.path.head;
            write_path_step(&mut out, step, options, is_dead, is_head);
        }

        if !dead_end_set.is_empty() {
            write_dead_ends_section(&mut out, &sorted, &dead_end_set);
        }
    }

    // Actors section (if defined in meta)
    if let Some(meta) = &graph.meta
        && let Some(actors) = &meta.actors
    {
        writeln!(out, "---").unwrap();
        writeln!(out).unwrap();
        write_actors_section(&mut out, actors);
    }

    out
}

// ============================================================================
// Internal rendering helpers
// ============================================================================

fn write_step_body(out: &mut String, step: &Step, options: &RenderOptions, compact: bool) {
    let heading = if compact { "###" } else { "##" };

    // Actor + timestamp line
    writeln!(out, "**Actor:** `{}`", step.step.actor).unwrap();
    writeln!(out, "**Timestamp:** {}", step.step.timestamp).unwrap();

    // Parents
    if !step.step.parents.is_empty() {
        let parents: Vec<String> = step.step.parents.iter().map(|p| format!("`{p}`")).collect();
        writeln!(out, "**Parents:** {}", parents.join(", ")).unwrap();
    }

    writeln!(out).unwrap();

    // Intent
    if let Some(meta) = &step.meta
        && let Some(intent) = &meta.intent
    {
        writeln!(out, "> {intent}").unwrap();
        writeln!(out).unwrap();
    }

    // Refs
    if let Some(meta) = &step.meta
        && !meta.refs.is_empty()
    {
        for r in &meta.refs {
            writeln!(out, "- **{}:** `{}`", r.rel, r.href).unwrap();
        }
        writeln!(out).unwrap();
    }

    // Changes
    if !step.change.is_empty() {
        writeln!(out, "{heading} Changes").unwrap();
        writeln!(out).unwrap();

        let mut artifacts: Vec<&String> = step.change.keys().collect();
        artifacts.sort();

        for artifact in artifacts {
            let change = &step.change[artifact];
            write_artifact_change(out, artifact, change, options);
        }
    }
}

fn write_artifact_change(
    out: &mut String,
    artifact: &str,
    change: &ArtifactChange,
    options: &RenderOptions,
) {
    let change_type = change
        .structural
        .as_ref()
        .map(|s| s.change_type.as_str())
        .unwrap_or("");

    match options.detail {
        Detail::Summary => match change_type {
            "review.comment" | "review.conversation" => {
                let display = friendly_artifact_name(artifact);
                let body = change
                    .structural
                    .as_ref()
                    .and_then(|s| s.extra.get("body"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let truncated = truncate_str(body, 120);
                if truncated.is_empty() {
                    writeln!(out, "- `{display}` (comment)").unwrap();
                } else {
                    writeln!(out, "- `{display}` \u{2014} \"{truncated}\"").unwrap();
                }
            }
            "review.decision" => {
                let state = change
                    .structural
                    .as_ref()
                    .and_then(|s| s.extra.get("state"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("COMMENTED");
                let marker = review_state_marker(state);
                let body = change.raw.as_deref().unwrap_or("");
                let truncated = truncate_str(body, 120);
                if truncated.is_empty() {
                    writeln!(out, "- {marker} {state}").unwrap();
                } else {
                    writeln!(out, "- {marker} {state} \u{2014} \"{truncated}\"").unwrap();
                }
            }
            "ci.run" => {
                let name = friendly_artifact_name(artifact);
                let conclusion = change
                    .structural
                    .as_ref()
                    .and_then(|s| s.extra.get("conclusion"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let marker = ci_conclusion_marker(conclusion);
                writeln!(out, "- {name} {marker} {conclusion}").unwrap();
            }
            _ => {
                let display = friendly_artifact_name(artifact);
                let annotation = change_annotation(change);
                writeln!(out, "- `{display}`{annotation}").unwrap();
            }
        },
        Detail::Full => {
            match change_type {
                "review.comment" | "review.conversation" => {
                    let display = friendly_artifact_name(artifact);
                    writeln!(out, "**`{display}`**").unwrap();
                    let body = change
                        .structural
                        .as_ref()
                        .and_then(|s| s.extra.get("body"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !body.is_empty() {
                        writeln!(out).unwrap();
                        for line in body.lines() {
                            writeln!(out, "> {line}").unwrap();
                        }
                    }
                    // Show diff_hunk if present
                    if let Some(raw) = &change.raw {
                        writeln!(out).unwrap();
                        writeln!(out, "```diff").unwrap();
                        writeln!(out, "{raw}").unwrap();
                        writeln!(out, "```").unwrap();
                    }
                    writeln!(out).unwrap();
                }
                "review.decision" => {
                    let state = change
                        .structural
                        .as_ref()
                        .and_then(|s| s.extra.get("state"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("COMMENTED");
                    let marker = review_state_marker(state);
                    writeln!(out, "**{marker} {state}**").unwrap();
                    if let Some(raw) = &change.raw {
                        writeln!(out).unwrap();
                        for line in raw.lines() {
                            writeln!(out, "> {line}").unwrap();
                        }
                    }
                    writeln!(out).unwrap();
                }
                "ci.run" => {
                    let name = friendly_artifact_name(artifact);
                    let conclusion = change
                        .structural
                        .as_ref()
                        .and_then(|s| s.extra.get("conclusion"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let marker = ci_conclusion_marker(conclusion);
                    write!(out, "**{name}** {marker} {conclusion}").unwrap();
                    if let Some(url) = change
                        .structural
                        .as_ref()
                        .and_then(|s| s.extra.get("url"))
                        .and_then(|v| v.as_str())
                    {
                        write!(out, " ([details]({url}))").unwrap();
                    }
                    writeln!(out).unwrap();
                    writeln!(out).unwrap();
                }
                _ => {
                    let display = friendly_artifact_name(artifact);
                    writeln!(out, "**`{display}`**").unwrap();
                    if let Some(raw) = &change.raw {
                        writeln!(out).unwrap();
                        writeln!(out, "```diff").unwrap();
                        writeln!(out, "{raw}").unwrap();
                        writeln!(out, "```").unwrap();
                    }
                    if let Some(structural) = &change.structural {
                        writeln!(out).unwrap();
                        let extra_str = if structural.extra.is_empty() {
                            String::new()
                        } else {
                            let pairs: Vec<String> = structural
                                .extra
                                .iter()
                                .map(|(k, v)| format!("{k}={v}"))
                                .collect();
                            format!(" ({})", pairs.join(", "))
                        };
                        writeln!(out, "Structural: `{}`{extra_str}", structural.change_type)
                            .unwrap();
                    }
                    writeln!(out).unwrap();
                }
            }
        }
    }
}

// ── Kind: agent-coding-session rendering ────────────────────────────

fn is_agent_coding_session(path: &Path) -> bool {
    path.meta.as_ref().and_then(|m| m.kind.as_deref())
        == Some(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION)
}

/// Render an agent-coding-session path as a flat transcript: the active
/// (head-ancestry) turns in order, speaker-labeled, with the generic
/// step/DAG scaffolding dropped. Non-turn event steps are omitted.
fn render_conversation_transcript(path: &Path, options: &RenderOptions) -> String {
    let mut out = String::new();

    if options.front_matter {
        write_path_front_matter(&mut out, path);
    }

    let title = path
        .meta
        .as_ref()
        .and_then(|m| m.title.as_deref())
        .unwrap_or(&path.path.id);
    writeln!(out, "# {title}").unwrap();
    writeln!(out).unwrap();

    write_transcript_context(&mut out, path);
    write_conversation_transcript_body(&mut out, path, options);
    out
}

/// Minimal header for a transcript: producing harness and base, without the
/// diffstat/step-count framing the generic path context carries.
fn write_transcript_context(out: &mut String, path: &Path) {
    if let Some(src) = path.meta.as_ref().and_then(|m| m.source.as_deref()) {
        writeln!(out, "**Source:** `{src}`").unwrap();
    }
    if let Some(base) = &path.path.base {
        let branch = base
            .branch
            .as_deref()
            .map(|b| format!(" (`{b}`)"))
            .unwrap_or_default();
        writeln!(out, "**Base:** `{}`{branch}", base.uri).unwrap();
    }
    writeln!(out).unwrap();
}

/// The turn-by-turn transcript body, shared by the single-path and graph
/// layouts. Walks active (head-ancestry) turns in causal order; abandoned
/// branches are summarized as a count, event steps are skipped.
fn write_conversation_transcript_body(out: &mut String, path: &Path, options: &RenderOptions) {
    let active = query::ancestors(&path.steps, &path.path.head);
    let sorted = topo_sort(&path.steps);

    let mut turns: Vec<&Step> = Vec::new();
    let mut omitted = 0usize;
    for &step in &sorted {
        let is_turn = step.change.values().any(|c| {
            c.structural.as_ref().map(|s| s.change_type.as_str()) == Some("conversation.append")
        });
        if !is_turn {
            continue; // event-only / non-turn steps
        }
        if !active.contains(step.step.id.as_str()) {
            omitted += 1;
            continue;
        }
        turns.push(step);
    }

    if options.detail == Detail::Summary {
        write_compact_transcript(out, &turns);
    } else {
        for step in &turns {
            let append = step
                .change
                .values()
                .find(|c| {
                    c.structural.as_ref().map(|s| s.change_type.as_str())
                        == Some("conversation.append")
                })
                .expect("turn step has a conversation.append change");
            let mut files: Vec<(&String, &ArtifactChange)> = step
                .change
                .iter()
                .filter(|(_, c)| {
                    c.structural.as_ref().map(|s| s.change_type.as_str()) == Some("file.write")
                })
                .collect();
            files.sort_by(|a, b| a.0.cmp(b.0));
            write_transcript_turn(out, append, &files, options);
        }
    }

    if omitted > 0 {
        writeln!(
            out,
            "_{omitted} abandoned turn{} omitted._",
            if omitted == 1 { "" } else { "s" }
        )
        .unwrap();
        writeln!(out).unwrap();
    }
}

/// Compact transcript: prose only. Turns with text render as speaker lines;
/// runs of text-less turns collapse into a per-tool breakdown line
/// (`*tools: Read (3), Write (1)*`). Empty turns produce no output.
fn write_compact_transcript(out: &mut String, turns: &[&Step]) {
    // Tool-name counts accumulated since the last speaker line, in first-seen
    // order.
    let mut pending: Vec<(String, usize)> = Vec::new();

    for step in turns {
        let Some(append) = step.change.values().find(|c| {
            c.structural.as_ref().map(|s| s.change_type.as_str()) == Some("conversation.append")
        }) else {
            continue;
        };
        let Some(s) = append.structural.as_ref() else {
            continue;
        };
        let extra = &s.extra;
        let text = extra
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let tools = extra.get("tool_uses").and_then(|v| v.as_array());

        if text.is_empty() {
            accumulate_tools(&mut pending, tools);
            continue;
        }

        flush_tool_breakdown(out, &mut pending);

        let role = extra.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let display = if role == "user" {
            text.to_string()
        } else {
            truncate_str(&text.replace('\n', " "), 200)
        };
        writeln!(out, "**{}:** {display}", speaker_label(role)).unwrap();
        writeln!(out).unwrap();

        accumulate_tools(&mut pending, tools);
    }

    flush_tool_breakdown(out, &mut pending);
}

/// Tally each `tool_uses[].name` into `pending` (first-seen order preserved).
fn accumulate_tools(pending: &mut Vec<(String, usize)>, tools: Option<&Vec<serde_json::Value>>) {
    let Some(tools) = tools else { return };
    for tool in tools {
        let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        match pending.iter_mut().find(|(n, _)| n == name) {
            Some((_, count)) => *count += 1,
            None => pending.push((name.to_string(), 1)),
        }
    }
}

/// Emit and clear the accumulated tool breakdown, if any.
fn flush_tool_breakdown(out: &mut String, pending: &mut Vec<(String, usize)>) {
    if pending.is_empty() {
        return;
    }
    let parts: Vec<String> = pending.iter().map(|(n, c)| format!("{n} ({c})")).collect();
    writeln!(out, "*tools: {}*", parts.join(", ")).unwrap();
    writeln!(out).unwrap();
    pending.clear();
}

/// One full-detail transcript turn: speaker line, then reasoning, tool calls
/// with inputs/results, delegations, file diffs, and the stop/tokens/cwd line.
fn write_transcript_turn(
    out: &mut String,
    append: &ArtifactChange,
    files: &[(&String, &ArtifactChange)],
    options: &RenderOptions,
) {
    let Some(s) = append.structural.as_ref() else {
        return;
    };
    let extra = &s.extra;
    let str_field = |k: &str| extra.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let text = str_field("text").trim();
    let thinking = str_field("thinking").trim();
    let tool_uses = extra
        .get("tool_uses")
        .and_then(|v| v.as_array())
        .filter(|t| !t.is_empty());
    let delegations = extra
        .get("delegations")
        .and_then(|v| v.as_array())
        .filter(|d| !d.is_empty());

    // Skip carrier turns that have nothing to show.
    if text.is_empty()
        && thinking.is_empty()
        && tool_uses.is_none()
        && delegations.is_none()
        && files.is_empty()
    {
        return;
    }

    let speaker = speaker_label(str_field("role"));
    if text.is_empty() {
        writeln!(out, "**{speaker}:**").unwrap();
    } else {
        writeln!(out, "**{speaker}:** {text}").unwrap();
    }
    writeln!(out).unwrap();

    if !thinking.is_empty() {
        writeln!(out, "**Reasoning:**").unwrap();
        writeln!(out).unwrap();
        for line in thinking.lines() {
            writeln!(out, "> {line}").unwrap();
        }
        writeln!(out).unwrap();
    }
    if let Some(tools) = tool_uses {
        writeln!(out, "**Tools:**").unwrap();
        for tool in tools {
            write_tool_use(out, tool);
        }
        writeln!(out).unwrap();
    }
    if let Some(delegs) = delegations {
        writeln!(out, "**Delegations:**").unwrap();
        for d in delegs {
            let agent = d.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
            let prompt = truncate_str(
                &d.get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .replace('\n', " "),
                80,
            );
            writeln!(out, "- `{agent}` \u{2014} {prompt}").unwrap();
        }
        writeln!(out).unwrap();
    }
    for (artifact, change) in files {
        write_conversation_file_write(out, artifact, change, options);
    }
    let meta_line = conversation_meta_line(extra);
    if !meta_line.is_empty() {
        writeln!(out, "*{meta_line}*").unwrap();
        writeln!(out).unwrap();
    }
}

/// Capitalized speaker label for a turn `role`.
fn speaker_label(role: &str) -> String {
    match role {
        "user" => "User".into(),
        "assistant" => "Assistant".into(),
        "system" => "System".into(),
        "" => "?".into(),
        other => {
            let mut chars = other.chars();
            let first = chars.next().unwrap();
            first.to_uppercase().collect::<String>() + chars.as_str()
        }
    }
}

/// One `tool_uses[]` entry as a list item: name, compact input, and result.
fn write_tool_use(out: &mut String, tool: &serde_json::Value) {
    let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("?");
    let input = tool
        .get("input")
        .map(compact_json)
        .filter(|s| !s.is_empty() && s != "{}" && s != "null")
        .map(|s| format!(" `{}`", truncate_str(&s, 80)))
        .unwrap_or_default();
    write!(out, "- `{name}`{input}").unwrap();
    if let Some(result) = tool.get("result") {
        let is_error = result
            .get("is_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let content = result.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let marker = if is_error { "error: " } else { "" };
        let content = truncate_str(&content.replace('\n', " "), 80);
        if !content.is_empty() {
            write!(out, " \u{2192} {marker}{content}").unwrap();
        } else if is_error {
            write!(out, " \u{2192} error").unwrap();
        }
    }
    writeln!(out).unwrap();
}

/// Compact one-line metadata: stop reason, token usage, environment.
fn conversation_meta_line(extra: &HashMap<String, serde_json::Value>) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(stop) = extra.get("stop_reason").and_then(|v| v.as_str()) {
        parts.push(format!("stop: {stop}"));
    }

    if let Some(usage) = extra.get("token_usage") {
        let n = |k: &str| usage.get(k).and_then(|v| v.as_u64());
        if let (Some(input), Some(output)) = (n("input_tokens"), n("output_tokens")) {
            let mut t = format!("tokens: {input} in, {output} out");
            if let Some(cached) = n("cache_read_tokens") {
                t.push_str(&format!(", {cached} cached"));
            }
            parts.push(t);
        }
    }

    if let Some(env) = extra.get("environment")
        && let Some(wd) = env.get("working_dir").and_then(|v| v.as_str())
    {
        let branch = env
            .get("vcs_branch")
            .and_then(|v| v.as_str())
            .map(|b| format!(" ({b})"))
            .unwrap_or_default();
        parts.push(format!("cwd: {wd}{branch}"));
    }

    parts.join(" \u{00b7} ")
}

/// Render a `file.write` sibling change: the path, operation, and diff.
fn write_conversation_file_write(
    out: &mut String,
    artifact: &str,
    change: &ArtifactChange,
    options: &RenderOptions,
) {
    let display = friendly_artifact_name(artifact);
    let op = change
        .structural
        .as_ref()
        .and_then(|s| s.extra.get("operation"))
        .and_then(|v| v.as_str())
        .map(|o| format!(" ({o})"))
        .unwrap_or_default();

    if options.detail == Detail::Summary {
        writeln!(out, "- wrote `{display}`{op}").unwrap();
        return;
    }

    writeln!(out, "**wrote `{display}`**{op}").unwrap();
    if let Some(raw) = &change.raw {
        writeln!(out).unwrap();
        writeln!(out, "```diff").unwrap();
        writeln!(out, "{raw}").unwrap();
        writeln!(out, "```").unwrap();
    }
    writeln!(out).unwrap();
}

/// Serialize a JSON value to a compact single-line string.
fn compact_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn change_annotation(change: &ArtifactChange) -> String {
    let mut parts = Vec::new();

    if let Some(raw) = &change.raw {
        let (add, del) = count_diff_lines(raw);
        if add > 0 || del > 0 {
            parts.push(format!("+{add} -{del}"));
        }
    }

    if let Some(structural) = &change.structural {
        parts.push(structural.change_type.clone());
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

fn count_diff_lines(raw: &str) -> (usize, usize) {
    let mut add = 0;
    let mut del = 0;
    for line in raw.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            add += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            del += 1;
        }
    }
    (add, del)
}

fn write_path_step(
    out: &mut String,
    step: &Step,
    options: &RenderOptions,
    is_dead: bool,
    is_head: bool,
) {
    // Header line with status markers
    let actor_short = actor_display(&step.step.actor);
    let markers = match (is_dead, is_head) {
        (true, _) => " [dead end]",
        (_, true) => " [head]",
        _ => "",
    };

    writeln!(
        out,
        "### {} \u{2014} {}{}",
        step.step.id, actor_short, markers
    )
    .unwrap();
    writeln!(out).unwrap();

    writeln!(out, "**Timestamp:** {}", step.step.timestamp).unwrap();

    // Parents
    if !step.step.parents.is_empty() {
        let parents: Vec<String> = step.step.parents.iter().map(|p| format!("`{p}`")).collect();
        writeln!(out, "**Parents:** {}", parents.join(", ")).unwrap();
    }

    writeln!(out).unwrap();

    // Intent
    if let Some(meta) = &step.meta
        && let Some(intent) = &meta.intent
    {
        writeln!(out, "> {intent}").unwrap();
        writeln!(out).unwrap();
    }

    // Refs
    if let Some(meta) = &step.meta
        && !meta.refs.is_empty()
    {
        for r in &meta.refs {
            writeln!(out, "- **{}:** `{}`", r.rel, r.href).unwrap();
        }
        writeln!(out).unwrap();
    }

    // Changes
    if !step.change.is_empty() {
        let mut artifacts: Vec<&String> = step.change.keys().collect();
        artifacts.sort();

        for artifact in artifacts {
            let change = &step.change[artifact];
            write_artifact_change(out, artifact, change, options);
        }
        if options.detail == Detail::Summary {
            writeln!(out).unwrap();
        }
    }
}

fn write_path_context(out: &mut String, path: &Path) {
    let ctx = source::detect(path);

    if let Some(identity) = &ctx.identity_line {
        writeln!(out, "{identity}").unwrap();
    }

    if let Some(base) = &path.path.base {
        write!(out, "**Base:** `{}`", base.uri).unwrap();
        if let Some(ref_str) = &base.ref_str {
            write!(out, " @ `{ref_str}`").unwrap();
        }
        if let Some(branch) = &base.branch {
            write!(out, " (`{branch}`)").unwrap();
        }
        writeln!(out).unwrap();
    }

    // Only show Head if no source-specific identity line (it's noise for PRs)
    if ctx.identity_line.is_none() {
        writeln!(out, "**Head:** `{}`", path.path.head).unwrap();
    }

    if let Some(meta) = &path.meta {
        if let Some(source) = &meta.source {
            writeln!(out, "**Source:** `{source}`").unwrap();
        }
        if let Some(intent) = &meta.intent {
            writeln!(out, "**Intent:** {intent}").unwrap();
        }
        if !meta.refs.is_empty() {
            for r in &meta.refs {
                writeln!(out, "- **{}:** `{}`", r.rel, r.href).unwrap();
            }
        }
    }

    // Diffstat — prefer source metadata, fall back to counting diffs
    let (total_add, total_del, file_count) = ctx
        .diffstat
        .unwrap_or_else(|| count_total_diff_lines(&path.steps));

    if total_add > 0 || total_del > 0 {
        write!(out, "**Changes:** +{total_add} \u{2212}{total_del}").unwrap();
        if let Some(f) = file_count {
            write!(out, " across {f} files").unwrap();
        }
        writeln!(out).unwrap();
    }

    // Summary stats
    let artifacts = query::all_artifacts(&path.steps);
    let dead_ends = query::dead_ends(&path.steps, &path.path.head);
    writeln!(
        out,
        "**Steps:** {} | **Artifacts:** {} | **Dead ends:** {}",
        path.steps.len(),
        artifacts.len(),
        dead_ends.len()
    )
    .unwrap();

    writeln!(out).unwrap();
}

fn write_dead_ends_section(out: &mut String, sorted: &[&Step], dead_end_set: &HashSet<&str>) {
    writeln!(out, "## Dead Ends").unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "These steps were attempted but did not contribute to the final result."
    )
    .unwrap();
    writeln!(out).unwrap();

    for step in sorted {
        if !dead_end_set.contains(step.step.id.as_str()) {
            continue;
        }
        let intent = step
            .meta
            .as_ref()
            .and_then(|m| m.intent.as_deref())
            .unwrap_or("(no intent recorded)");
        let parents: Vec<String> = step.step.parents.iter().map(|p| format!("`{p}`")).collect();
        let parent_str = if parents.is_empty() {
            "root".to_string()
        } else {
            parents.join(", ")
        };
        writeln!(
            out,
            "- **{}** ({}) \u{2014} {} | Parent: {}",
            step.step.id, step.step.actor, intent, parent_str
        )
        .unwrap();
    }
    writeln!(out).unwrap();
}

fn write_review_section(out: &mut String, sorted: &[&Step]) {
    // Collect review decisions and comments
    struct ReviewDecision<'a> {
        state: &'a str,
        actor: &'a str,
        body: Option<&'a str>,
    }
    struct ReviewComment<'a> {
        artifact: String,
        actor: &'a str,
        body: &'a str,
        diff_hunk: Option<&'a str>,
    }
    struct ConversationComment<'a> {
        actor: &'a str,
        body: &'a str,
    }

    let mut decisions: Vec<ReviewDecision<'_>> = Vec::new();
    let mut comments: Vec<ReviewComment<'_>> = Vec::new();
    let mut conversations: Vec<ConversationComment<'_>> = Vec::new();

    for step in sorted {
        for (artifact, change) in &step.change {
            let change_type = change
                .structural
                .as_ref()
                .map(|s| s.change_type.as_str())
                .unwrap_or("");
            match change_type {
                "review.decision" => {
                    let state = change
                        .structural
                        .as_ref()
                        .and_then(|s| s.extra.get("state"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("COMMENTED");
                    let body = change.raw.as_deref();
                    decisions.push(ReviewDecision {
                        state,
                        actor: &step.step.actor,
                        body,
                    });
                }
                "review.comment" => {
                    let body = change
                        .structural
                        .as_ref()
                        .and_then(|s| s.extra.get("body"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !body.is_empty() {
                        comments.push(ReviewComment {
                            artifact: friendly_artifact_name(artifact),
                            actor: &step.step.actor,
                            body,
                            diff_hunk: change.raw.as_deref(),
                        });
                    }
                }
                "review.conversation" => {
                    let body = change
                        .structural
                        .as_ref()
                        .and_then(|s| s.extra.get("body"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !body.is_empty() {
                        conversations.push(ConversationComment {
                            actor: &step.step.actor,
                            body,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    if decisions.is_empty() && comments.is_empty() && conversations.is_empty() {
        return;
    }

    writeln!(out, "## Review").unwrap();
    writeln!(out).unwrap();

    for d in &decisions {
        let marker = review_state_marker(d.state);
        let actor_short = d.actor.split(':').next_back().unwrap_or(d.actor);
        write!(out, "**{} {}** by {actor_short}", marker, d.state).unwrap();
        if let Some(body) = d.body
            && !body.is_empty()
        {
            writeln!(out, ":").unwrap();
            for line in body.lines() {
                writeln!(out, "> {line}").unwrap();
            }
        } else {
            writeln!(out).unwrap();
        }
        writeln!(out).unwrap();
    }

    if !conversations.is_empty() {
        writeln!(out, "### Discussion").unwrap();
        writeln!(out).unwrap();

        for c in &conversations {
            let actor_short = c.actor.split(':').next_back().unwrap_or(c.actor);
            writeln!(out, "**{actor_short}:**").unwrap();
            for line in c.body.lines() {
                writeln!(out, "> {line}").unwrap();
            }
            writeln!(out).unwrap();
        }
    }

    if !comments.is_empty() {
        writeln!(out, "### Inline comments").unwrap();
        writeln!(out).unwrap();

        for c in &comments {
            let actor_short = c.actor.split(':').next_back().unwrap_or(c.actor);
            writeln!(out, "**{}** \u{2014} {actor_short}:", c.artifact).unwrap();
            for line in c.body.lines() {
                writeln!(out, "> {line}").unwrap();
            }
            if let Some(hunk) = c.diff_hunk {
                writeln!(out).unwrap();
                writeln!(out, "```diff").unwrap();
                writeln!(out, "{hunk}").unwrap();
                writeln!(out, "```").unwrap();
            }
            writeln!(out).unwrap();
        }
    }
}

fn write_actors_section(out: &mut String, actors: &HashMap<String, toolpath::v1::ActorDefinition>) {
    writeln!(out, "## Actors").unwrap();
    writeln!(out).unwrap();

    let mut keys: Vec<&String> = actors.keys().collect();
    keys.sort();

    for key in keys {
        let def = &actors[key];
        let name = def.name.as_deref().unwrap_or(key);
        write!(out, "- **`{key}`** \u{2014} {name}").unwrap();
        if let Some(provider) = &def.provider {
            write!(out, " ({provider}").unwrap();
            if let Some(model) = &def.model {
                write!(out, ", {model}").unwrap();
            }
            write!(out, ")").unwrap();
        }
        writeln!(out).unwrap();
    }
    writeln!(out).unwrap();
}

// ============================================================================
// Front matter
// ============================================================================

fn write_step_front_matter(out: &mut String, step: &Step) {
    writeln!(out, "---").unwrap();
    writeln!(out, "type: step").unwrap();
    writeln!(out, "id: {}", step.step.id).unwrap();
    writeln!(out, "actor: {}", step.step.actor).unwrap();
    writeln!(out, "timestamp: {}", step.step.timestamp).unwrap();
    if !step.step.parents.is_empty() {
        let parents: Vec<&str> = step.step.parents.iter().map(|s| s.as_str()).collect();
        writeln!(out, "parents: [{}]", parents.join(", ")).unwrap();
    }
    let mut artifacts: Vec<&str> = step.change.keys().map(|k| k.as_str()).collect();
    artifacts.sort();
    if !artifacts.is_empty() {
        writeln!(out, "artifacts: [{}]", artifacts.join(", ")).unwrap();
    }
    writeln!(out, "---").unwrap();
    writeln!(out).unwrap();
}

fn write_path_front_matter(out: &mut String, path: &Path) {
    writeln!(out, "---").unwrap();
    writeln!(out, "type: path").unwrap();
    writeln!(out, "id: {}", path.path.id).unwrap();
    writeln!(out, "head: {}", path.path.head).unwrap();
    if let Some(base) = &path.path.base {
        writeln!(out, "base: {}", base.uri).unwrap();
        if let Some(ref_str) = &base.ref_str {
            writeln!(out, "base_ref: {ref_str}").unwrap();
        }
        if let Some(branch) = &base.branch {
            writeln!(out, "base_branch: {branch}").unwrap();
        }
    }
    writeln!(out, "steps: {}", path.steps.len()).unwrap();
    let actors = query::all_actors(&path.steps);
    let mut actor_list: Vec<&str> = actors.iter().copied().collect();
    actor_list.sort();
    writeln!(out, "actors: [{}]", actor_list.join(", ")).unwrap();
    let mut artifacts: Vec<&str> = query::all_artifacts(&path.steps).into_iter().collect();
    artifacts.sort();
    writeln!(out, "artifacts: [{}]", artifacts.join(", ")).unwrap();
    let dead_ends = query::dead_ends(&path.steps, &path.path.head);
    writeln!(out, "dead_ends: {}", dead_ends.len()).unwrap();
    writeln!(out, "---").unwrap();
    writeln!(out).unwrap();
}

fn write_graph_front_matter(out: &mut String, graph: &Graph) {
    writeln!(out, "---").unwrap();
    writeln!(out, "type: graph").unwrap();
    writeln!(out, "id: {}", graph.graph.id).unwrap();
    let inline_count = graph
        .paths
        .iter()
        .filter(|p| matches!(p, PathOrRef::Path(_)))
        .count();
    let ref_count = graph
        .paths
        .iter()
        .filter(|p| matches!(p, PathOrRef::Ref(_)))
        .count();
    writeln!(out, "paths: {inline_count}").unwrap();
    if ref_count > 0 {
        writeln!(out, "external_refs: {ref_count}").unwrap();
    }
    writeln!(out, "---").unwrap();
    writeln!(out).unwrap();
}

// ============================================================================
// Utilities
// ============================================================================

/// Format an actor string for display: `"agent:claude-code/session-abc"` -> `"agent:claude-code/session-abc"`.
///
/// We keep the full actor string — it's the anchor that lets an LLM
/// reference back into the toolpath document.
fn actor_display(actor: &str) -> &str {
    actor
}

/// Convert artifact URIs to friendlier display names:
/// - `review://path/to/file.rs#L42` -> `path/to/file.rs:42`
/// - `ci://checks/test` -> `test`
/// - `review://conversation` -> `conversation`
/// - `review://decision` -> `decision`
fn friendly_artifact_name(artifact: &str) -> String {
    if let Some(rest) = artifact.strip_prefix("review://") {
        if let Some(pos) = rest.rfind("#L") {
            format!("{}:{}", &rest[..pos], &rest[pos + 2..])
        } else {
            rest.to_string()
        }
    } else if let Some(rest) = artifact.strip_prefix("ci://checks/") {
        rest.to_string()
    } else {
        artifact.to_string()
    }
}

/// Truncate a string to a maximum number of characters, adding "..." if truncated.
fn truncate_str(s: &str, max: usize) -> String {
    let s = s.lines().collect::<Vec<_>>().join(" ").trim().to_string();
    if s.chars().count() <= max {
        s
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}...")
    }
}

/// Text marker for review states.
fn review_state_marker(state: &str) -> &'static str {
    match state {
        "APPROVED" => "[approved]",
        "CHANGES_REQUESTED" => "[changes requested]",
        "COMMENTED" => "[commented]",
        "DISMISSED" => "[dismissed]",
        _ => "[review]",
    }
}

/// Count total diff lines across all steps (excluding review/CI artifacts).
fn count_total_diff_lines(steps: &[Step]) -> (u64, u64, Option<u64>) {
    let mut total_add: u64 = 0;
    let mut total_del: u64 = 0;
    let mut files: HashSet<&str> = HashSet::new();
    for step in steps {
        for (artifact, change) in &step.change {
            // Skip review and CI artifacts
            if artifact.starts_with("review://") || artifact.starts_with("ci://") {
                continue;
            }
            if let Some(raw) = &change.raw {
                let (a, d) = count_diff_lines(raw);
                total_add += a as u64;
                total_del += d as u64;
                files.insert(artifact.as_str());
            }
        }
    }
    let file_count = if files.is_empty() {
        None
    } else {
        Some(files.len() as u64)
    };
    (total_add, total_del, file_count)
}

/// Compute a friendly date range string from step timestamps.
/// Returns empty string if no timestamps found.
pub(crate) fn friendly_date_range(steps: &[Step]) -> String {
    if steps.is_empty() {
        return String::new();
    }

    let mut first: Option<&str> = None;
    let mut last: Option<&str> = None;

    for step in steps {
        let ts = step.step.timestamp.as_str();
        if ts.is_empty() || ts.starts_with("1970") {
            continue;
        }
        match first {
            None => {
                first = Some(ts);
                last = Some(ts);
            }
            Some(f) => {
                if ts < f {
                    first = Some(ts);
                }
                if ts > last.unwrap_or("") {
                    last = Some(ts);
                }
            }
        }
    }

    let Some(first) = first else {
        return String::new();
    };
    let last = last.unwrap_or(first);

    // Extract YYYY-MM-DD from ISO 8601
    let first_date = &first[..first.len().min(10)];
    let last_date = &last[..last.len().min(10)];

    let Some(first_fmt) = format_date(first_date) else {
        return String::new();
    };

    if first_date == last_date {
        return first_fmt;
    }

    let Some(last_fmt) = format_date(last_date) else {
        return first_fmt;
    };

    // Same month and year
    let first_parts: Vec<&str> = first_date.split('-').collect();
    let last_parts: Vec<&str> = last_date.split('-').collect();

    if first_parts.len() == 3 && last_parts.len() == 3 {
        if first_parts[0] == last_parts[0] && first_parts[1] == last_parts[1] {
            // Same month: "Feb 26\u{2013}27, 2026"
            let month = month_abbrev(first_parts[1]);
            let day1 = first_parts[2].trim_start_matches('0');
            let day2 = last_parts[2].trim_start_matches('0');
            return format!("{month} {day1}\u{2013}{day2}, {}", first_parts[0]);
        }
        if first_parts[0] == last_parts[0] {
            // Same year: "Feb 26 \u{2013} Mar 1, 2026"
            let month1 = month_abbrev(first_parts[1]);
            let day1 = first_parts[2].trim_start_matches('0');
            let month2 = month_abbrev(last_parts[1]);
            let day2 = last_parts[2].trim_start_matches('0');
            return format!(
                "{month1} {day1} \u{2013} {month2} {day2}, {}",
                first_parts[0]
            );
        }
    }

    // Different years
    format!("{first_fmt} \u{2013} {last_fmt}")
}

/// Format a YYYY-MM-DD date string to "Mon DD, YYYY".
fn format_date(date: &str) -> Option<String> {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let month = month_abbrev(parts[1]);
    let day = parts[2].trim_start_matches('0');
    Some(format!("{month} {day}, {}", parts[0]))
}

fn month_abbrev(month: &str) -> &'static str {
    match month {
        "01" => "Jan",
        "02" => "Feb",
        "03" => "Mar",
        "04" => "Apr",
        "05" => "May",
        "06" => "Jun",
        "07" => "Jul",
        "08" => "Aug",
        "09" => "Sep",
        "10" => "Oct",
        "11" => "Nov",
        "12" => "Dec",
        _ => "???",
    }
}

/// Text marker for CI conclusions.
fn ci_conclusion_marker(conclusion: &str) -> &'static str {
    match conclusion {
        "success" => "[pass]",
        "failure" => "[fail]",
        "cancelled" | "timed_out" => "[cancelled]",
        "skipped" => "[skip]",
        "neutral" => "[neutral]",
        _ => "[unknown]",
    }
}

/// Format a set of actors as a compact comma-separated string.
fn format_actor_list(actors: &HashSet<&str>) -> String {
    let mut list: Vec<&str> = actors.iter().copied().collect();
    list.sort();
    list.iter()
        .map(|a| format!("`{a}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Topological sort of steps respecting parent edges.
/// Falls back to input order for steps without declared parents.
fn topo_sort<'a>(steps: &'a [Step]) -> Vec<&'a Step> {
    let index: HashMap<&str, &Step> = steps.iter().map(|s| (s.step.id.as_str(), s)).collect();
    let ids: Vec<&str> = steps.iter().map(|s| s.step.id.as_str()).collect();
    let id_set: HashSet<&str> = ids.iter().copied().collect();

    // Kahn's algorithm
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();

    for &id in &ids {
        in_degree.entry(id).or_insert(0);
        children.entry(id).or_default();
    }

    for step in steps {
        for parent in &step.step.parents {
            if id_set.contains(parent.as_str()) {
                *in_degree.entry(step.step.id.as_str()).or_insert(0) += 1;
                children
                    .entry(parent.as_str())
                    .or_default()
                    .push(step.step.id.as_str());
            }
        }
    }

    // Seed queue with roots, ordered by position in input
    let mut queue: Vec<&str> = ids
        .iter()
        .copied()
        .filter(|id| in_degree.get(id).copied().unwrap_or(0) == 0)
        .collect();

    let mut result: Vec<&'a Step> = Vec::with_capacity(steps.len());

    while let Some(id) = queue.first().copied() {
        queue.remove(0);
        if let Some(step) = index.get(id) {
            result.push(step);
        }
        if let Some(kids) = children.get(id) {
            for &child in kids {
                let deg = in_degree.get_mut(child).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push(child);
                }
            }
        }
    }

    // Append any remaining (cycle or orphan) steps in original order
    let placed: HashSet<&str> = result.iter().map(|s| s.step.id.as_str()).collect();
    for step in steps {
        if !placed.contains(step.step.id.as_str()) {
            result.push(step);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath::v1::{
        Base, Graph, GraphIdentity, GraphMeta, Path, PathIdentity, PathMeta, PathOrRef, PathRef,
        Ref, Step, StructuralChange,
    };

    fn make_step(id: &str, actor: &str, parents: &[&str]) -> Step {
        let mut step = Step::new(id, actor, "2026-01-29T10:00:00Z")
            .with_raw_change("src/main.rs", "@@ -1 +1 @@\n-old\n+new");
        for p in parents {
            step = step.with_parent(*p);
        }
        step
    }

    fn make_step_with_intent(id: &str, actor: &str, parents: &[&str], intent: &str) -> Step {
        make_step(id, actor, parents).with_intent(intent)
    }

    // ── render_step ──────────────────────────────────────────────────────

    #[test]
    fn test_render_step_basic() {
        let step = make_step("s1", "human:alex", &[]);
        let opts = RenderOptions::default();
        let md = render_step(&step, &opts);

        assert!(md.starts_with("# s1"));
        assert!(md.contains("human:alex"));
        assert!(md.contains("src/main.rs"));
    }

    #[test]
    fn test_render_step_with_intent() {
        let step = make_step_with_intent("s1", "human:alex", &[], "Fix the bug");
        let opts = RenderOptions::default();
        let md = render_step(&step, &opts);

        assert!(md.contains("> Fix the bug"));
    }

    #[test]
    fn test_render_step_with_parents() {
        let step = make_step("s2", "agent:claude", &["s1"]);
        let opts = RenderOptions::default();
        let md = render_step(&step, &opts);

        assert!(md.contains("`s1`"));
    }

    #[test]
    fn test_render_step_with_front_matter() {
        let step = make_step("s1", "human:alex", &[]);
        let opts = RenderOptions {
            front_matter: true,
            ..Default::default()
        };
        let md = render_step(&step, &opts);

        assert!(md.starts_with("---\n"));
        assert!(md.contains("type: step"));
        assert!(md.contains("id: s1"));
        assert!(md.contains("actor: human:alex"));
    }

    #[test]
    fn test_render_step_full_detail() {
        let step = make_step("s1", "human:alex", &[]);
        let opts = RenderOptions {
            detail: Detail::Full,
            ..Default::default()
        };
        let md = render_step(&step, &opts);

        assert!(md.contains("```diff"));
        assert!(md.contains("-old"));
        assert!(md.contains("+new"));
    }

    #[test]
    fn test_render_step_summary_has_diffstat() {
        let step = make_step("s1", "human:alex", &[]);
        let opts = RenderOptions::default();
        let md = render_step(&step, &opts);

        assert!(md.contains("+1 -1"));
    }

    // ── render_path ──────────────────────────────────────────────────────

    #[test]
    fn test_render_path_basic() {
        let s1 = make_step_with_intent("s1", "human:alex", &[], "Start");
        let s2 = make_step_with_intent("s2", "agent:claude", &["s1"], "Continue");
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: Some(Base::vcs("github:org/repo", "abc123")),
                head: "s2".into(),
                graph_ref: None,
            },
            steps: vec![s1, s2],
            meta: Some(PathMeta {
                title: Some("My PR".into()),
                ..Default::default()
            }),
        };
        let opts = RenderOptions::default();
        let md = render_path(&path, &opts);

        assert!(md.starts_with("# My PR"));
        assert!(md.contains("github:org/repo"));
        assert!(md.contains("## Timeline"));
        assert!(md.contains("### s1"));
        assert!(md.contains("### s2"));
        assert!(md.contains("[head]"));
    }

    #[test]
    fn test_render_path_with_dead_ends() {
        let s1 = make_step("s1", "human:alex", &[]);
        let s2 = make_step_with_intent("s2", "agent:claude", &["s1"], "Good approach");
        let s2a = make_step_with_intent("s2a", "agent:claude", &["s1"], "Bad approach (abandoned)");
        let s3 = make_step("s3", "human:alex", &["s2"]);
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s3".into(),
                graph_ref: None,
            },
            steps: vec![s1, s2, s2a, s3],
            meta: None,
        };
        let opts = RenderOptions::default();
        let md = render_path(&path, &opts);

        assert!(md.contains("[dead end]"));
        assert!(md.contains("## Dead Ends"));
        assert!(md.contains("Bad approach (abandoned)"));
    }

    #[test]
    fn test_render_path_with_front_matter() {
        let s1 = make_step("s1", "human:alex", &[]);
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![s1],
            meta: None,
        };
        let opts = RenderOptions {
            front_matter: true,
            ..Default::default()
        };
        let md = render_path(&path, &opts);

        assert!(md.starts_with("---\n"));
        assert!(md.contains("type: path"));
        assert!(md.contains("id: p1"));
        assert!(md.contains("steps: 1"));
        assert!(md.contains("dead_ends: 0"));
    }

    #[test]
    fn test_render_path_stats_line() {
        let s1 = make_step("s1", "human:alex", &[]);
        let s2 = make_step("s2", "agent:claude", &["s1"]);
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s2".into(),
                graph_ref: None,
            },
            steps: vec![s1, s2],
            meta: None,
        };
        let md = render_path(&path, &RenderOptions::default());

        assert!(md.contains("**Steps:** 2"));
        assert!(md.contains("**Artifacts:** 1"));
        assert!(md.contains("**Dead ends:** 0"));
    }

    #[test]
    fn test_render_path_with_refs() {
        let s1 = make_step("s1", "human:alex", &[]);
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![s1],
            meta: Some(PathMeta {
                refs: vec![Ref {
                    rel: "fixes".into(),
                    href: "issue://github/org/repo/issues/42".into(),
                }],
                ..Default::default()
            }),
        };
        let md = render_path(&path, &RenderOptions::default());

        assert!(md.contains("**fixes:**"));
        assert!(md.contains("issue://github/org/repo/issues/42"));
    }

    // ── render_graph ─────────────────────────────────────────────────────

    #[test]
    fn test_render_graph_basic() {
        let s1 = make_step_with_intent("s1", "human:alex", &[], "First");
        let s2 = make_step_with_intent("s2", "agent:claude", &["s1"], "Second");
        let path1 = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: Some(Base::vcs("github:org/repo", "abc")),
                head: "s2".into(),
                graph_ref: None,
            },
            steps: vec![s1, s2],
            meta: Some(PathMeta {
                title: Some("PR #42".into()),
                ..Default::default()
            }),
        };

        let s3 = make_step("s3", "human:bob", &[]);
        let path2 = Path {
            path: PathIdentity {
                id: "p2".into(),
                base: None,
                head: "s3".into(),
                graph_ref: None,
            },
            steps: vec![s3],
            meta: Some(PathMeta {
                title: Some("PR #43".into()),
                ..Default::default()
            }),
        };

        let graph = Graph {
            graph: GraphIdentity { id: "g1".into() },
            paths: vec![
                PathOrRef::Path(Box::new(path1)),
                PathOrRef::Path(Box::new(path2)),
            ],
            meta: Some(GraphMeta {
                title: Some("Release v2.0".into()),
                ..Default::default()
            }),
        };
        let opts = RenderOptions::default();
        let md = render_graph(&graph, &opts);

        assert!(md.starts_with("# Release v2.0"));
        assert!(md.contains("| PR #42"));
        assert!(md.contains("| PR #43"));
        assert!(md.contains("## PR #42"));
        assert!(md.contains("## PR #43"));
    }

    #[test]
    fn test_render_graph_with_refs() {
        let graph = Graph {
            graph: GraphIdentity { id: "g1".into() },
            paths: vec![PathOrRef::Ref(PathRef {
                ref_url: "https://example.com/path.json".into(),
            })],
            meta: None,
        };
        let md = render_graph(&graph, &RenderOptions::default());

        assert!(md.contains("External references"));
        assert!(md.contains("example.com/path.json"));
    }

    #[test]
    fn test_render_graph_with_front_matter() {
        let s1 = make_step("s1", "human:alex", &[]);
        let path1 = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![s1],
            meta: None,
        };
        let graph = Graph {
            graph: GraphIdentity { id: "g1".into() },
            paths: vec![
                PathOrRef::Path(Box::new(path1)),
                PathOrRef::Ref(PathRef {
                    ref_url: "https://example.com".into(),
                }),
            ],
            meta: None,
        };
        let opts = RenderOptions {
            front_matter: true,
            ..Default::default()
        };
        let md = render_graph(&graph, &opts);

        assert!(md.starts_with("---\n"));
        assert!(md.contains("type: graph"));
        assert!(md.contains("paths: 1"));
        assert!(md.contains("external_refs: 1"));
    }

    #[test]
    fn test_render_graph_with_meta_refs() {
        let graph = Graph {
            graph: GraphIdentity { id: "g1".into() },
            paths: vec![],
            meta: Some(GraphMeta {
                title: Some("Release".into()),
                refs: vec![Ref {
                    rel: "milestone".into(),
                    href: "issue://github/org/repo/milestone/5".into(),
                }],
                ..Default::default()
            }),
        };
        let md = render_graph(&graph, &RenderOptions::default());

        assert!(md.contains("**milestone:**"));
    }

    // ── render (dispatch) ────────────────────────────────────────────────

    #[test]
    fn test_render_single_path_graph_uses_path_layout() {
        let s1 = make_step("s1", "human:alex", &[]);
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![s1],
            meta: None,
        };
        let graph = Graph::from_path(path);
        let md = render(&graph, &RenderOptions::default());
        assert!(md.contains("## Timeline"));
    }

    #[test]
    fn test_render_empty_graph_uses_graph_layout() {
        let graph = Graph {
            graph: GraphIdentity { id: "g1".into() },
            paths: vec![],
            meta: Some(GraphMeta {
                title: Some("My Graph".into()),
                ..Default::default()
            }),
        };
        let md = render(&graph, &RenderOptions::default());
        assert!(md.contains("# My Graph"));
    }

    // ── topo_sort ────────────────────────────────────────────────────────

    #[test]
    fn test_topo_sort_linear() {
        let s1 = make_step("s1", "human:alex", &[]);
        let s2 = make_step("s2", "agent:claude", &["s1"]);
        let s3 = make_step("s3", "human:alex", &["s2"]);
        let steps = vec![s3.clone(), s1.clone(), s2.clone()]; // scrambled input
        let sorted = topo_sort(&steps);
        let ids: Vec<&str> = sorted.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(ids, vec!["s1", "s2", "s3"]);
    }

    #[test]
    fn test_topo_sort_branching() {
        let s1 = make_step("s1", "human:alex", &[]);
        let s2a = make_step("s2a", "agent:claude", &["s1"]);
        let s2b = make_step("s2b", "agent:claude", &["s1"]);
        let s3 = make_step("s3", "human:alex", &["s2a", "s2b"]);
        let steps = vec![s1, s2a, s2b, s3];
        let sorted = topo_sort(&steps);
        let ids: Vec<&str> = sorted.iter().map(|s| s.step.id.as_str()).collect();

        // s1 must come first, s3 must come last
        assert_eq!(ids[0], "s1");
        assert_eq!(ids[3], "s3");
    }

    #[test]
    fn test_topo_sort_preserves_input_order_for_roots() {
        let s1 = make_step("s1", "human:alex", &[]);
        let s2 = make_step("s2", "human:bob", &[]);
        let steps = vec![s1, s2];
        let sorted = topo_sort(&steps);
        let ids: Vec<&str> = sorted.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(ids, vec!["s1", "s2"]);
    }

    // ── count_diff_lines ─────────────────────────────────────────────────

    #[test]
    fn test_count_diff_lines() {
        let diff = "@@ -1,3 +1,4 @@\n-old1\n-old2\n+new1\n+new2\n+new3\n context";
        let (add, del) = count_diff_lines(diff);
        assert_eq!(add, 3);
        assert_eq!(del, 2);
    }

    #[test]
    fn test_count_diff_lines_ignores_triple_prefix() {
        let diff = "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new";
        let (add, del) = count_diff_lines(diff);
        assert_eq!(add, 1);
        assert_eq!(del, 1);
    }

    #[test]
    fn test_count_diff_lines_empty() {
        assert_eq!(count_diff_lines(""), (0, 0));
    }

    // ── structural changes ───────────────────────────────────────────────

    #[test]
    fn test_render_structural_change_summary() {
        let mut step = Step::new("s1", "human:alex", "2026-01-29T10:00:00Z");
        step.change.insert(
            "src/main.rs".into(),
            toolpath::v1::ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "rename_function".into(),
                    extra: Default::default(),
                }),
            },
        );
        let md = render_step(&step, &RenderOptions::default());
        assert!(md.contains("rename_function"));
    }

    #[test]
    fn test_render_structural_change_full() {
        let mut extra = std::collections::HashMap::new();
        extra.insert("from".to_string(), serde_json::json!("foo"));
        extra.insert("to".to_string(), serde_json::json!("bar"));
        let mut step = Step::new("s1", "human:alex", "2026-01-29T10:00:00Z");
        step.change.insert(
            "src/main.rs".into(),
            toolpath::v1::ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "rename_function".into(),
                    extra,
                }),
            },
        );
        let md = render_step(
            &step,
            &RenderOptions {
                detail: Detail::Full,
                ..Default::default()
            },
        );
        assert!(md.contains("Structural: `rename_function`"));
    }

    // ── actors section ───────────────────────────────────────────────────

    #[test]
    fn test_render_path_with_actors() {
        let s1 = make_step("s1", "human:alex", &[]);
        let mut actors = std::collections::HashMap::new();
        actors.insert(
            "human:alex".into(),
            toolpath::v1::ActorDefinition {
                name: Some("Alex".into()),
                provider: None,
                model: None,
                identities: vec![],
                keys: vec![],
            },
        );
        actors.insert(
            "agent:claude-code".into(),
            toolpath::v1::ActorDefinition {
                name: Some("Claude Code".into()),
                provider: Some("Anthropic".into()),
                model: Some("claude-sonnet-4-20250514".into()),
                identities: vec![],
                keys: vec![],
            },
        );
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![s1],
            meta: Some(PathMeta {
                actors: Some(actors),
                ..Default::default()
            }),
        };
        let md = render_path(&path, &RenderOptions::default());

        assert!(md.contains("## Actors"));
        assert!(md.contains("Alex"));
        assert!(md.contains("Claude Code"));
        assert!(md.contains("Anthropic"));
    }

    // ── full detail mode ─────────────────────────────────────────────────

    #[test]
    fn test_render_path_full_detail() {
        let s1 = make_step("s1", "human:alex", &[]);
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![s1],
            meta: None,
        };
        let opts = RenderOptions {
            detail: Detail::Full,
            ..Default::default()
        };
        let md = render_path(&path, &opts);

        assert!(md.contains("```diff"));
        assert!(md.contains("-old"));
        assert!(md.contains("+new"));
    }

    // ── edge cases ───────────────────────────────────────────────────────

    #[test]
    fn test_render_path_no_title() {
        let s1 = make_step("s1", "human:alex", &[]);
        let path = Path {
            path: PathIdentity {
                id: "path-42".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![s1],
            meta: None,
        };
        let md = render_path(&path, &RenderOptions::default());
        assert!(md.starts_with("# path-42"));
    }

    #[test]
    fn test_render_step_no_changes() {
        let step = Step::new("s1", "human:alex", "2026-01-29T10:00:00Z");
        let md = render_step(&step, &RenderOptions::default());
        assert!(md.contains("# s1"));
        assert!(!md.contains("## Changes"));
    }

    #[test]
    fn test_render_graph_empty_paths() {
        let graph = Graph {
            graph: GraphIdentity { id: "g1".into() },
            paths: vec![],
            meta: None,
        };
        let md = render_graph(&graph, &RenderOptions::default());
        assert!(md.contains("# g1"));
    }

    // ── review/CI rendering ───────────────────────────────────────────

    fn make_review_comment_step(id: &str, actor: &str, artifact: &str, body: &str) -> Step {
        let mut extra = std::collections::HashMap::new();
        extra.insert("body".to_string(), serde_json::json!(body));
        let mut step = Step::new(id, actor, "2026-01-29T10:00:00Z");
        step.change.insert(
            artifact.to_string(),
            ArtifactChange {
                raw: Some("@@ -1,3 +1,4 @@\n fn example() {\n+    let x = 42;\n }".to_string()),
                structural: Some(StructuralChange {
                    change_type: "review.comment".into(),
                    extra,
                }),
            },
        );
        step
    }

    fn make_review_decision_step(id: &str, actor: &str, state: &str, body: &str) -> Step {
        let mut extra = std::collections::HashMap::new();
        extra.insert("state".to_string(), serde_json::json!(state));
        let mut step = Step::new(id, actor, "2026-01-29T11:00:00Z");
        step.change.insert(
            "review://decision".to_string(),
            ArtifactChange {
                raw: if body.is_empty() {
                    None
                } else {
                    Some(body.to_string())
                },
                structural: Some(StructuralChange {
                    change_type: "review.decision".into(),
                    extra,
                }),
            },
        );
        step
    }

    fn make_ci_step(id: &str, name: &str, conclusion: &str) -> Step {
        let mut extra = std::collections::HashMap::new();
        extra.insert("conclusion".to_string(), serde_json::json!(conclusion));
        extra.insert(
            "url".to_string(),
            serde_json::json!("https://github.com/acme/widgets/actions/runs/123"),
        );
        let mut step = Step::new(id, "ci:github-actions", "2026-01-29T12:00:00Z");
        step.change.insert(
            format!("ci://checks/{}", name),
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "ci.run".into(),
                    extra,
                }),
            },
        );
        step
    }

    #[test]
    fn test_render_review_comment_summary() {
        let step = make_review_comment_step(
            "s1",
            "human:bob",
            "review://src/main.rs#L42",
            "Consider using a constant here.",
        );
        let md = render_step(&step, &RenderOptions::default());

        // Should show friendly artifact name and body
        assert!(md.contains("src/main.rs:42"));
        assert!(md.contains("Consider using a constant here."));
        // Should NOT show the opaque review:// URI
        assert!(!md.contains("review://"));
    }

    #[test]
    fn test_render_review_comment_full() {
        let step = make_review_comment_step(
            "s1",
            "human:bob",
            "review://src/main.rs#L42",
            "Consider using a constant here.",
        );
        let md = render_step(
            &step,
            &RenderOptions {
                detail: Detail::Full,
                ..Default::default()
            },
        );

        // Should show body as blockquote
        assert!(md.contains("> Consider using a constant here."));
        // Should show diff_hunk
        assert!(md.contains("```diff"));
        assert!(md.contains("let x = 42"));
    }

    #[test]
    fn test_render_review_decision_summary() {
        let step = make_review_decision_step("s1", "human:dave", "APPROVED", "LGTM!");
        let md = render_step(&step, &RenderOptions::default());

        assert!(md.contains("[approved]"));
        assert!(md.contains("APPROVED"));
        assert!(md.contains("LGTM!"));
    }

    #[test]
    fn test_render_ci_summary() {
        let step = make_ci_step("s1", "test", "success");
        let md = render_step(&step, &RenderOptions::default());

        assert!(md.contains("test"));
        assert!(md.contains("[pass]"));
        assert!(md.contains("success"));
        // Should NOT show ci://checks/ prefix
        assert!(!md.contains("ci://checks/"));
    }

    #[test]
    fn test_render_ci_failure() {
        let step = make_ci_step("s1", "lint", "failure");
        let md = render_step(&step, &RenderOptions::default());

        assert!(md.contains("lint"));
        assert!(md.contains("[fail]"));
        assert!(md.contains("failure"));
    }

    #[test]
    fn test_render_ci_full_with_url() {
        let step = make_ci_step("s1", "test", "success");
        let md = render_step(
            &step,
            &RenderOptions {
                detail: Detail::Full,
                ..Default::default()
            },
        );

        assert!(md.contains("details"));
        assert!(md.contains("actions/runs/123"));
    }

    #[test]
    fn test_render_review_section() {
        let s1 = make_step("s1", "human:alice", &[]);
        let s2 = make_review_comment_step(
            "s2",
            "human:bob",
            "review://src/main.rs#L42",
            "Consider using a constant.",
        );
        let s3 = make_review_decision_step("s3", "human:dave", "APPROVED", "Ship it!");
        let mut s2 = s2;
        s2 = s2.with_parent("s1");
        let mut s3 = s3;
        s3 = s3.with_parent("s2");
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s3".into(),
                graph_ref: None,
            },
            steps: vec![s1, s2, s3],
            meta: None,
        };
        let md = render_path(&path, &RenderOptions::default());

        assert!(md.contains("## Review"));
        assert!(md.contains("APPROVED"));
        assert!(md.contains("Ship it!"));
        assert!(md.contains("### Inline comments"));
        assert!(md.contains("src/main.rs:42"));
        assert!(md.contains("Consider using a constant."));
    }

    #[test]
    fn test_render_no_review_section_without_reviews() {
        let s1 = make_step("s1", "human:alex", &[]);
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![s1],
            meta: None,
        };
        let md = render_path(&path, &RenderOptions::default());

        assert!(!md.contains("## Review"));
    }

    // ── PR identity and diffstat ──────────────────────────────────────

    #[test]
    fn test_render_pr_identity() {
        let s1 = make_step("s1", "human:alice", &[]);
        let mut extra = std::collections::HashMap::new();
        let github = serde_json::json!({
            "number": 42,
            "author": "alice",
            "state": "open",
            "draft": false,
            "merged": false,
            "additions": 150,
            "deletions": 30,
            "changed_files": 5
        });
        extra.insert("github".to_string(), github);
        let path = Path {
            path: PathIdentity {
                id: "pr-42".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![s1],
            meta: Some(PathMeta {
                title: Some("Add feature".into()),
                extra,
                ..Default::default()
            }),
        };
        let md = render_path(&path, &RenderOptions::default());

        assert!(md.contains("**PR #42**"));
        assert!(md.contains("by alice"));
        assert!(md.contains("open"));
        assert!(md.contains("+150"));
        assert!(md.contains("\u{2212}30"));
        assert!(md.contains("5 files"));
        // Should NOT show opaque head ID
        assert!(!md.contains("**Head:**"));
    }

    #[test]
    fn test_render_no_pr_identity_without_github_meta() {
        let s1 = make_step("s1", "human:alex", &[]);
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![s1],
            meta: None,
        };
        let md = render_path(&path, &RenderOptions::default());

        // Should show Head when no GitHub meta
        assert!(md.contains("**Head:**"));
        assert!(!md.contains("**PR #"));
    }

    // ── friendly helpers ──────────────────────────────────────────────

    #[test]
    fn test_friendly_artifact_name() {
        assert_eq!(
            friendly_artifact_name("review://src/main.rs#L42"),
            "src/main.rs:42"
        );
        assert_eq!(friendly_artifact_name("ci://checks/test"), "test");
        assert_eq!(friendly_artifact_name("review://decision"), "decision");
        assert_eq!(friendly_artifact_name("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn test_friendly_date_range_same_day() {
        let s1 = Step::new("s1", "human:alex", "2026-02-26T10:00:00Z");
        let s2 = Step::new("s2", "human:alex", "2026-02-26T14:00:00Z");
        assert_eq!(friendly_date_range(&[s1, s2]), "Feb 26, 2026");
    }

    #[test]
    fn test_friendly_date_range_same_month() {
        let s1 = Step::new("s1", "human:alex", "2026-02-26T10:00:00Z");
        let s2 = Step::new("s2", "human:alex", "2026-02-27T14:00:00Z");
        assert_eq!(friendly_date_range(&[s1, s2]), "Feb 26\u{2013}27, 2026");
    }

    #[test]
    fn test_friendly_date_range_different_months() {
        let s1 = Step::new("s1", "human:alex", "2026-02-26T10:00:00Z");
        let s2 = Step::new("s2", "human:alex", "2026-03-01T14:00:00Z");
        assert_eq!(
            friendly_date_range(&[s1, s2]),
            "Feb 26 \u{2013} Mar 1, 2026"
        );
    }

    #[test]
    fn test_friendly_date_range_empty() {
        assert_eq!(friendly_date_range(&[]), "");
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(
            truncate_str("hello world this is long", 10),
            "hello worl..."
        );
        assert_eq!(truncate_str("line1\nline2", 20), "line1 line2");
    }

    // ── PR conversation comments ────────────────────────────────────

    fn make_conversation_step(id: &str, actor: &str, body: &str) -> Step {
        let mut extra = std::collections::HashMap::new();
        extra.insert("body".to_string(), serde_json::json!(body));
        let mut step = Step::new(id, actor, "2026-01-29T15:00:00Z");
        step.change.insert(
            "review://conversation".to_string(),
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "review.conversation".into(),
                    extra,
                }),
            },
        );
        step
    }

    #[test]
    fn test_render_conversation_summary() {
        let step = make_conversation_step("s1", "human:carol", "Looks good overall!");
        let md = render_step(&step, &RenderOptions::default());

        assert!(md.contains("conversation"));
        assert!(md.contains("Looks good overall!"));
        // Should NOT show review:// prefix
        assert!(!md.contains("review://"));
    }

    #[test]
    fn test_render_conversation_full() {
        let step = make_conversation_step("s1", "human:carol", "Looks good overall!");
        let md = render_step(
            &step,
            &RenderOptions {
                detail: Detail::Full,
                ..Default::default()
            },
        );

        assert!(md.contains("> Looks good overall!"));
        assert!(!md.contains("review://"));
    }

    #[test]
    fn test_review_section_includes_conversations() {
        let s1 = make_step("s1", "human:alice", &[]);
        let s2 = make_conversation_step("s2", "human:carol", "Looks good overall!");
        let s3 = make_review_decision_step("s3", "human:dave", "APPROVED", "Ship it!");
        let s2 = s2.with_parent("s1");
        let s3 = s3.with_parent("s2");
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s3".into(),
                graph_ref: None,
            },
            steps: vec![s1, s2, s3],
            meta: None,
        };
        let md = render_path(&path, &RenderOptions::default());

        assert!(md.contains("## Review"));
        assert!(md.contains("### Discussion"));
        assert!(md.contains("carol"));
        assert!(md.contains("Looks good overall!"));
        assert!(md.contains("APPROVED"));
    }

    #[test]
    fn test_render_merged_pr() {
        let s1 = make_step("s1", "human:alice", &[]);
        let mut extra = std::collections::HashMap::new();
        let github = serde_json::json!({
            "number": 7,
            "author": "alice",
            "state": "closed",
            "draft": false,
            "merged": true,
            "additions": 42,
            "deletions": 10,
            "changed_files": 3
        });
        extra.insert("github".to_string(), github);
        let path = Path {
            path: PathIdentity {
                id: "pr-7".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![s1],
            meta: Some(PathMeta {
                title: Some("Fix the thing".into()),
                extra,
                ..Default::default()
            }),
        };
        let md = render_path(&path, &RenderOptions::default());

        assert!(md.contains("**PR #7**"));
        assert!(md.contains("by alice"));
        // merged overrides state=closed
        assert!(md.contains("merged"));
        assert!(!md.contains("closed"));
    }

    #[test]
    fn test_catch_all_uses_friendly_name() {
        // An artifact with an unknown structural type should still get a friendly name
        let mut step = Step::new("s1", "human:alex", "2026-01-29T10:00:00Z");
        step.change.insert(
            "review://some/path#L5".to_string(),
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "review.custom".into(),
                    extra: Default::default(),
                }),
            },
        );
        let md = render_step(&step, &RenderOptions::default());

        // Should use friendly name (some/path:5), not raw review:// URI
        assert!(md.contains("some/path:5"));
        assert!(!md.contains("review://"));
    }

    // ── Kind: agent-coding-session rendering ──────────────────────────

    fn conv_append(role: &str, extras: &[(&str, serde_json::Value)]) -> ArtifactChange {
        let mut extra: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();
        extra.insert("role".into(), serde_json::json!(role));
        for (k, v) in extras {
            extra.insert((*k).into(), v.clone());
        }
        ArtifactChange {
            raw: None,
            structural: Some(StructuralChange {
                change_type: "conversation.append".into(),
                extra,
            }),
        }
    }

    fn agent_coding_session_path() -> Path {
        let key = "claude-code://sess-1";

        let mut user = Step::new("u1", "human:user", "2026-01-01T00:00:00Z");
        user.change.insert(
            key.into(),
            conv_append("user", &[("text", serde_json::json!("add a greeting"))]),
        );

        let mut asst = Step::new("a1", "agent:gpt-5.5", "2026-01-01T00:00:01Z");
        asst.step.parents = vec!["u1".into()];
        asst.change.insert(
            key.into(),
            conv_append(
                "assistant",
                &[
                    ("text", serde_json::json!("done")),
                    ("thinking", serde_json::json!("I'll edit main.rs")),
                    ("stop_reason", serde_json::json!("tool_use")),
                    (
                        "token_usage",
                        serde_json::json!({"input_tokens": 100, "output_tokens": 20, "cache_read_tokens": 50}),
                    ),
                    (
                        "tool_uses",
                        serde_json::json!([{
                            "id": "c1", "name": "write_file",
                            "input": {"file_path": "main.rs"},
                            "category": "file_write",
                            "result": {"content": "ok", "is_error": false}
                        }]),
                    ),
                ],
            ),
        );
        asst.change.insert(
            "main.rs".into(),
            ArtifactChange {
                raw: Some("@@ -0,0 +1 @@\n+fn main() {}".into()),
                structural: Some(StructuralChange {
                    change_type: "file.write".into(),
                    extra: std::collections::HashMap::from([(
                        "operation".to_string(),
                        serde_json::json!("add"),
                    )]),
                }),
            },
        );

        let mut ev = Step::new("e1", "tool:claude-code", "2026-01-01T00:00:02Z");
        ev.step.parents = vec!["a1".into()];
        ev.change.insert(
            key.into(),
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "conversation.event".into(),
                    extra: std::collections::HashMap::from([(
                        "entry_type".to_string(),
                        serde_json::json!("attachment"),
                    )]),
                }),
            },
        );

        Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "e1".into(),
                graph_ref: None,
            },
            steps: vec![user, asst, ev],
            meta: Some(PathMeta {
                kind: Some(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION.into()),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn truncate_str_is_char_boundary_safe() {
        // Truncating at a byte index that lands inside a multibyte char (here
        // the em-dash, bytes 198..201 at max=200) must not panic.
        let s = format!("{}—tail", "a".repeat(198));
        let out = truncate_str(&s, 200);
        assert!(out.ends_with("..."));
        assert!(out.starts_with(&"a".repeat(198)));
    }

    #[test]
    fn agent_coding_session_renders_flat_transcript() {
        let path = agent_coding_session_path();
        let md = render_path(
            &path,
            &RenderOptions {
                detail: Detail::Full,
                front_matter: false,
            },
        );

        // Speaker-labeled turns with content.
        assert!(
            md.contains("**User:** add a greeting"),
            "user turn missing:\n{md}"
        );
        assert!(md.contains("**Assistant:** done"), "assistant turn missing");
        assert!(
            md.contains("**Reasoning:**") && md.contains("I'll edit main.rs"),
            "reasoning missing:\n{md}"
        );
        assert!(
            md.contains("**Tools:**") && md.contains("`write_file`") && md.contains("\u{2192} ok"),
            "tool call missing:\n{md}"
        );
        assert!(
            md.contains("tokens: 100 in, 20 out, 50 cached"),
            "token usage missing:\n{md}"
        );
        assert!(md.contains("stop: tool_use"), "stop reason missing");
        assert!(
            md.contains("wrote `main.rs`") && md.contains("(add)"),
            "file.write missing:\n{md}"
        );

        // The generic DAG scaffolding is dropped: no per-step UUID headers,
        // no timestamp/parents lines, no dead-end markers, no event noise.
        assert!(!md.contains("### a1"), "step header leaked:\n{md}");
        assert!(!md.contains("**Timestamp:**"), "timestamp leaked:\n{md}");
        assert!(!md.contains("[dead end]"), "dead-end marker leaked:\n{md}");
        assert!(!md.contains("_attachment_"), "event noise leaked:\n{md}");
        assert!(
            !md.contains("## Timeline"),
            "timeline heading leaked:\n{md}"
        );
    }

    #[test]
    fn agent_coding_session_summary_compacts_tool_calls() {
        let path = agent_coding_session_path();
        let md = render_path(&path, &RenderOptions::default()); // Summary
        assert!(md.contains("**User:** add a greeting"), "user:\n{md}");
        assert!(md.contains("**Assistant:** done"), "assistant:\n{md}");
        // Tools collapse into a per-name breakdown; no diffs or reasoning.
        assert!(
            md.contains("*tools: write_file (1)*"),
            "tool breakdown:\n{md}"
        );
        assert!(
            !md.contains("```diff"),
            "summary should not emit diffs:\n{md}"
        );
        assert!(
            !md.contains("**Reasoning:**"),
            "summary omits reasoning:\n{md}"
        );
    }

    #[test]
    fn agent_coding_session_summary_drops_empty_turns_and_breaks_down_tools() {
        // user → assistant (no text, 2× Read + 1× Bash) → assistant ("ok", 1× Read)
        let key = "claude-code://sess-1";
        let mut user = Step::new("u1", "human:user", "2026-01-01T00:00:00Z");
        user.change.insert(
            key.into(),
            conv_append("user", &[("text", serde_json::json!("go"))]),
        );

        let mut work = Step::new("a1", "agent:gpt-5.5", "2026-01-01T00:00:01Z");
        work.step.parents = vec!["u1".into()];
        work.change.insert(
            key.into(),
            conv_append(
                "assistant",
                &[(
                    "tool_uses",
                    serde_json::json!([
                        {"id": "1", "name": "Read", "input": {}, "category": "file_read"},
                        {"id": "2", "name": "Read", "input": {}, "category": "file_read"},
                        {"id": "3", "name": "Bash", "input": {}, "category": "shell"}
                    ]),
                )],
            ),
        );

        let mut reply = Step::new("a2", "agent:gpt-5.5", "2026-01-01T00:00:02Z");
        reply.step.parents = vec!["a1".into()];
        reply.change.insert(
            key.into(),
            conv_append(
                "assistant",
                &[
                    ("text", serde_json::json!("ok")),
                    ("tool_uses", serde_json::json!([{"id": "4", "name": "Read", "input": {}, "category": "file_read"}])),
                ],
            ),
        );

        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "a2".into(),
                graph_ref: None,
            },
            steps: vec![user, work, reply],
            meta: Some(PathMeta {
                kind: Some(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION.into()),
                ..Default::default()
            }),
        };

        let md = render_path(&path, &RenderOptions::default()); // Summary
        // Text-less work turn produces no speaker line; only the two real
        // messages get one.
        assert_eq!(
            md.matches("**Assistant:**").count(),
            1,
            "empty turn rendered:\n{md}"
        );
        assert!(md.contains("**User:** go"));
        assert!(md.contains("**Assistant:** ok"));
        // The work turn's tools collapse into a per-name breakdown before "ok".
        assert!(
            md.contains("*tools: Read (2), Bash (1)*"),
            "breakdown:\n{md}"
        );
    }

    #[test]
    fn agent_coding_session_omits_abandoned_turns() {
        // Add a dead-end assistant turn off the user turn that isn't on the
        // head's ancestry; the transcript notes it as omitted, not inline.
        let mut path = agent_coding_session_path();
        let mut dead = Step::new("d1", "agent:gpt-5.5", "2026-01-01T00:00:03Z");
        dead.step.parents = vec!["u1".into()];
        dead.change.insert(
            "claude-code://sess-1".into(),
            conv_append(
                "assistant",
                &[("text", serde_json::json!("abandoned attempt"))],
            ),
        );
        path.steps.push(dead);

        let md = render_path(
            &path,
            &RenderOptions {
                detail: Detail::Full,
                front_matter: false,
            },
        );
        assert!(
            !md.contains("abandoned attempt"),
            "dead-end content shown:\n{md}"
        );
        assert!(
            md.contains("1 abandoned turn omitted"),
            "omission note:\n{md}"
        );
    }

    #[test]
    fn without_kind_conversation_renders_generically() {
        // Same changes, but no meta.kind: the renderer must not apply the
        // agent-coding-session transcript treatment.
        let mut path = agent_coding_session_path();
        path.meta = None;
        let md = render_path(
            &path,
            &RenderOptions {
                detail: Detail::Full,
                front_matter: false,
            },
        );
        assert!(
            !md.contains("**Reasoning:**"),
            "kind treatment leaked:\n{md}"
        );
        assert!(
            md.contains("Structural: `conversation.append`"),
            "expected the generic structural dump:\n{md}"
        );
    }
}
