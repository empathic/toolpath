//! Generate Graphviz DOT visualizations from Toolpath documents.
//!
//! Renders [`Document`]s — Steps, Paths, and Graphs — as Graphviz DOT
//! digraphs. Steps become nodes, parent references become edges, and
//! actor types are color-coded (blue for humans, green for agents,
//! yellow for tools). Dead ends are highlighted with dashed red borders.
//!
//! # Example
//!
//! ```
//! use toolpath::v1::{Document, Path, PathIdentity, Step};
//! use toolpath_dot::{render, RenderOptions};
//!
//! let step = Step::new("step-001", "human:alex", "2026-01-29T10:00:00Z")
//!     .with_raw_change("src/main.rs", "@@ -1 +1 @@\n-old\n+new")
//!     .with_intent("Fix greeting");
//!
//! let path = Path {
//!     path: PathIdentity { id: "p1".into(), base: None, head: "step-001".into() },
//!     steps: vec![step],
//!     meta: None,
//! };
//!
//! let dot = render(&Document::Path(path), &RenderOptions::default());
//! assert!(dot.contains("digraph toolpath"));
//! ```
//!
//! Pipe the output through Graphviz to produce images:
//!
//! ```bash
//! path derive git --repo . --branch main | path render dot | dot -Tpng -o graph.png
//! ```

use std::collections::{HashMap, HashSet};

use toolpath::v1::{Document, Graph, Path, PathOrRef, Step, query};

/// Options controlling what information is rendered in the DOT output.
pub struct RenderOptions {
    /// Include filenames from each step's change map.
    pub show_files: bool,
    /// Include the time portion of each step's timestamp.
    pub show_timestamps: bool,
    /// Render dead-end steps with dashed red borders.
    pub highlight_dead_ends: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            show_files: false,
            show_timestamps: false,
            highlight_dead_ends: true,
        }
    }
}

/// Render any Toolpath [`Document`] variant to a Graphviz DOT string.
pub fn render(doc: &Document, options: &RenderOptions) -> String {
    match doc {
        Document::Graph(g) => render_graph(g, options),
        Document::Path(p) => render_path(p, options),
        Document::Step(s) => render_step(s, options),
    }
}

/// Render a single [`Step`] as a DOT digraph.
pub fn render_step(step: &Step, options: &RenderOptions) -> String {
    let mut dot = String::new();
    dot.push_str("digraph toolpath {\n");
    dot.push_str("  rankdir=TB;\n");
    dot.push_str("  node [shape=box, style=rounded, fontname=\"Helvetica\"];\n\n");

    let label = format_step_label_html(step, options);
    let color = actor_color(&step.step.actor);
    dot.push_str(&format!(
        "  \"{}\" [label={}, fillcolor=\"{}\", style=\"rounded,filled\"];\n",
        step.step.id, label, color
    ));

    for parent in &step.step.parents {
        dot.push_str(&format!("  \"{}\" -> \"{}\";\n", parent, step.step.id));
    }

    dot.push_str("}\n");
    dot
}

/// Render a [`Path`] as a DOT digraph.
pub fn render_path(path: &Path, options: &RenderOptions) -> String {
    let mut dot = String::new();
    dot.push_str("digraph toolpath {\n");
    dot.push_str("  rankdir=TB;\n");
    dot.push_str("  node [shape=box, style=rounded, fontname=\"Helvetica\"];\n");
    dot.push_str("  edge [color=\"#666666\"];\n");
    dot.push_str("  splines=ortho;\n\n");

    // Add title
    if let Some(meta) = &path.meta
        && let Some(title) = &meta.title
    {
        dot.push_str("  labelloc=\"t\";\n");
        dot.push_str(&format!("  label=\"{}\";\n", escape_dot(title)));
        dot.push_str("  fontsize=16;\n");
        dot.push_str("  fontname=\"Helvetica-Bold\";\n\n");
    }

    // Find ancestors of head (active path)
    let active_steps = query::ancestors(&path.steps, &path.path.head);

    // Add base node
    if let Some(base) = &path.path.base {
        let short_commit = safe_prefix(base.ref_str.as_deref().unwrap_or(""), 8);
        let base_label = format!(
            "<<b>BASE</b><br/><font point-size=\"10\">{}</font><br/><font point-size=\"9\" color=\"#666666\">{}</font>>",
            escape_html(&base.uri),
            escape_html(&short_commit)
        );
        dot.push_str(&format!(
            "  \"__base__\" [label={}, shape=ellipse, style=filled, fillcolor=\"#e0e0e0\"];\n",
            base_label
        ));
    }

    // Add step nodes
    for step in &path.steps {
        let label = format_step_label_html(step, options);
        let color = actor_color(&step.step.actor);
        let is_head = step.step.id == path.path.head;
        let is_active = active_steps.contains(&step.step.id);
        let is_dead_end = !is_active && options.highlight_dead_ends;

        let mut style = "rounded,filled".to_string();
        let mut penwidth = "1";
        let mut fillcolor = color.to_string();

        if is_head {
            style = "rounded,filled,bold".to_string();
            penwidth = "3";
        } else if is_dead_end {
            fillcolor = "#ffcccc".to_string(); // Light red for dead ends
            style = "rounded,filled,dashed".to_string();
        }

        dot.push_str(&format!(
            "  \"{}\" [label={}, fillcolor=\"{}\", style=\"{}\", penwidth={}];\n",
            step.step.id, label, fillcolor, style, penwidth
        ));
    }

    dot.push('\n');

    // Add edges
    for step in &path.steps {
        if step.step.parents.is_empty() {
            // Root step - connect to base
            if path.path.base.is_some() {
                dot.push_str(&format!("  \"__base__\" -> \"{}\";\n", step.step.id));
            }
        } else {
            for parent in &step.step.parents {
                let is_active_edge =
                    active_steps.contains(&step.step.id) && active_steps.contains(parent);
                let edge_style = if is_active_edge {
                    "color=\"#333333\", penwidth=2"
                } else {
                    "color=\"#cccccc\", style=dashed"
                };
                dot.push_str(&format!(
                    "  \"{}\" -> \"{}\" [{}];\n",
                    parent, step.step.id, edge_style
                ));
            }
        }
    }

    // Add legend
    dot.push_str("\n  // Legend\n");
    dot.push_str("  subgraph cluster_legend {\n");
    dot.push_str("    label=\"Legend\";\n");
    dot.push_str("    fontname=\"Helvetica-Bold\";\n");
    dot.push_str("    style=filled;\n");
    dot.push_str("    fillcolor=\"#f8f8f8\";\n");
    dot.push_str("    node [shape=box, style=\"rounded,filled\", width=0.9, fontname=\"Helvetica\", fontsize=10];\n");
    dot.push_str(&format!(
        "    leg_human [label=\"human\", fillcolor=\"{}\"];\n",
        actor_color("human:x")
    ));
    dot.push_str(&format!(
        "    leg_agent [label=\"agent\", fillcolor=\"{}\"];\n",
        actor_color("agent:x")
    ));
    dot.push_str(&format!(
        "    leg_tool [label=\"tool\", fillcolor=\"{}\"];\n",
        actor_color("tool:x")
    ));
    if options.highlight_dead_ends {
        dot.push_str(
            "    leg_dead [label=\"dead end\", fillcolor=\"#ffcccc\", style=\"rounded,filled,dashed\"];\n",
        );
    }
    dot.push_str("    leg_human -> leg_agent -> leg_tool [style=invis];\n");
    if options.highlight_dead_ends {
        dot.push_str("    leg_tool -> leg_dead [style=invis];\n");
    }
    dot.push_str("  }\n");

    dot.push_str("}\n");
    dot
}

/// Render a [`Graph`] as a DOT digraph.
pub fn render_graph(graph: &Graph, options: &RenderOptions) -> String {
    let mut dot = String::new();
    dot.push_str("digraph toolpath {\n");
    dot.push_str("  rankdir=TB;\n");
    dot.push_str("  compound=true;\n");
    dot.push_str("  newrank=true;\n");
    dot.push_str("  node [shape=box, style=rounded, fontname=\"Helvetica\"];\n");
    dot.push_str("  edge [color=\"#333333\"];\n");
    dot.push_str("  splines=ortho;\n\n");

    // Add title
    if let Some(meta) = &graph.meta
        && let Some(title) = &meta.title
    {
        dot.push_str("  labelloc=\"t\";\n");
        dot.push_str(&format!("  label=\"{}\";\n", escape_dot(title)));
        dot.push_str("  fontsize=18;\n");
        dot.push_str("  fontname=\"Helvetica-Bold\";\n\n");
    }

    // Build a map of commit hashes to step IDs across all paths
    let mut commit_to_step: HashMap<String, String> = HashMap::new();

    for path_or_ref in &graph.paths {
        if let PathOrRef::Path(path) = path_or_ref {
            for step in &path.steps {
                if let Some(meta) = &step.meta
                    && let Some(source) = &meta.source
                {
                    commit_to_step.insert(source.revision.clone(), step.step.id.clone());
                    if source.revision.len() >= 8 {
                        commit_to_step
                            .insert(safe_prefix(&source.revision, 8), step.step.id.clone());
                    }
                }
            }
        }
    }

    // No BASE nodes - commits without parents in the graph are simply root nodes

    // Collect all heads and all step IDs
    let mut heads: HashSet<String> = HashSet::new();
    let mut all_step_ids: HashSet<String> = HashSet::new();
    for path_or_ref in &graph.paths {
        if let PathOrRef::Path(path) = path_or_ref {
            heads.insert(path.path.head.clone());
            for step in &path.steps {
                all_step_ids.insert(step.step.id.clone());
            }
        }
    }

    // Track root steps for base connections
    let mut root_steps: Vec<(String, Option<String>)> = Vec::new();

    // Assign colors to paths
    let path_colors = [
        "#e3f2fd", "#e8f5e9", "#fff3e0", "#f3e5f5", "#e0f7fa", "#fce4ec",
    ];

    // Add step nodes inside clusters
    for (i, path_or_ref) in graph.paths.iter().enumerate() {
        if let PathOrRef::Path(path) = path_or_ref {
            let path_name = path
                .meta
                .as_ref()
                .and_then(|m| m.title.as_ref())
                .map(|t| t.as_str())
                .unwrap_or(&path.path.id);

            let cluster_color = path_colors[i % path_colors.len()];

            dot.push_str(&format!("  subgraph cluster_{} {{\n", i));
            dot.push_str(&format!("    label=\"{}\";\n", escape_dot(path_name)));
            dot.push_str("    fontname=\"Helvetica-Bold\";\n");
            dot.push_str("    style=filled;\n");
            dot.push_str(&format!("    fillcolor=\"{}\";\n", cluster_color));
            dot.push_str("    margin=12;\n\n");

            let active_steps = query::ancestors(&path.steps, &path.path.head);

            for step in &path.steps {
                let label = format_step_label_html(step, options);
                let color = actor_color(&step.step.actor);
                let is_head = heads.contains(&step.step.id);
                let is_active = active_steps.contains(&step.step.id);
                let is_dead_end = !is_active && options.highlight_dead_ends;

                let mut style = "rounded,filled".to_string();
                let mut penwidth = "1";
                let mut fillcolor = color.to_string();

                if is_head {
                    style = "rounded,filled,bold".to_string();
                    penwidth = "3";
                } else if is_dead_end {
                    fillcolor = "#ffcccc".to_string();
                    style = "rounded,filled,dashed".to_string();
                }

                dot.push_str(&format!(
                    "    \"{}\" [label={}, fillcolor=\"{}\", style=\"{}\", penwidth={}];\n",
                    step.step.id, label, fillcolor, style, penwidth
                ));

                // Track root steps
                let is_root = step.step.parents.is_empty()
                    || step.step.parents.iter().all(|p| !all_step_ids.contains(p));
                if is_root {
                    root_steps.push((
                        step.step.id.clone(),
                        path.path.base.as_ref().and_then(|b| b.ref_str.clone()),
                    ));
                }
            }

            dot.push_str("  }\n\n");
        }
    }

    // Add all edges (outside clusters for cross-cluster edges)
    for path_or_ref in &graph.paths {
        if let PathOrRef::Path(path) = path_or_ref {
            let active_steps = query::ancestors(&path.steps, &path.path.head);

            for step in &path.steps {
                for parent in &step.step.parents {
                    if all_step_ids.contains(parent) {
                        let is_active_edge =
                            active_steps.contains(&step.step.id) && active_steps.contains(parent);
                        let edge_style = if is_active_edge {
                            "color=\"#333333\", penwidth=2"
                        } else {
                            "color=\"#cccccc\", style=dashed"
                        };
                        dot.push_str(&format!(
                            "  \"{}\" -> \"{}\" [{}];\n",
                            parent, step.step.id, edge_style
                        ));
                    }
                }
            }
        }
    }

    // Add edges from base commits to root steps (cross-cluster edges)
    dot.push_str("\n  // Cross-cluster edges (where branches diverge)\n");
    for (step_id, base_commit) in &root_steps {
        if let Some(commit) = base_commit {
            let short_commit = safe_prefix(commit, 8);
            // Only create edge if the base commit exists as a step in another path
            if let Some(parent_step_id) = commit_to_step
                .get(commit)
                .or_else(|| commit_to_step.get(&short_commit))
            {
                dot.push_str(&format!(
                    "  \"{}\" -> \"{}\" [color=\"#333333\", penwidth=2];\n",
                    parent_step_id, step_id
                ));
            }
            // Otherwise, this is just a root node with no parent - that's fine
        }
    }

    // Add external refs
    for (i, path_or_ref) in graph.paths.iter().enumerate() {
        if let PathOrRef::Ref(path_ref) = path_or_ref {
            let ref_id = format!("ref_{}", i);
            let ref_label = format!(
                "<<b>$ref</b><br/><font point-size=\"9\">{}</font>>",
                escape_html(&path_ref.ref_url)
            );
            dot.push_str(&format!(
                "  \"{}\" [label={}, shape=note, style=filled, fillcolor=\"#ffffcc\"];\n",
                ref_id, ref_label
            ));
        }
    }

    dot.push_str("}\n");
    dot
}

fn format_step_label_html(step: &Step, options: &RenderOptions) -> String {
    let mut rows = vec![];

    // Commit hash (from VCS source) or step ID
    let header = if let Some(meta) = &step.meta {
        if let Some(source) = &meta.source {
            // Show short commit hash
            let short_rev = safe_prefix(&source.revision, 8);
            format!("<b>{}</b>", escape_html(&short_rev))
        } else {
            format!("<b>{}</b>", escape_html(&step.step.id))
        }
    } else {
        format!("<b>{}</b>", escape_html(&step.step.id))
    };
    rows.push(header);

    // Actor (shortened)
    let actor_short = step
        .step
        .actor
        .split(':')
        .next_back()
        .unwrap_or(&step.step.actor);
    rows.push(format!(
        "<font point-size=\"10\">{}</font>",
        escape_html(actor_short)
    ));

    // Intent if available
    if let Some(meta) = &step.meta
        && let Some(intent) = &meta.intent
    {
        let short_intent = if intent.chars().count() > 40 {
            let truncated: String = intent.chars().take(37).collect();
            format!("{}\u{2026}", truncated)
        } else {
            intent.clone()
        };
        rows.push(format!(
            "<font point-size=\"9\"><i>{}</i></font>",
            escape_html(&short_intent)
        ));
    }

    // Timestamp if requested
    if options.show_timestamps {
        let ts = &step.step.timestamp;
        // Show just time portion
        if let Some(time_part) = ts.split('T').nth(1) {
            rows.push(format!(
                "<font point-size=\"8\" color=\"gray\">{}</font>",
                escape_html(time_part.trim_end_matches('Z'))
            ));
        }
    }

    // Files if requested
    if options.show_files {
        let files: Vec<_> = step.change.keys().collect();
        if !files.is_empty() {
            let files_str = if files.len() <= 2 {
                files
                    .iter()
                    .map(|f| f.split('/').next_back().unwrap_or(f))
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                format!("{} files", files.len())
            };
            rows.push(format!(
                "<font point-size=\"8\" color=\"#666666\">{}</font>",
                escape_html(&files_str)
            ));
        }
    }

    format!("<{}>", rows.join("<br/>"))
}

/// Return a fill color for a given actor string.
pub fn actor_color(actor: &str) -> &'static str {
    if actor.starts_with("human:") {
        "#cce5ff" // Light blue
    } else if actor.starts_with("agent:") {
        "#d4edda" // Light green
    } else if actor.starts_with("tool:") {
        "#fff3cd" // Light yellow
    } else if actor.starts_with("ci:") {
        "#e2d5f1" // Light purple
    } else {
        "#f8f9fa" // Light gray
    }
}

/// Return the first `n` characters of a string, safe for any UTF-8 content.
fn safe_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Escape a string for use in DOT label attributes (double-quoted context).
pub fn escape_dot(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Escape a string for use inside HTML-like DOT labels.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath::v1::{
        Base, Graph, GraphIdentity, GraphMeta, Path, PathIdentity, PathMeta, PathOrRef, PathRef,
        Step,
    };

    fn make_step(id: &str, actor: &str, parents: &[&str]) -> Step {
        let mut step = Step::new(id, actor, "2026-01-01T12:00:00Z")
            .with_raw_change("src/main.rs", "@@ -1 +1 @@\n-old\n+new");
        for p in parents {
            step = step.with_parent(*p);
        }
        step
    }

    fn make_step_with_intent(id: &str, actor: &str, parents: &[&str], intent: &str) -> Step {
        make_step(id, actor, parents).with_intent(intent)
    }

    fn make_step_with_source(id: &str, actor: &str, parents: &[&str], revision: &str) -> Step {
        make_step(id, actor, parents).with_vcs_source("git", revision)
    }

    // ── escape_dot ─────────────────────────────────────────────────────

    #[test]
    fn test_escape_dot_quotes() {
        assert_eq!(escape_dot(r#"say "hello""#), r#"say \"hello\""#);
    }

    #[test]
    fn test_escape_dot_backslash() {
        assert_eq!(escape_dot(r"path\to\file"), r"path\\to\\file");
    }

    #[test]
    fn test_escape_dot_newline() {
        assert_eq!(escape_dot("line1\nline2"), r"line1\nline2");
    }

    #[test]
    fn test_escape_dot_passthrough() {
        assert_eq!(escape_dot("simple text"), "simple text");
    }

    // ── escape_html ────────────────────────────────────────────────────

    #[test]
    fn test_escape_html_ampersand() {
        assert_eq!(escape_html("a & b"), "a &amp; b");
    }

    #[test]
    fn test_escape_html_angle_brackets() {
        assert_eq!(escape_html("<tag>"), "&lt;tag&gt;");
    }

    #[test]
    fn test_escape_html_quotes() {
        assert_eq!(escape_html(r#"a "b""#), "a &quot;b&quot;");
    }

    #[test]
    fn test_escape_html_combined() {
        assert_eq!(
            escape_html(r#"<a href="url">&</a>"#),
            "&lt;a href=&quot;url&quot;&gt;&amp;&lt;/a&gt;"
        );
    }

    // ── actor_color ────────────────────────────────────────────────────

    #[test]
    fn test_actor_color_human() {
        assert_eq!(actor_color("human:alex"), "#cce5ff");
    }

    #[test]
    fn test_actor_color_agent() {
        assert_eq!(actor_color("agent:claude"), "#d4edda");
    }

    #[test]
    fn test_actor_color_tool() {
        assert_eq!(actor_color("tool:rustfmt"), "#fff3cd");
    }

    #[test]
    fn test_actor_color_ci() {
        assert_eq!(actor_color("ci:github-actions"), "#e2d5f1");
    }

    #[test]
    fn test_actor_color_unknown() {
        assert_eq!(actor_color("other:thing"), "#f8f9fa");
    }

    // ── safe_prefix ────────────────────────────────────────────────────

    #[test]
    fn test_safe_prefix_normal() {
        assert_eq!(safe_prefix("abcdef1234", 8), "abcdef12");
    }

    #[test]
    fn test_safe_prefix_shorter_than_n() {
        assert_eq!(safe_prefix("abc", 8), "abc");
    }

    #[test]
    fn test_safe_prefix_multibyte() {
        assert_eq!(safe_prefix("日本語", 2), "日本");
    }

    // ── render_step ────────────────────────────────────────────────────

    #[test]
    fn test_render_step_basic() {
        let step = make_step("s1", "human:alex", &[]);
        let opts = RenderOptions::default();
        let dot = render_step(&step, &opts);

        assert!(dot.starts_with("digraph toolpath {"));
        assert!(dot.contains("\"s1\""));
        assert!(dot.contains("#cce5ff")); // human color
        assert!(dot.ends_with("}\n"));
    }

    #[test]
    fn test_render_step_with_parents() {
        let step = make_step("s2", "agent:claude", &["s1"]);
        let opts = RenderOptions::default();
        let dot = render_step(&step, &opts);

        assert!(dot.contains("\"s1\" -> \"s2\""));
    }

    #[test]
    fn test_render_step_with_intent() {
        let step = make_step_with_intent("s1", "human:alex", &[], "Fix the bug");
        let opts = RenderOptions::default();
        let dot = render_step(&step, &opts);

        assert!(dot.contains("Fix the bug"));
    }

    #[test]
    fn test_render_step_truncates_long_intent() {
        let long_intent = "A".repeat(50);
        let step = make_step_with_intent("s1", "human:alex", &[], &long_intent);
        let opts = RenderOptions::default();
        let dot = render_step(&step, &opts);

        // Intent > 40 chars should be truncated with ellipsis
        assert!(dot.contains("\u{2026}")); // unicode ellipsis
    }

    #[test]
    fn test_render_step_with_vcs_source() {
        let step = make_step_with_source("s1", "human:alex", &[], "abcdef1234567890");
        let opts = RenderOptions::default();
        let dot = render_step(&step, &opts);

        // Should show short commit hash
        assert!(dot.contains("abcdef12"));
    }

    // ── render_path ────────────────────────────────────────────────────

    #[test]
    fn test_render_path_basic() {
        let s1 = make_step("s1", "human:alex", &[]);
        let s2 = make_step("s2", "agent:claude", &["s1"]);
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: Some(Base::vcs("github:org/repo", "abc123")),
                head: "s2".into(),
            },
            steps: vec![s1, s2],
            meta: Some(PathMeta {
                title: Some("Test Path".into()),
                ..Default::default()
            }),
        };
        let opts = RenderOptions::default();
        let dot = render_path(&path, &opts);

        assert!(dot.contains("digraph toolpath"));
        assert!(dot.contains("Test Path"));
        assert!(dot.contains("__base__"));
        assert!(dot.contains("\"s1\""));
        assert!(dot.contains("\"s2\""));
        // s2 is head, should be bold
        assert!(dot.contains("penwidth=3"));
        // Legend
        assert!(dot.contains("cluster_legend"));
    }

    #[test]
    fn test_render_path_dead_end_highlighting() {
        let s1 = make_step("s1", "human:alex", &[]);
        let s2 = make_step("s2", "agent:claude", &["s1"]);
        let s2a = make_step("s2a", "agent:claude", &["s1"]); // dead end
        let s3 = make_step("s3", "human:alex", &["s2"]);
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s3".into(),
            },
            steps: vec![s1, s2, s2a, s3],
            meta: None,
        };
        let opts = RenderOptions {
            highlight_dead_ends: true,
            ..Default::default()
        };
        let dot = render_path(&path, &opts);

        assert!(dot.contains("#ffcccc")); // dead end color
        assert!(dot.contains("dashed"));
    }

    #[test]
    fn test_render_path_with_timestamps() {
        let s1 = make_step("s1", "human:alex", &[]);
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
            },
            steps: vec![s1],
            meta: None,
        };
        let opts = RenderOptions {
            show_timestamps: true,
            ..Default::default()
        };
        let dot = render_path(&path, &opts);

        assert!(dot.contains("12:00:00")); // time portion
    }

    #[test]
    fn test_render_path_with_files() {
        let s1 = make_step("s1", "human:alex", &[]);
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
            },
            steps: vec![s1],
            meta: None,
        };
        let opts = RenderOptions {
            show_files: true,
            ..Default::default()
        };
        let dot = render_path(&path, &opts);

        assert!(dot.contains("main.rs"));
    }

    // ── render_graph ───────────────────────────────────────────────────

    #[test]
    fn test_render_graph_basic() {
        let s1 = make_step("s1", "human:alex", &[]);
        let s2 = make_step("s2", "agent:claude", &["s1"]);
        let path1 = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: Some(Base::vcs("github:org/repo", "abc123")),
                head: "s2".into(),
            },
            steps: vec![s1, s2],
            meta: Some(PathMeta {
                title: Some("Branch: main".into()),
                ..Default::default()
            }),
        };

        let s3 = make_step("s3", "human:bob", &[]);
        let path2 = Path {
            path: PathIdentity {
                id: "p2".into(),
                base: Some(Base::vcs("github:org/repo", "abc123")),
                head: "s3".into(),
            },
            steps: vec![s3],
            meta: Some(PathMeta {
                title: Some("Branch: feature".into()),
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
                title: Some("Test Graph".into()),
                ..Default::default()
            }),
        };

        let opts = RenderOptions::default();
        let dot = render_graph(&graph, &opts);

        assert!(dot.contains("digraph toolpath"));
        assert!(dot.contains("compound=true"));
        assert!(dot.contains("Test Graph"));
        assert!(dot.contains("cluster_0"));
        assert!(dot.contains("cluster_1"));
        assert!(dot.contains("Branch: main"));
        assert!(dot.contains("Branch: feature"));
    }

    #[test]
    fn test_render_graph_with_refs() {
        let graph = Graph {
            graph: GraphIdentity { id: "g1".into() },
            paths: vec![PathOrRef::Ref(PathRef {
                ref_url: "https://example.com/path.json".to_string(),
            })],
            meta: None,
        };

        let opts = RenderOptions::default();
        let dot = render_graph(&graph, &opts);

        assert!(dot.contains("$ref"));
        assert!(dot.contains("example.com/path.json"));
        assert!(dot.contains("#ffffcc")); // ref note color
    }

    // ── render (dispatch) ──────────────────────────────────────────────

    #[test]
    fn test_render_dispatches_step() {
        let step = make_step("s1", "human:alex", &[]);
        let doc = Document::Step(step);
        let opts = RenderOptions::default();
        let dot = render(&doc, &opts);
        assert!(dot.contains("\"s1\""));
    }

    #[test]
    fn test_render_dispatches_path() {
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
            },
            steps: vec![make_step("s1", "human:alex", &[])],
            meta: None,
        };
        let doc = Document::Path(path);
        let opts = RenderOptions::default();
        let dot = render(&doc, &opts);
        assert!(dot.contains("cluster_legend"));
    }

    #[test]
    fn test_render_dispatches_graph() {
        let graph = Graph {
            graph: GraphIdentity { id: "g1".into() },
            paths: vec![],
            meta: None,
        };
        let doc = Document::Graph(graph);
        let opts = RenderOptions::default();
        let dot = render(&doc, &opts);
        assert!(dot.contains("compound=true"));
    }
}
