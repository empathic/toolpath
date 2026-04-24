//! Shared derivation: [`ConversationView`] → [`toolpath::v1::Path`].
//!
//! Provider-agnostic mapping used by the Pi, Claude, and future conversation
//! providers. Takes a [`ConversationView`] and emits a [`Path`] document with
//! one step per turn and a `conversation.append` structural change carrying
//! the turn's text, thinking, tool uses, and token usage.

use std::collections::HashMap;

use toolpath::v1::{
    ActorDefinition, ArtifactChange, Base, Path, PathIdentity, PathMeta, Step, StepIdentity,
    StructuralChange,
};

use crate::{ConversationView, Role, ToolCategory, ToolInvocation, Turn};

/// Configuration for [`derive_path`].
#[derive(Debug, Clone)]
pub struct DeriveConfig {
    /// Override `path.base.uri`. If `None`, fall back to the first turn's
    /// `environment.working_dir`.
    pub base_uri: Option<String>,
    /// Override `path.id`. If `None`, derive as `path-{provider}-{8chars}`.
    pub path_id: Option<String>,
    /// Override `meta.title`. If `None`, default to `"{provider} session: {8chars}"`.
    pub title: Option<String>,
    /// Include `Turn.thinking` in the structural change extras.
    pub include_thinking: bool,
    /// Include `Turn.tool_uses` in the structural change extras.
    pub include_tool_uses: bool,
}

impl Default for DeriveConfig {
    fn default() -> Self {
        Self {
            base_uri: None,
            path_id: None,
            title: None,
            include_thinking: true,
            include_tool_uses: true,
        }
    }
}

/// Derive a [`Path`] from a [`ConversationView`].
pub fn derive_path(view: &ConversationView, config: &DeriveConfig) -> Path {
    let provider = view.provider_id.as_deref().unwrap_or("unknown");
    let id_prefix: String = view.id.chars().take(8).collect();

    let path_id = config
        .path_id
        .clone()
        .unwrap_or_else(|| format!("path-{}-{}", provider, id_prefix));

    // Base URI: config override wins; otherwise first turn's working_dir
    let base = config
        .base_uri
        .clone()
        .map(|uri| Base { uri, ref_str: None })
        .or_else(|| {
            view.turns
                .iter()
                .find_map(|t| t.environment.as_ref()?.working_dir.clone())
                .map(|wd| {
                    let uri = if wd.starts_with('/') {
                        format!("file://{}", wd)
                    } else {
                        wd
                    };
                    Base { uri, ref_str: None }
                })
        });

    let conv_artifact_key = format!("{}://{}", provider, view.id);

    let mut steps: Vec<Step> = Vec::with_capacity(view.turns.len());
    let mut turn_to_step: HashMap<String, String> = HashMap::new();
    let mut actors: HashMap<String, ActorDefinition> = HashMap::new();

    for (idx, turn) in view.turns.iter().enumerate() {
        let step_id = format!("step-{:04}", idx + 1);
        turn_to_step.insert(turn.id.clone(), step_id.clone());

        let actor = actor_for_turn(turn, provider);
        record_actor(&mut actors, &actor, turn, provider, view);

        let mut step = Step {
            step: StepIdentity {
                id: step_id,
                parents: Vec::new(),
                actor,
                timestamp: turn.timestamp.clone(),
            },
            change: HashMap::new(),
            meta: None,
        };

        // Parent mapping
        if let Some(parent_id) = &turn.parent_id
            && let Some(parent_step_id) = turn_to_step.get(parent_id)
        {
            step.step.parents.push(parent_step_id.clone());
        }

        // Build conversation.append structural change extras
        let mut extra: HashMap<String, serde_json::Value> = HashMap::new();
        extra.insert(
            "role".to_string(),
            serde_json::Value::String(turn.role.to_string()),
        );
        extra.insert(
            "text".to_string(),
            serde_json::Value::String(turn.text.clone()),
        );

        if config.include_thinking
            && let Some(thinking) = &turn.thinking
        {
            extra.insert(
                "thinking".to_string(),
                serde_json::Value::String(thinking.clone()),
            );
        }

        if config.include_tool_uses && !turn.tool_uses.is_empty() {
            let arr: Vec<serde_json::Value> = turn
                .tool_uses
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "id": t.id,
                        "name": t.name,
                        "input": t.input,
                        "category": t.category,
                    })
                })
                .collect();
            extra.insert("tool_uses".to_string(), serde_json::Value::Array(arr));
        }

        if let Some(usage) = &turn.token_usage
            && let Ok(v) = serde_json::to_value(usage)
        {
            extra.insert("token_usage".to_string(), v);
        }

        if !turn.delegations.is_empty()
            && let Ok(v) = serde_json::to_value(&turn.delegations)
        {
            extra.insert("delegations".to_string(), v);
        }

        step.change.insert(
            conv_artifact_key.clone(),
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "conversation.append".to_string(),
                    extra,
                }),
            },
        );

        // File-write tool invocations → artifact changes. Each gets a unified
        // diff in `raw` (so it renders like a git diff) plus the structured
        // before/after strings in `structural.extra` for tools that want to
        // re-apply or inspect the op programmatically.
        for tool in &turn.tool_uses {
            if tool.category != Some(ToolCategory::FileWrite) {
                continue;
            }
            let Some(path) = extract_file_path(tool) else {
                continue;
            };
            // Shared derivation doesn't have access to a local checkout,
            // so it can't resolve pre-write file state. Providers that do
            // (e.g. `toolpath-claude`) build their own steps and pass a
            // resolved `before_state` directly to `file_write_diff`.
            let (raw, mut t_extra) = file_write_change(tool, &path, None);
            t_extra.insert(
                "tool".to_string(),
                serde_json::Value::String(tool.name.clone()),
            );
            t_extra.insert(
                "tool_id".to_string(),
                serde_json::Value::String(tool.id.clone()),
            );
            step.change.insert(
                path,
                ArtifactChange {
                    raw,
                    structural: Some(StructuralChange {
                        change_type: "file.write".to_string(),
                        extra: t_extra,
                    }),
                },
            );
        }

        steps.push(step);
    }

    let head = steps.last().map(|s| s.step.id.clone()).unwrap_or_default();

    // Meta
    let title = config
        .title
        .clone()
        .unwrap_or_else(|| format!("{} session: {}", provider, id_prefix));

    let mut meta = PathMeta {
        title: Some(title),
        source: view.provider_id.clone(),
        ..Default::default()
    };

    if !actors.is_empty() {
        meta.actors = Some(actors);
    }

    if !view.files_changed.is_empty()
        && let Ok(v) = serde_json::to_value(&view.files_changed)
    {
        meta.extra.insert("files_changed".to_string(), v);
    }

    Path {
        path: PathIdentity {
            id: path_id,
            base,
            head,
        },
        steps,
        meta: Some(meta),
    }
}

fn actor_for_turn(turn: &Turn, provider: &str) -> String {
    match &turn.role {
        Role::User => "human:user".to_string(),
        Role::Assistant => {
            let model = turn.model.as_deref().unwrap_or("unknown");
            format!("agent:{}", model)
        }
        Role::System => format!("system:{}", provider),
        Role::Other(s) => format!("{}:unknown", s),
    }
}

fn record_actor(
    actors: &mut HashMap<String, ActorDefinition>,
    actor: &str,
    turn: &Turn,
    provider: &str,
    _view: &ConversationView,
) {
    if actors.contains_key(actor) {
        return;
    }
    let def = if let Some(rest) = actor.strip_prefix("agent:") {
        ActorDefinition {
            name: Some(rest.to_string()),
            provider: Some(provider.to_string()),
            model: turn.model.clone(),
            identities: vec![],
            keys: vec![],
        }
    } else if let Some(rest) = actor.strip_prefix("human:") {
        ActorDefinition {
            name: Some(rest.to_string()),
            ..Default::default()
        }
    } else {
        // system:*, other:*
        let name = actor.split_once(':').map(|x| x.1).unwrap_or("").to_string();
        ActorDefinition {
            name: Some(name),
            ..Default::default()
        }
    };
    actors.insert(actor.to_string(), def);
}

fn extract_file_path(tool: &ToolInvocation) -> Option<String> {
    for field in &["file_path", "path", "filename", "file"] {
        if let Some(v) = tool.input.get(*field)
            && let Some(s) = v.as_str()
        {
            return Some(s.to_string());
        }
    }
    None
}

/// Build `(raw_diff, extra)` for a single FileWrite tool invocation.
///
/// See [`file_write_diff`] for the input shapes handled; this helper
/// additionally captures the structured before/after strings in `extra`.
///
/// `before_state` is threaded through to [`file_write_diff`] for the
/// `Write { content }` shape: when `Some`, it becomes the pre-image and
/// is also recorded in `extra["before"]`. When `None`, the diff falls
/// back to an empty pre-image (addition-only hunk).
fn file_write_change(
    tool: &ToolInvocation,
    path: &str,
    before_state: Option<&str>,
) -> (Option<String>, HashMap<String, serde_json::Value>) {
    let input = &tool.input;
    let str_field = |k: &str| input.get(k).and_then(|v| v.as_str()).map(str::to_string);

    let mut extra: HashMap<String, serde_json::Value> = HashMap::new();

    if let (Some(old), Some(new)) = (str_field("old_string"), str_field("new_string")) {
        extra.insert("before".to_string(), serde_json::Value::String(old.clone()));
        extra.insert("after".to_string(), serde_json::Value::String(new.clone()));
    } else if let Some(content) = str_field("content") {
        if let Some(before) = before_state {
            extra.insert(
                "before".to_string(),
                serde_json::Value::String(before.to_string()),
            );
        }
        extra.insert("after".to_string(), serde_json::Value::String(content));
    } else if let Some(edits) = input.get("edits").and_then(|v| v.as_array()) {
        extra.insert("edits".to_string(), serde_json::Value::Array(edits.clone()));
    }

    (
        file_write_diff(&tool.name, input, path, before_state),
        extra,
    )
}

/// Compute a unified diff string for a file-write tool invocation, given the
/// raw tool input JSON. Handles Claude's Edit / Write / MultiEdit / NotebookEdit
/// shapes; returns `None` for any unrecognised shape or if nothing to diff.
///
/// Exposed so non-Conversation derivers (e.g. `toolpath-claude`'s bespoke
/// Claude-JSONL deriver, which emits its own `tool.invoke` steps) can populate
/// `ArtifactChange.raw` without reimplementing the diff logic.
///
/// Shapes handled:
///   - `Edit    { old_string, new_string, ... }`  → diff old→new
///   - `Write   { content }`                      → diff `before_state`→content
///     (uses `""` when `before_state` is `None`, producing an addition-only hunk)
///   - `MultiEdit { edits: [{old_string, new_string}, ...] }` → hunks joined,
///     each prefixed with `# edit N/total` so consumers can tell them apart.
///
/// # `before_state` for `Write`
///
/// The `Write` tool replaces a file's whole contents but the JSONL log
/// doesn't carry the prior state. Callers that can reconstruct it
/// out-of-band (e.g. by reading `git show HEAD:<path>`) should pass it
/// as `before_state`; the resulting diff shows honest `-`/`+` lines for
/// replaced content. When `None`, we fall back to diffing against the
/// empty string — correct for new files, misleading for overwrites, but
/// the best we can do from the log alone.
///
/// `before_state` is ignored for `Edit` / `MultiEdit` shapes, which
/// already carry their own `old_string`/`new_string` pre-image.
pub fn file_write_diff(
    tool_name: &str,
    input: &serde_json::Value,
    path: &str,
    before_state: Option<&str>,
) -> Option<String> {
    let str_field = |k: &str| input.get(k).and_then(|v| v.as_str());

    // Edit / NotebookEdit / anything else with old/new pair.
    if let (Some(old), Some(new)) = (str_field("old_string"), str_field("new_string")) {
        return Some(unified_diff(path, old, new));
    }

    // Write — whole-file content; diff against the caller-supplied
    // before-state when present, else empty (addition-only hunk).
    if let Some(content) = str_field("content") {
        let before = before_state.unwrap_or("");
        return Some(unified_diff(path, before, content));
    }

    // MultiEdit — multiple sequential edits on one file.
    if let Some(edits) = input.get("edits").and_then(|v| v.as_array()) {
        if edits.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        for (idx, edit) in edits.iter().enumerate() {
            let old = edit
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new = edit
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let header = format!("# edit {}/{}", idx + 1, edits.len());
            parts.push(format!("{header}\n{}", unified_diff(path, old, new)));
        }
        return Some(parts.join("\n"));
    }

    // Unused today, but keeps `tool_name` addressable for future per-tool
    // branches (e.g. NotebookEdit may one day need cell-scoped diffs).
    let _ = tool_name;
    None
}

/// Produce a minimal unified-diff string using `similar::TextDiff`.
///
/// Always emits a `--- a/{path}` / `+++ b/{path}` header even when one side is
/// empty so downstream renderers can anchor the change to the file it touched.
///
/// Any leading `/` on `path` is stripped before splicing into the header —
/// git-style `a/` and `b/` prefixes already denote the repo root, so an
/// absolute path like `/abs/file.rs` would otherwise emit `--- a//abs/file.rs`,
/// which breaks `patch(1)` and other consumers that parse the header.
pub fn unified_diff(path: &str, before: &str, after: &str) -> String {
    use similar::TextDiff;
    let diff = TextDiff::from_lines(before, after);
    let display = path.trim_start_matches('/');
    let mut out = String::new();
    out.push_str(&format!("--- a/{display}\n+++ b/{display}\n"));
    out.push_str(
        &diff
            .unified_diff()
            .context_radius(3)
            .header("", "")
            .to_string(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DelegatedWork, EnvironmentSnapshot, TokenUsage, ToolInvocation, ToolResult};

    fn base_turn(id: &str, role: Role) -> Turn {
        Turn {
            id: id.to_string(),
            parent_id: None,
            role,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            text: String::new(),
            thinking: None,
            tool_uses: vec![],
            model: None,
            stop_reason: None,
            token_usage: None,
            environment: None,
            delegations: vec![],
            extra: HashMap::new(),
        }
    }

    fn view_with(turns: Vec<Turn>) -> ConversationView {
        ConversationView {
            id: "abcdef012345".to_string(),
            started_at: None,
            last_activity: None,
            turns,
            total_usage: None,
            provider_id: Some("pi".to_string()),
            files_changed: vec![],
            session_ids: vec![],
            events: vec![],
        }
    }

    fn conv_change(step: &Step) -> &StructuralChange {
        let key = step
            .change
            .keys()
            .find(|k| k.contains("://"))
            .expect("conversation artifact key present");
        step.change[key].structural.as_ref().unwrap()
    }

    #[test]
    fn test_empty_view() {
        let view = view_with(vec![]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(path.steps.is_empty());
        assert_eq!(path.path.head, "");
    }

    #[test]
    fn test_single_user_turn() {
        let mut turn = base_turn("t1", Role::User);
        turn.text = "hello".into();
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps.len(), 1);
        assert_eq!(path.steps[0].step.actor, "human:user");
        assert_eq!(path.steps[0].step.id, "step-0001");
    }

    #[test]
    fn test_single_assistant_turn() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.model = Some("claude-opus-4-7".into());
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[0].step.actor, "agent:claude-opus-4-7");
    }

    #[test]
    fn test_assistant_without_model() {
        let turn = base_turn("t1", Role::Assistant);
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[0].step.actor, "agent:unknown");
    }

    #[test]
    fn test_system_role() {
        let turn = base_turn("t1", Role::System);
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[0].step.actor, "system:pi");
    }

    #[test]
    fn test_other_role() {
        let turn = base_turn("t1", Role::Other("tool".into()));
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[0].step.actor, "tool:unknown");
    }

    #[test]
    fn test_parent_id_preserved() {
        let t1 = base_turn("t1", Role::User);
        let mut t2 = base_turn("t2", Role::Assistant);
        t2.parent_id = Some("t1".into());
        t2.model = Some("m".into());
        let view = view_with(vec![t1, t2]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[1].step.parents, vec!["step-0001".to_string()]);
    }

    fn fw_tool(name: &str, id: &str, input: serde_json::Value) -> ToolInvocation {
        ToolInvocation {
            id: id.to_string(),
            name: name.to_string(),
            input,
            result: None,
            category: Some(ToolCategory::FileWrite),
        }
    }

    #[test]
    fn test_tool_use_filewrite_with_file_path_field() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool(
            "Write",
            "tu1",
            serde_json::json!({"file_path": "src/main.rs"}),
        )];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(path.steps[0].change.contains_key("src/main.rs"));
        let sc = path.steps[0].change["src/main.rs"]
            .structural
            .as_ref()
            .unwrap();
        assert_eq!(sc.change_type, "file.write");
        assert_eq!(sc.extra["tool"], serde_json::json!("Write"));
        assert_eq!(sc.extra["tool_id"], serde_json::json!("tu1"));
    }

    #[test]
    fn test_tool_use_filewrite_with_path_field() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool("Edit", "tu1", serde_json::json!({"path": "a.rs"}))];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(path.steps[0].change.contains_key("a.rs"));
    }

    #[test]
    fn test_tool_use_filewrite_with_filename_field() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool("W", "tu1", serde_json::json!({"filename": "b.rs"}))];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(path.steps[0].change.contains_key("b.rs"));
    }

    #[test]
    fn test_tool_use_filewrite_with_file_field() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool("W", "tu1", serde_json::json!({"file": "c.rs"}))];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(path.steps[0].change.contains_key("c.rs"));
    }

    #[test]
    fn test_tool_use_filewrite_no_recognized_field() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool("W", "tu1", serde_json::json!({"other": "foo"}))];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[0].change.len(), 1);
        let sc = conv_change(&path.steps[0]);
        assert!(sc.extra.contains_key("tool_uses"));
    }

    #[test]
    fn test_tool_use_non_filewrite_ignored() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![ToolInvocation {
            id: "tu1".into(),
            name: "Read".into(),
            input: serde_json::json!({"file_path": "x.rs"}),
            result: None,
            category: Some(ToolCategory::FileRead),
        }];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(!path.steps[0].change.contains_key("x.rs"));
        assert_eq!(path.steps[0].change.len(), 1);
    }

    #[test]
    fn test_tool_use_edit_emits_unified_diff() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool(
            "Edit",
            "tu1",
            serde_json::json!({
                "file_path": "src/login.rs",
                "old_string": "validate_token()",
                "new_string": "validate_token_v2()",
            }),
        )];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let ch = &path.steps[0].change["src/login.rs"];
        let raw = ch.raw.as_deref().expect("edit should emit unified diff");
        assert!(raw.contains("--- a/src/login.rs"));
        assert!(raw.contains("+++ b/src/login.rs"));
        assert!(raw.contains("-validate_token()"));
        assert!(raw.contains("+validate_token_v2()"));
        let sc = ch.structural.as_ref().unwrap();
        assert_eq!(sc.extra["before"], serde_json::json!("validate_token()"));
        assert_eq!(sc.extra["after"], serde_json::json!("validate_token_v2()"));
    }

    #[test]
    fn test_tool_use_write_emits_full_content_diff() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool(
            "Write",
            "tu1",
            serde_json::json!({
                "file_path": "hello.txt",
                "content": "hi\nthere\n",
            }),
        )];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let ch = &path.steps[0].change["hello.txt"];
        let raw = ch.raw.as_deref().expect("write should emit diff");
        assert!(raw.contains("+hi"));
        assert!(raw.contains("+there"));
        let sc = ch.structural.as_ref().unwrap();
        assert_eq!(sc.extra["after"], serde_json::json!("hi\nthere\n"));
        assert!(!sc.extra.contains_key("before"));
    }

    #[test]
    fn test_file_write_diff_write_without_before_state_is_addition_only() {
        // Backwards-compatible fallback: `None` → diff against "".
        let input = serde_json::json!({
            "file_path": "hello.txt",
            "content": "hi\nthere\n",
        });
        let raw =
            file_write_diff("Write", &input, "hello.txt", None).expect("write should emit diff");
        assert!(raw.contains("+hi"));
        assert!(raw.contains("+there"));
        // No `-` lines — nothing was there before.
        assert!(
            !raw.lines()
                .any(|l| l.starts_with('-') && !l.starts_with("---"))
        );
    }

    #[test]
    fn test_file_write_diff_write_with_before_state_shows_replacement() {
        let input = serde_json::json!({
            "file_path": "hello.txt",
            "content": "hi\nthere\n",
        });
        let raw = file_write_diff("Write", &input, "hello.txt", Some("bye\nfriend\n"))
            .expect("write should emit diff");
        // Before content should appear as removals.
        assert!(raw.contains("-bye"));
        assert!(raw.contains("-friend"));
        // After content should appear as additions.
        assert!(raw.contains("+hi"));
        assert!(raw.contains("+there"));
    }

    #[test]
    fn test_file_write_diff_before_state_ignored_for_edit_shape() {
        // `Edit` has its own `old_string`; supplied before_state should
        // be ignored.
        let input = serde_json::json!({
            "file_path": "a.rs",
            "old_string": "foo",
            "new_string": "bar",
        });
        let raw = file_write_diff("Edit", &input, "a.rs", Some("something else entirely"))
            .expect("edit should emit diff");
        assert!(raw.contains("-foo"));
        assert!(raw.contains("+bar"));
        assert!(!raw.contains("something else entirely"));
    }

    #[test]
    fn test_unified_diff_strips_leading_slash_on_absolute_path() {
        // Regression for #36: headers for absolute paths must not contain `a//`.
        let raw = unified_diff("/abs/path.rs", "a\n", "b\n");
        assert!(
            raw.contains("--- a/abs/path.rs\n"),
            "missing stripped --- header: {raw}"
        );
        assert!(
            raw.contains("+++ b/abs/path.rs\n"),
            "missing stripped +++ header: {raw}"
        );
        assert!(
            !raw.contains("a//"),
            "header should not contain doubled slash: {raw}"
        );
        assert!(
            !raw.contains("b//"),
            "header should not contain doubled slash: {raw}"
        );
    }

    #[test]
    fn test_unified_diff_preserves_relative_path() {
        // Relative paths (no leading slash) are unchanged — only a single
        // leading `/` is stripped.
        let raw = unified_diff("src/login.rs", "a\n", "b\n");
        assert!(raw.contains("--- a/src/login.rs\n"), "{raw}");
        assert!(raw.contains("+++ b/src/login.rs\n"), "{raw}");
    }

    #[test]
    fn test_tool_use_multiedit_emits_per_hunk_diff() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool(
            "MultiEdit",
            "tu1",
            serde_json::json!({
                "file_path": "m.rs",
                "edits": [
                    {"old_string": "foo", "new_string": "bar"},
                    {"old_string": "baz", "new_string": "qux"},
                ],
            }),
        )];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let ch = &path.steps[0].change["m.rs"];
        let raw = ch.raw.as_deref().expect("multiedit should emit diff");
        assert!(raw.contains("# edit 1/2"));
        assert!(raw.contains("# edit 2/2"));
        assert!(raw.contains("-foo"));
        assert!(raw.contains("+bar"));
        assert!(raw.contains("-baz"));
        assert!(raw.contains("+qux"));
    }

    #[test]
    fn test_thinking_included_when_enabled() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.thinking = Some("hmm".into());
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let sc = conv_change(&path.steps[0]);
        assert_eq!(sc.extra["thinking"], serde_json::json!("hmm"));
    }

    #[test]
    fn test_thinking_omitted_when_disabled() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.thinking = Some("hmm".into());
        let view = view_with(vec![turn]);
        let cfg = DeriveConfig {
            include_thinking: false,
            ..Default::default()
        };
        let path = derive_path(&view, &cfg);
        let sc = conv_change(&path.steps[0]);
        assert!(!sc.extra.contains_key("thinking"));
    }

    #[test]
    fn test_tool_uses_included_when_enabled() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![ToolInvocation {
            id: "tu1".into(),
            name: "Read".into(),
            input: serde_json::json!({}),
            result: Some(ToolResult {
                content: "x".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileRead),
        }];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let sc = conv_change(&path.steps[0]);
        assert!(sc.extra.contains_key("tool_uses"));
    }

    #[test]
    fn test_tool_uses_omitted_when_disabled() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![ToolInvocation {
            id: "tu1".into(),
            name: "Read".into(),
            input: serde_json::json!({}),
            result: None,
            category: Some(ToolCategory::FileRead),
        }];
        let view = view_with(vec![turn]);
        let cfg = DeriveConfig {
            include_tool_uses: false,
            ..Default::default()
        };
        let path = derive_path(&view, &cfg);
        let sc = conv_change(&path.steps[0]);
        assert!(!sc.extra.contains_key("tool_uses"));
    }

    #[test]
    fn test_base_uri_from_working_dir() {
        let mut turn = base_turn("t1", Role::User);
        turn.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/Users/alex/proj".into()),
            ..Default::default()
        });
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.path.base.unwrap().uri, "file:///Users/alex/proj");
    }

    #[test]
    fn test_base_uri_from_config_override() {
        let mut turn = base_turn("t1", Role::User);
        turn.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/Users/alex/proj".into()),
            ..Default::default()
        });
        let view = view_with(vec![turn]);
        let cfg = DeriveConfig {
            base_uri: Some("github:org/repo".into()),
            ..Default::default()
        };
        let path = derive_path(&view, &cfg);
        assert_eq!(path.path.base.unwrap().uri, "github:org/repo");
    }

    #[test]
    fn test_base_uri_absent_when_no_source() {
        let turn = base_turn("t1", Role::User);
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(path.path.base.is_none());
    }

    #[test]
    fn test_path_id_from_config_override() {
        let view = view_with(vec![]);
        let cfg = DeriveConfig {
            path_id: Some("my-custom-id".into()),
            ..Default::default()
        };
        let path = derive_path(&view, &cfg);
        assert_eq!(path.path.id, "my-custom-id");
    }

    #[test]
    fn test_path_id_default_format() {
        let view = view_with(vec![]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.path.id, "path-pi-abcdef01");
    }

    #[test]
    fn test_files_changed_in_meta() {
        let mut view = view_with(vec![]);
        view.files_changed = vec!["a.rs".into(), "b.rs".into()];
        let path = derive_path(&view, &DeriveConfig::default());
        let meta = path.meta.unwrap();
        assert_eq!(
            meta.extra["files_changed"],
            serde_json::json!(["a.rs", "b.rs"])
        );
    }

    #[test]
    fn test_actors_in_meta() {
        let u = base_turn("t1", Role::User);
        let mut a = base_turn("t2", Role::Assistant);
        a.model = Some("claude-opus-4-7".into());
        let view = view_with(vec![u, a]);
        let path = derive_path(&view, &DeriveConfig::default());
        let actors = path.meta.unwrap().actors.unwrap();
        assert!(actors.contains_key("human:user"));
        assert!(actors.contains_key("agent:claude-opus-4-7"));
        let agent = &actors["agent:claude-opus-4-7"];
        assert_eq!(agent.provider.as_deref(), Some("pi"));
        assert_eq!(agent.model.as_deref(), Some("claude-opus-4-7"));
        let human = &actors["human:user"];
        assert_eq!(human.name.as_deref(), Some("user"));
    }

    #[test]
    fn test_head_is_last_step_id() {
        let turns = vec![
            base_turn("t1", Role::User),
            base_turn("t2", Role::User),
            base_turn("t3", Role::User),
        ];
        let view = view_with(turns);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.path.head, "step-0003");
    }

    #[test]
    fn test_token_usage_in_extras() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.token_usage = Some(TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            cache_read_tokens: None,
            cache_write_tokens: None,
        });
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let sc = conv_change(&path.steps[0]);
        assert!(sc.extra.contains_key("token_usage"));
        assert_eq!(
            sc.extra["token_usage"]["input_tokens"],
            serde_json::json!(100)
        );
    }

    #[test]
    fn test_delegations_in_extras() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.delegations = vec![DelegatedWork {
            agent_id: "sub-1".into(),
            prompt: "do a thing".into(),
            turns: vec![],
            result: None,
        }];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let sc = conv_change(&path.steps[0]);
        assert!(sc.extra.contains_key("delegations"));
        assert_eq!(
            sc.extra["delegations"][0]["agent_id"],
            serde_json::json!("sub-1")
        );
    }

    #[test]
    fn test_title_from_config() {
        let view = view_with(vec![]);
        let cfg = DeriveConfig {
            title: Some("My Session".into()),
            ..Default::default()
        };
        let path = derive_path(&view, &cfg);
        assert_eq!(path.meta.unwrap().title.as_deref(), Some("My Session"));
    }

    #[test]
    fn test_title_default_when_unset() {
        let view = view_with(vec![]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(
            path.meta.unwrap().title.as_deref(),
            Some("pi session: abcdef01")
        );
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut t1 = base_turn("t1", Role::User);
        t1.text = "hello".into();
        t1.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/proj".into()),
            ..Default::default()
        });
        let mut t2 = base_turn("t2", Role::Assistant);
        t2.parent_id = Some("t1".into());
        t2.model = Some("m".into());
        t2.tool_uses = vec![fw_tool(
            "Write",
            "tu1",
            serde_json::json!({"file_path": "x.rs"}),
        )];

        let mut view = view_with(vec![t1, t2]);
        view.files_changed = vec!["x.rs".into()];

        let path = derive_path(&view, &DeriveConfig::default());
        let json = serde_json::to_string(&path).unwrap();
        let back: Path = serde_json::from_str(&json).unwrap();
        assert_eq!(back.path.id, path.path.id);
        assert_eq!(back.path.head, path.path.head);
        assert_eq!(back.steps.len(), 2);
        assert_eq!(back.steps[1].step.parents, vec!["step-0001".to_string()]);
        assert!(back.steps[1].change.contains_key("x.rs"));
    }
}
