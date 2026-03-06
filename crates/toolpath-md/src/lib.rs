#![doc = include_str!("../README.md")]

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use toolpath::v1::{
    ArtifactChange, Document, Graph, Path, PathOrRef, Step, query,
};

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

/// Render any Toolpath [`Document`] variant to a Markdown string.
///
/// # Examples
///
/// ```
/// use toolpath::v1::{Document, Step};
/// use toolpath_md::{render, RenderOptions};
///
/// let step = Step::new("s1", "human:alex", "2026-01-29T10:00:00Z")
///     .with_raw_change("src/main.rs", "@@ -1 +1 @@\n-old\n+new")
///     .with_intent("Fix greeting");
/// let doc = Document::Step(step);
/// let md = render(&doc, &RenderOptions::default());
/// assert!(md.contains("# s1"));
/// assert!(md.contains("human:alex"));
/// ```
pub fn render(doc: &Document, options: &RenderOptions) -> String {
    match doc {
        Document::Graph(g) => render_graph(g, options),
        Document::Path(p) => render_path(p, options),
        Document::Step(s) => render_step(s, options),
    }
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
    match options.detail {
        Detail::Summary => {
            let annotation = change_annotation(change);
            writeln!(out, "- `{artifact}`{annotation}").unwrap();
        }
        Detail::Full => {
            writeln!(out, "**`{artifact}`**").unwrap();
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
                writeln!(out, "Structural: `{}`{extra_str}", structural.change_type).unwrap();
            }
            writeln!(out).unwrap();
        }
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
        (true, _) => " \u{274c} dead end",
        (_, true) => " \u{2705} head",
        _ => "",
    };

    writeln!(out, "### {} \u{2014} {}{}", step.step.id, actor_short, markers).unwrap();
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
    if let Some(base) = &path.path.base {
        write!(out, "**Base:** `{}`", base.uri).unwrap();
        if let Some(ref_str) = &base.ref_str {
            write!(out, " @ `{ref_str}`").unwrap();
        }
        writeln!(out).unwrap();
    }

    writeln!(out, "**Head:** `{}`", path.path.head).unwrap();

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

fn write_actors_section(
    out: &mut String,
    actors: &HashMap<String, toolpath::v1::ActorDefinition>,
) {
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
        assert!(md.contains("\u{2705} head")); // head marker
    }

    #[test]
    fn test_render_path_with_dead_ends() {
        let s1 = make_step("s1", "human:alex", &[]);
        let s2 = make_step_with_intent("s2", "agent:claude", &["s1"], "Good approach");
        let s2a =
            make_step_with_intent("s2a", "agent:claude", &["s1"], "Bad approach (abandoned)");
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
        let opts = RenderOptions::default();
        let md = render_path(&path, &opts);

        assert!(md.contains("\u{274c} dead end"));
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
    fn test_render_dispatches_step() {
        let step = make_step("s1", "human:alex", &[]);
        let doc = Document::Step(step);
        let md = render(&doc, &RenderOptions::default());
        assert!(md.contains("# s1"));
    }

    #[test]
    fn test_render_dispatches_path() {
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
        let doc = Document::Path(path);
        let md = render(&doc, &RenderOptions::default());
        assert!(md.contains("## Timeline"));
    }

    #[test]
    fn test_render_dispatches_graph() {
        let graph = Graph {
            graph: GraphIdentity { id: "g1".into() },
            paths: vec![],
            meta: Some(GraphMeta {
                title: Some("My Graph".into()),
                ..Default::default()
            }),
        };
        let doc = Document::Graph(graph);
        let md = render(&doc, &RenderOptions::default());
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
}
