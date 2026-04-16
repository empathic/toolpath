# Conversation Projection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable round-trip conversion between toolpath documents and agent conversation formats, starting with Claude JSONL.

**Architecture:** Three-layer serde pattern — `toolpath` (grammar), `toolpath-convo` (conversation sub-protocol + extraction + projection trait), `toolpath-claude` (Claude JSONL ↔ sub-protocol). Both derive and project directions pass through `ConversationView` as the narrow waist.

**Tech Stack:** Rust, serde_json, existing toolpath/toolpath-convo/toolpath-claude crates.

**Spec:** `docs/superpowers/specs/2026-04-16-conversation-projection-design.md`

---

### Task 1: Add `toolpath` dependency to `toolpath-convo`

**Files:**
- Modify: `crates/toolpath-convo/Cargo.toml`
- Modify: `Cargo.toml` (workspace root — no change needed, `toolpath` already in workspace deps)

- [ ] **Step 1: Add dependency**

In `crates/toolpath-convo/Cargo.toml`, add `toolpath` to `[dependencies]`:

```toml
[dependencies]
toolpath = { workspace = true }
chrono = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p toolpath-convo`
Expected: Compiles with no errors.

- [ ] **Step 3: Run existing tests**

Run: `cargo test -p toolpath-convo`
Expected: All 28 unit tests + 1 doc test pass (no regressions).

- [ ] **Step 4: Commit**

```bash
git add crates/toolpath-convo/Cargo.toml
git commit -m "chore: add toolpath dependency to toolpath-convo"
```

---

### Task 2: Add `ConversationProjector` trait and `AnyProjector` to `toolpath-convo`

**Files:**
- Create: `crates/toolpath-convo/src/project.rs`
- Modify: `crates/toolpath-convo/src/lib.rs` (add `pub mod project;` and re-exports)

- [ ] **Step 1: Write the tests for `ConversationProjector` and `AnyProjector`**

Create `crates/toolpath-convo/src/project.rs`:

```rust
use std::any::Any;

use crate::{ConversationView, ConvoError, Result};

/// Trait for projecting a [`ConversationView`] into a provider's native
/// conversation format.
///
/// This is the "serialize" half of the serde pattern — the inverse of
/// [`ConversationProvider`](crate::ConversationProvider) which loads
/// native formats into `ConversationView`.
///
/// Each provider implements this with its own `Output` type:
/// - Claude: `Output = toolpath_claude::Conversation`
/// - Future providers define their own native types.
pub trait ConversationProjector {
    /// The provider's native conversation type.
    type Output;

    /// Project a provider-neutral conversation into the native format.
    fn project(&self, view: &ConversationView) -> Result<Self::Output>;
}

// ── Type-erased wrapper ─────────────────────────────────────────────

trait ErasedProjector: Send + Sync {
    fn project_erased(&self, view: &ConversationView) -> Result<Box<dyn Any>>;
}

impl<T> ErasedProjector for T
where
    T: ConversationProjector + Send + Sync + 'static,
    T::Output: 'static,
{
    fn project_erased(&self, view: &ConversationView) -> Result<Box<dyn Any>> {
        self.project(view).map(|o| Box::new(o) as Box<dyn Any>)
    }
}

/// Type-erased wrapper around any [`ConversationProjector`].
///
/// Use this when you need dynamic dispatch over projectors with
/// different `Output` types. The caller downcasts the result.
///
/// ```
/// use toolpath_convo::project::{ConversationProjector, AnyProjector};
/// use toolpath_convo::{ConversationView, Result};
///
/// struct MockProjector;
/// impl ConversationProjector for MockProjector {
///     type Output = Vec<String>;
///     fn project(&self, view: &ConversationView) -> Result<Self::Output> {
///         Ok(view.turns.iter().map(|t| t.text.clone()).collect())
///     }
/// }
///
/// let any = AnyProjector::new(MockProjector);
/// # let view = ConversationView {
/// #     id: "test".into(), started_at: None, last_activity: None,
/// #     turns: vec![], total_usage: None, provider_id: None,
/// #     files_changed: vec![], session_ids: vec![],
/// # };
/// let texts: Vec<String> = any.project_as(&view).unwrap();
/// assert!(texts.is_empty());
/// ```
pub struct AnyProjector(Box<dyn ErasedProjector>);

impl AnyProjector {
    /// Wrap a concrete projector in a type-erased container.
    pub fn new<P>(projector: P) -> Self
    where
        P: ConversationProjector + Send + Sync + 'static,
        P::Output: 'static,
    {
        Self(Box::new(projector))
    }

    /// Project and return the result as `Box<dyn Any>`.
    pub fn project(&self, view: &ConversationView) -> Result<Box<dyn Any>> {
        self.0.project_erased(view)
    }

    /// Project and downcast to the expected output type.
    ///
    /// Returns `Err` if the projection fails or the downcast fails.
    pub fn project_as<T: 'static>(&self, view: &ConversationView) -> Result<T> {
        let boxed = self.0.project_erased(view)?;
        boxed
            .downcast::<T>()
            .map(|b| *b)
            .map_err(|_| ConvoError::Provider("projection output type mismatch".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConversationView;

    struct TestProjector;

    impl ConversationProjector for TestProjector {
        type Output = Vec<String>;

        fn project(&self, view: &ConversationView) -> Result<Self::Output> {
            Ok(view.turns.iter().map(|t| t.text.clone()).collect())
        }
    }

    fn empty_view() -> ConversationView {
        ConversationView {
            id: "test".into(),
            started_at: None,
            last_activity: None,
            turns: vec![],
            total_usage: None,
            provider_id: None,
            files_changed: vec![],
            session_ids: vec![],
        }
    }

    #[test]
    fn test_concrete_projector() {
        let projector = TestProjector;
        let result = projector.project(&empty_view()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_any_projector_project() {
        let any = AnyProjector::new(TestProjector);
        let result = any.project(&empty_view()).unwrap();
        let texts = result.downcast::<Vec<String>>().unwrap();
        assert!(texts.is_empty());
    }

    #[test]
    fn test_any_projector_project_as() {
        let any = AnyProjector::new(TestProjector);
        let texts: Vec<String> = any.project_as(&empty_view()).unwrap();
        assert!(texts.is_empty());
    }

    #[test]
    fn test_any_projector_project_as_wrong_type() {
        let any = AnyProjector::new(TestProjector);
        let result = any.project_as::<String>(&empty_view());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("type mismatch")
        );
    }

    #[test]
    fn test_any_projector_with_turns() {
        use crate::{Role, Turn};
        use std::collections::HashMap;

        let view = ConversationView {
            id: "test".into(),
            started_at: None,
            last_activity: None,
            turns: vec![
                Turn {
                    id: "t1".into(),
                    parent_id: None,
                    role: Role::User,
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    text: "Hello".into(),
                    thinking: None,
                    tool_uses: vec![],
                    model: None,
                    stop_reason: None,
                    token_usage: None,
                    environment: None,
                    delegations: vec![],
                    extra: HashMap::new(),
                },
                Turn {
                    id: "t2".into(),
                    parent_id: Some("t1".into()),
                    role: Role::Assistant,
                    timestamp: "2026-01-01T00:00:01Z".into(),
                    text: "Hi there".into(),
                    thinking: None,
                    tool_uses: vec![],
                    model: None,
                    stop_reason: None,
                    token_usage: None,
                    environment: None,
                    delegations: vec![],
                    extra: HashMap::new(),
                },
            ],
            total_usage: None,
            provider_id: None,
            files_changed: vec![],
            session_ids: vec![],
        };

        let any = AnyProjector::new(TestProjector);
        let texts: Vec<String> = any.project_as(&view).unwrap();
        assert_eq!(texts, vec!["Hello", "Hi there"]);
    }
}
```

- [ ] **Step 2: Wire up the module**

In `crates/toolpath-convo/src/lib.rs`, add after the existing module declarations (before `// ── Error`):

```rust
pub mod project;
```

And add re-exports after the existing trait section (after `ConversationWatcher`):

```rust
pub use project::{AnyProjector, ConversationProjector};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p toolpath-convo`
Expected: All existing tests pass plus the new `project::tests::*` tests (5 new tests).

- [ ] **Step 4: Commit**

```bash
git add crates/toolpath-convo/src/project.rs crates/toolpath-convo/src/lib.rs
git commit -m "feat: add ConversationProjector trait and AnyProjector wrapper"
```

---

### Task 3: Add `extract_conversation` to `toolpath-convo`

**Files:**
- Create: `crates/toolpath-convo/src/extract.rs`
- Modify: `crates/toolpath-convo/src/lib.rs` (add `pub mod extract;` and re-export)

This function walks a toolpath `Path` and reconstructs a `ConversationView` by recognizing the conversation sub-protocol's structural change types (`conversation.init`, `conversation.append`, `tool.invoke`) and actor patterns (`human:*`, `agent:*`, `*/tool:*`).

- [ ] **Step 1: Write the failing tests**

Create `crates/toolpath-convo/src/extract.rs` with tests first, empty `extract_conversation` that returns an empty view:

```rust
use std::collections::HashMap;

use crate::{
    ConversationView, EnvironmentSnapshot, Role, TokenUsage, ToolCategory, ToolInvocation,
    ToolResult, Turn,
};
use toolpath::v1::{ArtifactChange, Path, Step, StepIdentity, StructuralChange};

/// Extract a [`ConversationView`] from a toolpath [`Path`] that follows
/// the conversation sub-protocol.
///
/// Recognizes three structural change types:
/// - `conversation.init` — session metadata (populates provider_id, session info)
/// - `conversation.append` — conversation turns (creates Turn entries)
/// - `tool.invoke` — tool invocations (attaches to parent turn's tool_uses)
///
/// Actor patterns determine roles:
/// - `human:*` → [`Role::User`]
/// - `agent:*` (no `/tool:`) → [`Role::Assistant`]
/// - `*/tool:*` → tool invocation step, attached to parent turn
///
/// Steps that don't match any sub-protocol structural change type are
/// silently skipped.
pub fn extract_conversation(path: &Path) -> ConversationView {
    let mut view = ConversationView {
        id: String::new(),
        started_at: None,
        last_activity: None,
        turns: Vec::new(),
        total_usage: None,
        provider_id: None,
        files_changed: Vec::new(),
        session_ids: Vec::new(),
    };

    // Build a map from step ID → index for parent lookups
    let step_index: HashMap<&str, usize> = path
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.step.id.as_str(), i))
        .collect();

    // Track turn index by step ID for attaching tool invocations
    let mut turn_by_step_id: HashMap<&str, usize> = HashMap::new();

    for step in &path.steps {
        // Check for conversation artifact changes
        for (artifact_key, change) in &step.change {
            let Some(structural) = &change.structural else {
                continue;
            };

            match structural.change_type.as_str() {
                "conversation.init" => {
                    handle_init(&mut view, artifact_key, structural);
                }
                "conversation.append" => {
                    let turn = handle_append(step, structural);
                    turn_by_step_id.insert(&step.step.id, view.turns.len());
                    view.turns.push(turn);
                }
                "tool.invoke" => {
                    // Find the parent turn to attach this tool invocation to
                    let parent_turn_idx = step
                        .step
                        .parents
                        .iter()
                        .find_map(|pid| turn_by_step_id.get(pid.as_str()).copied());

                    if let Some(idx) = parent_turn_idx {
                        let invocation = handle_tool_invoke(artifact_key, structural);
                        view.turns[idx].tool_uses.push(invocation);

                        // Track files changed
                        if !artifact_key.starts_with("agent://") {
                            let category = invocation_category(structural);
                            if category == Some(ToolCategory::FileWrite) {
                                if !view.files_changed.contains(artifact_key) {
                                    view.files_changed.push(artifact_key.clone());
                                }
                            }
                        }
                    }
                }
                _ => {} // Unknown structural change type — skip
            }
        }
    }

    // Compute total usage
    view.total_usage = sum_usage(&view.turns);

    // Parse timestamps for started_at / last_activity
    if let Some(first) = view.turns.first() {
        view.started_at = chrono::DateTime::parse_from_rfc3339(&first.timestamp)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc));
    }
    if let Some(last) = view.turns.last() {
        view.last_activity = chrono::DateTime::parse_from_rfc3339(&last.timestamp)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc));
    }

    view
}

fn handle_init(view: &mut ConversationView, artifact_key: &str, sc: &StructuralChange) {
    // Extract session ID from agent://<provider>/<session-id>
    if let Some(rest) = artifact_key.strip_prefix("agent://") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            view.provider_id = Some(parts[0].to_string());
            view.id = parts[1].to_string();
        }
    }

    // Extract environment info from init extra fields — stored for
    // consumers but not mapped to ConversationView fields directly
    // (ConversationView doesn't have top-level env fields).
}

fn handle_append(step: &Step, sc: &StructuralChange) -> Turn {
    let role = match sc.extra.get("role").and_then(|v| v.as_str()) {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        Some("system") => Role::System,
        Some(other) => Role::Other(other.to_string()),
        None => actor_to_role(&step.step.actor),
    };

    let text = sc
        .extra
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let thinking = sc
        .extra
        .get("thinking")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let model = sc
        .extra
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let stop_reason = sc
        .extra
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let token_usage = extract_usage(sc);

    let parent_id = step.step.parents.first().map(|p| p.to_string());

    Turn {
        id: step.step.id.clone(),
        parent_id,
        role,
        timestamp: step.step.timestamp.clone(),
        text,
        thinking,
        tool_uses: Vec::new(), // Filled in by tool.invoke steps
        model,
        stop_reason,
        token_usage,
        environment: None,
        delegations: Vec::new(),
        extra: HashMap::new(),
    }
}

fn handle_tool_invoke(artifact_key: &str, sc: &StructuralChange) -> ToolInvocation {
    let id = sc
        .extra
        .get("tool_use_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let name = sc
        .extra
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let input = sc
        .extra
        .get("input")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));

    let result = sc.extra.get("result").and_then(|v| v.as_str()).map(|content| {
        let is_error = sc
            .extra
            .get("is_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        ToolResult {
            content: content.to_string(),
            is_error,
        }
    });

    let category = invocation_category(sc);

    ToolInvocation {
        id,
        name,
        input,
        result,
        category,
    }
}

fn actor_to_role(actor: &str) -> Role {
    if actor.starts_with("human:") {
        Role::User
    } else if actor.contains("/tool:") {
        // Tool actors shouldn't produce turns, but fallback
        Role::Other("tool".to_string())
    } else if actor.starts_with("agent:") {
        Role::Assistant
    } else if actor.starts_with("tool:") {
        Role::System
    } else {
        Role::Other(actor.to_string())
    }
}

fn invocation_category(sc: &StructuralChange) -> Option<ToolCategory> {
    sc.extra
        .get("category")
        .and_then(|v| v.as_str())
        .and_then(|s| match s {
            "file_read" => Some(ToolCategory::FileRead),
            "file_write" => Some(ToolCategory::FileWrite),
            "file_search" => Some(ToolCategory::FileSearch),
            "shell" => Some(ToolCategory::Shell),
            "network" => Some(ToolCategory::Network),
            "delegation" => Some(ToolCategory::Delegation),
            _ => None,
        })
}

fn extract_usage(sc: &StructuralChange) -> Option<TokenUsage> {
    let input = sc.extra.get("input_tokens").and_then(|v| v.as_u64()).map(|n| n as u32);
    let output = sc.extra.get("output_tokens").and_then(|v| v.as_u64()).map(|n| n as u32);

    if input.is_none() && output.is_none() {
        return None;
    }

    Some(TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: sc.extra.get("cache_read_tokens").and_then(|v| v.as_u64()).map(|n| n as u32),
        cache_write_tokens: sc.extra.get("cache_write_tokens").and_then(|v| v.as_u64()).map(|n| n as u32),
    })
}

fn sum_usage(turns: &[Turn]) -> Option<TokenUsage> {
    let mut total = TokenUsage::default();
    let mut any = false;
    for turn in turns {
        if let Some(u) = &turn.token_usage {
            any = true;
            total.input_tokens = Some(total.input_tokens.unwrap_or(0) + u.input_tokens.unwrap_or(0));
            total.output_tokens = Some(total.output_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0));
            total.cache_read_tokens = match (total.cache_read_tokens, u.cache_read_tokens) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            total.cache_write_tokens = match (total.cache_write_tokens, u.cache_write_tokens) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
        }
    }
    if any { Some(total) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use toolpath::v1::{PathIdentity, StepMeta};

    fn make_path(steps: Vec<Step>) -> Path {
        let head = steps.last().map(|s| s.step.id.clone()).unwrap_or_else(|| "empty".into());
        Path {
            path: PathIdentity {
                id: "test-path".into(),
                base: None,
                head,
            },
            steps,
            meta: None,
        }
    }

    fn init_step(session_id: &str, provider: &str) -> Step {
        let mut extra = HashMap::new();
        extra.insert("version".to_string(), json!("1.0.0"));
        extra.insert("working_dir".to_string(), json!("/test/project"));

        Step {
            step: StepIdentity {
                id: "step-init".into(),
                parents: vec![],
                actor: format!("tool:{}", provider),
                timestamp: "2026-01-01T00:00:00Z".into(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    format!("agent://{}/{}", provider, session_id),
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.init".into(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        }
    }

    fn append_step(id: &str, actor: &str, role: &str, text: &str, parent: Option<&str>, artifact: &str) -> Step {
        let mut extra = HashMap::new();
        extra.insert("role".to_string(), json!(role));
        extra.insert("text".to_string(), json!(text));

        Step {
            step: StepIdentity {
                id: id.into(),
                parents: parent.into_iter().map(|p| p.to_string()).collect(),
                actor: actor.into(),
                timestamp: "2026-01-01T00:00:01Z".into(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact.into(),
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".into(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        }
    }

    fn tool_step(id: &str, parent: &str, tool_name: &str, artifact_key: &str, tool_use_id: &str) -> Step {
        let mut extra = HashMap::new();
        extra.insert("tool_use_id".to_string(), json!(tool_use_id));
        extra.insert("name".to_string(), json!(tool_name));
        extra.insert("input".to_string(), json!({"file_path": "src/main.rs"}));
        extra.insert("result".to_string(), json!("fn main() {}"));
        extra.insert("is_error".to_string(), json!(false));
        extra.insert("category".to_string(), json!("file_read"));

        Step {
            step: StepIdentity {
                id: id.into(),
                parents: vec![parent.into()],
                actor: format!("agent:claude-code/tool:{}", tool_name),
                timestamp: "2026-01-01T00:00:02Z".into(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact_key.into(),
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "tool.invoke".into(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        }
    }

    #[test]
    fn test_extract_empty_path() {
        let path = make_path(vec![]);
        let view = extract_conversation(&path);
        assert!(view.turns.is_empty());
        assert!(view.id.is_empty());
    }

    #[test]
    fn test_extract_init_sets_metadata() {
        let path = make_path(vec![init_step("session-123", "claude")]);
        let view = extract_conversation(&path);
        assert_eq!(view.id, "session-123");
        assert_eq!(view.provider_id.as_deref(), Some("claude"));
    }

    #[test]
    fn test_extract_simple_conversation() {
        let artifact = "agent://claude/session-1";
        let path = make_path(vec![
            init_step("session-1", "claude"),
            append_step("u1", "human:user", "user", "Hello", Some("step-init"), artifact),
            append_step("a1", "agent:claude-code", "assistant", "Hi there", Some("u1"), artifact),
        ]);

        let view = extract_conversation(&path);
        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[0].text, "Hello");
        assert_eq!(view.turns[1].role, Role::Assistant);
        assert_eq!(view.turns[1].text, "Hi there");
    }

    #[test]
    fn test_extract_with_tool_invocations() {
        let artifact = "agent://claude/session-1";
        let path = make_path(vec![
            append_step("u1", "human:user", "user", "Read the file", None, artifact),
            append_step("a1", "agent:claude-code", "assistant", "Reading...", Some("u1"), artifact),
            tool_step("t1", "a1", "Read", "src/main.rs", "tool-1"),
        ]);

        let view = extract_conversation(&path);
        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[1].tool_uses.len(), 1);
        assert_eq!(view.turns[1].tool_uses[0].name, "Read");
        assert_eq!(view.turns[1].tool_uses[0].id, "tool-1");
        assert_eq!(view.turns[1].tool_uses[0].result.as_ref().unwrap().content, "fn main() {}");
        assert!(!view.turns[1].tool_uses[0].result.as_ref().unwrap().is_error);
        assert_eq!(view.turns[1].tool_uses[0].category, Some(ToolCategory::FileRead));
    }

    #[test]
    fn test_extract_with_token_usage() {
        let artifact = "agent://claude/session-1";
        let mut step = append_step("a1", "agent:claude-code", "assistant", "Hi", None, artifact);
        // Add usage fields to the structural change
        if let Some(sc) = step.change.get_mut(artifact).and_then(|c| c.structural.as_mut()) {
            sc.extra.insert("input_tokens".to_string(), json!(100));
            sc.extra.insert("output_tokens".to_string(), json!(50));
            sc.extra.insert("cache_read_tokens".to_string(), json!(500));
        }

        let path = make_path(vec![step]);
        let view = extract_conversation(&path);

        let usage = view.turns[0].token_usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
        assert_eq!(usage.cache_read_tokens, Some(500));
        assert!(usage.cache_write_tokens.is_none());

        let total = view.total_usage.as_ref().unwrap();
        assert_eq!(total.input_tokens, Some(100));
    }

    #[test]
    fn test_extract_with_thinking() {
        let artifact = "agent://claude/session-1";
        let mut step = append_step("a1", "agent:claude-code", "assistant", "Answer", None, artifact);
        if let Some(sc) = step.change.get_mut(artifact).and_then(|c| c.structural.as_mut()) {
            sc.extra.insert("thinking".to_string(), json!("Let me think about this..."));
        }

        let path = make_path(vec![step]);
        let view = extract_conversation(&path);

        assert_eq!(view.turns[0].thinking.as_deref(), Some("Let me think about this..."));
    }

    #[test]
    fn test_extract_parent_chain() {
        let artifact = "agent://claude/session-1";
        let path = make_path(vec![
            append_step("u1", "human:user", "user", "First", None, artifact),
            append_step("a1", "agent:claude-code", "assistant", "Reply 1", Some("u1"), artifact),
            append_step("u2", "human:user", "user", "Second", Some("a1"), artifact),
        ]);

        let view = extract_conversation(&path);
        assert!(view.turns[0].parent_id.is_none());
        assert_eq!(view.turns[1].parent_id.as_deref(), Some("u1"));
        assert_eq!(view.turns[2].parent_id.as_deref(), Some("a1"));
    }

    #[test]
    fn test_extract_skips_unknown_structural_types() {
        let path = make_path(vec![Step {
            step: StepIdentity {
                id: "s1".into(),
                parents: vec![],
                actor: "human:alex".into(),
                timestamp: "2026-01-01T00:00:00Z".into(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    "src/main.rs".into(),
                    ArtifactChange {
                        raw: Some("@@ -1 +1 @@\n-old\n+new".into()),
                        structural: Some(StructuralChange {
                            change_type: "code.edit".into(),
                            extra: HashMap::new(),
                        }),
                    },
                );
                m
            },
            meta: None,
        }]);

        let view = extract_conversation(&path);
        assert!(view.turns.is_empty());
    }

    #[test]
    fn test_extract_role_from_actor_fallback() {
        let artifact = "agent://claude/session-1";
        // Step without explicit role in extra — should fall back to actor pattern
        let mut step = append_step("u1", "human:alex", "user", "Hello", None, artifact);
        if let Some(sc) = step.change.get_mut(artifact).and_then(|c| c.structural.as_mut()) {
            sc.extra.remove("role");
        }

        let path = make_path(vec![step]);
        let view = extract_conversation(&path);
        assert_eq!(view.turns[0].role, Role::User);
    }

    #[test]
    fn test_extract_multiple_tools_same_turn() {
        let artifact = "agent://claude/session-1";
        let path = make_path(vec![
            append_step("a1", "agent:claude-code", "assistant", "Reading files...", None, artifact),
            tool_step("t1", "a1", "Read", "src/main.rs", "tool-1"),
            tool_step("t2", "a1", "Read", "src/lib.rs", "tool-2"),
        ]);

        let view = extract_conversation(&path);
        assert_eq!(view.turns.len(), 1);
        // Both tool steps should attach to the same assistant turn
        assert_eq!(view.turns[0].tool_uses.len(), 2);
    }
}
```

- [ ] **Step 2: Wire up the module**

In `crates/toolpath-convo/src/lib.rs`, add the module and re-export:

```rust
pub mod extract;
```

After the existing re-exports:

```rust
pub use extract::extract_conversation;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p toolpath-convo`
Expected: All existing tests pass plus the new `extract::tests::*` tests (9 new tests).

- [ ] **Step 4: Commit**

```bash
git add crates/toolpath-convo/src/extract.rs crates/toolpath-convo/src/lib.rs
git commit -m "feat: add extract_conversation for toolpath Path to ConversationView"
```

---

### Task 4: Enriched derive in `toolpath-claude`

**Files:**
- Modify: `crates/toolpath-claude/src/derive.rs`

This is the largest task. The current `derive_path()` is upgraded to produce sub-protocol-compliant output: full text, tool invocation steps, `agent://` URNs, token usage, init steps.

- [ ] **Step 1: Update tests to expect enriched output**

In `crates/toolpath-claude/src/derive.rs`, update the existing tests. Replace the test helper and tests. The key changes:

1. `convo_artifact` format changes from `claude://<session-id>` to `agent://claude/<session-id>`
2. Step IDs use full UUIDs (not 8-char prefixes)
3. Tool invocations produce separate steps with `agent:claude-code/tool:<Name>` actors
4. `conversation.append` extra fields include full text (no truncation), `model`, `stop_reason`
5. `DeriveConfig.include_thinking` defaults to `true`

Replace the `test_derive_path_conversation_artifact` test:

```rust
#[test]
fn test_derive_path_conversation_artifact() {
    let entries = vec![make_entry(
        "uuid-1111",
        MessageRole::User,
        "Hello",
        "2024-01-01T00:00:00Z",
    )];
    let convo = make_conversation(entries);
    let config = DeriveConfig::default();

    let path = derive_path(&convo, &config);

    let convo_key = format!("agent://claude/{}", convo.session_id);
    assert!(path.steps[0].change.contains_key(&convo_key));

    let change = &path.steps[0].change[&convo_key];
    let structural = change.structural.as_ref().unwrap();
    assert_eq!(structural.change_type, "conversation.append");
    assert_eq!(structural.extra["role"], "user");
    assert_eq!(structural.extra["text"], "Hello");
}
```

Replace the `test_derive_path_basic` test:

```rust
#[test]
fn test_derive_path_basic() {
    let entries = vec![
        make_entry("uuid-1111-aaaa", MessageRole::User, "Hello", "2024-01-01T00:00:00Z"),
        make_entry("uuid-2222-bbbb", MessageRole::Assistant, "Hi there", "2024-01-01T00:00:01Z"),
    ];
    let convo = make_conversation(entries);
    let config = DeriveConfig::default();

    let path = derive_path(&convo, &config);

    assert!(path.path.id.starts_with("path-claude-"));
    // Conversation turns produce steps (no tool steps in this case)
    assert_eq!(path.steps.len(), 2);
    assert_eq!(path.steps[0].step.actor, "human:user");
    assert!(path.steps[1].step.actor.starts_with("agent:"));
    // Step IDs are full UUIDs
    assert_eq!(path.steps[0].step.id, "uuid-1111-aaaa");
    assert_eq!(path.steps[1].step.id, "uuid-2222-bbbb");
}
```

Add a new test for tool invocation steps:

```rust
#[test]
fn test_derive_path_tool_invocation_steps() {
    let mut convo = Conversation::new("test-session-12345678".to_string());
    let entry = ConversationEntry {
        parent_uuid: None,
        is_sidechain: false,
        entry_type: "assistant".to_string(),
        uuid: "uuid-tool-1234".to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
        session_id: Some("test-session".to_string()),
        message: Some(Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Parts(vec![
                ContentPart::Text { text: "Let me read that".to_string() },
                ContentPart::ToolUse {
                    id: "t1".to_string(),
                    name: "Read".to_string(),
                    input: serde_json::json!({"file_path": "/tmp/test.rs"}),
                },
            ])),
            model: Some("claude-opus-4-6".to_string()),
            id: None,
            message_type: None,
            stop_reason: Some("tool_use".to_string()),
            stop_sequence: None,
            usage: Some(Usage {
                input_tokens: Some(100),
                output_tokens: Some(50),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            }),
        }),
        cwd: None,
        git_branch: None,
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    };
    convo.add_entry(entry);
    let config = DeriveConfig::default();

    let path = derive_path(&convo, &config);

    // Should have 2 steps: conversation step + tool step
    assert_eq!(path.steps.len(), 2);

    // First step: conversation.append
    let convo_step = &path.steps[0];
    assert_eq!(convo_step.step.actor, "agent:claude-opus-4-6");
    let convo_key = format!("agent://claude/{}", convo.session_id);
    let sc = convo_step.change[&convo_key].structural.as_ref().unwrap();
    assert_eq!(sc.change_type, "conversation.append");
    assert_eq!(sc.extra["text"], "Let me read that");
    assert_eq!(sc.extra["stop_reason"], "tool_use");
    assert_eq!(sc.extra["model"], "claude-opus-4-6");
    assert_eq!(sc.extra["input_tokens"], 100);
    assert_eq!(sc.extra["output_tokens"], 50);

    // Second step: tool.invoke
    let tool_step = &path.steps[1];
    assert_eq!(tool_step.step.actor, "agent:claude-code/tool:Read");
    assert!(tool_step.step.parents.contains(&convo_step.step.id));
    assert!(tool_step.change.contains_key("/tmp/test.rs"));
    let tool_sc = tool_step.change["/tmp/test.rs"].structural.as_ref().unwrap();
    assert_eq!(tool_sc.change_type, "tool.invoke");
    assert_eq!(tool_sc.extra["name"], "Read");
    assert_eq!(tool_sc.extra["tool_use_id"], "t1");
    assert_eq!(tool_sc.extra["category"], "file_read");
}
```

Add test for untruncated text:

```rust
#[test]
fn test_derive_path_no_truncation() {
    let long_text = "a".repeat(5000);
    let entries = vec![make_entry("uuid-1111", MessageRole::User, &long_text, "2024-01-01T00:00:00Z")];
    let convo = make_conversation(entries);
    let config = DeriveConfig::default();

    let path = derive_path(&convo, &config);

    let convo_key = format!("agent://claude/{}", convo.session_id);
    let sc = path.steps[0].change[&convo_key].structural.as_ref().unwrap();
    assert_eq!(sc.extra["text"].as_str().unwrap().len(), 5000);
}
```

- [ ] **Step 2: Run tests to see them fail**

Run: `cargo test -p toolpath-claude derive`
Expected: New/updated tests fail (old artifact key format, truncation, no tool steps).

- [ ] **Step 3: Update `derive_path()` implementation**

Rewrite the `derive_path()` function in `crates/toolpath-claude/src/derive.rs`. Key changes:

1. Change `convo_artifact` to `format!("agent://claude/{}", conversation.session_id)`
2. Change step ID from `format!("step-{}", safe_prefix(&entry.uuid, 8))` to `entry.uuid.clone()`
3. Remove truncation — use full text
4. Default `include_thinking` to `true`
5. Add `model`, `stop_reason`, token usage fields to `conversation.append` extra
6. Collect tool invocations into separate steps with actor `agent:claude-code/tool:<Name>`
7. For tool artifact keys: file paths for file tools (Read/Write/Edit/Glob/Grep), `agent://claude/<session>/tool/<category>/<tool_use_id>` for others
8. Add `category` field to tool.invoke structural changes using the same `tool_category()` function from `provider.rs`
9. Handle tool results from subsequent tool-result-only entries (scan ahead or use the provider's cross-entry assembly)

The tool category function is already in `provider.rs`. Make it `pub(crate)` so `derive.rs` can use it, or duplicate the simple match.

Update `DeriveConfig`:

```rust
#[derive(Default)]
pub struct DeriveConfig {
    pub project_path: Option<String>,
    pub include_thinking: bool,
}
```

Note: `include_thinking` keeps its current default of `false` for backward compatibility in the `DeriveConfig` struct, but the CLI default changes (see Task 7). The enriched derive always captures full text and tool invocations regardless.

- [ ] **Step 4: Run tests**

Run: `cargo test -p toolpath-claude derive`
Expected: All derive tests pass.

- [ ] **Step 5: Fix any other tests broken by the enriched output**

Run: `cargo test --workspace`
Expected: Some tests in `toolpath-cli` may fail due to changed derive output (artifact keys, step counts). Fix those tests.

- [ ] **Step 6: Commit**

```bash
git add crates/toolpath-claude/src/derive.rs crates/toolpath-claude/src/provider.rs
git commit -m "feat: enriched derive with sub-protocol compliance

- Full text (no truncation), thinking, token usage, stop_reason, model
- Tool invocations as separate steps with agent:*/tool:* actors
- agent://claude/<session-id> URN scheme
- Full UUID step IDs"
```

---

### Task 5: Claude `ConversationProjector` implementation

**Files:**
- Create: `crates/toolpath-claude/src/project.rs`
- Modify: `crates/toolpath-claude/src/lib.rs` (add `pub mod project;`)

- [ ] **Step 1: Write failing tests**

Create `crates/toolpath-claude/src/project.rs`:

```rust
use crate::types::{
    ContentPart, Conversation, ConversationEntry, Message, MessageContent, MessageRole,
    ToolResultContent, Usage,
};
use toolpath_convo::{
    ConversationProjector, ConversationView, ConvoError, Role, TokenUsage, ToolCategory,
    ToolInvocation, ToolResult, Turn,
};

use std::collections::HashMap;

/// Projects a [`ConversationView`] into Claude's native [`Conversation`] format.
///
/// Handles:
/// - User/assistant turns → `ConversationEntry` with appropriate message content
/// - Tool invocations → `ContentPart::ToolUse` in assistant entries
/// - Tool results → separate tool-result-only user entries (Claude's convention)
/// - Token usage → `Message.usage` fields
/// - Session ID extraction from `ConversationView.id`
pub struct ClaudeProjector;

impl ConversationProjector for ClaudeProjector {
    type Output = Conversation;

    fn project(&self, view: &ConversationView) -> toolpath_convo::Result<Conversation> {
        project_conversation(view)
    }
}

fn project_conversation(view: &ConversationView) -> toolpath_convo::Result<Conversation> {
    let session_id = view.id.clone();
    let mut convo = Conversation::new(session_id.clone());
    convo.started_at = view.started_at;
    convo.last_activity = view.last_activity;

    for turn in &view.turns {
        match &turn.role {
            Role::User => {
                let entry = project_user_turn(turn, &session_id);
                convo.add_entry(entry);
            }
            Role::Assistant => {
                let entry = project_assistant_turn(turn, &session_id);
                convo.add_entry(entry);

                // Emit tool-result-only entries for tool invocations with results
                let results_entry = project_tool_results(turn, &session_id);
                if let Some(entry) = results_entry {
                    convo.add_entry(entry);
                }
            }
            _ => {
                // System / Other roles — emit as-is with best-effort mapping
                let entry = project_other_turn(turn, &session_id);
                convo.add_entry(entry);
            }
        }
    }

    Ok(convo)
}

fn project_user_turn(turn: &Turn, session_id: &str) -> ConversationEntry {
    ConversationEntry {
        uuid: turn.id.clone(),
        entry_type: "user".into(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.into()),
        parent_uuid: turn.parent_id.clone(),
        is_sidechain: false,
        message: Some(Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text(turn.text.clone())),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        }),
        cwd: turn.environment.as_ref().and_then(|e| e.working_dir.clone()),
        git_branch: turn.environment.as_ref().and_then(|e| e.vcs_branch.clone()),
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    }
}

fn project_assistant_turn(turn: &Turn, session_id: &str) -> ConversationEntry {
    let mut parts: Vec<ContentPart> = Vec::new();

    // Add thinking block if present
    if let Some(thinking) = &turn.thinking {
        parts.push(ContentPart::Thinking {
            thinking: thinking.clone(),
            signature: None,
        });
    }

    // Add text
    if !turn.text.is_empty() {
        parts.push(ContentPart::Text {
            text: turn.text.clone(),
        });
    }

    // Add tool use parts
    for tool_use in &turn.tool_uses {
        parts.push(ContentPart::ToolUse {
            id: tool_use.id.clone(),
            name: tool_use.name.clone(),
            input: tool_use.input.clone(),
        });
    }

    let content = if parts.len() == 1 && matches!(&parts[0], ContentPart::Text { .. }) {
        // Simple text-only message
        Some(MessageContent::Text(turn.text.clone()))
    } else if parts.is_empty() {
        None
    } else {
        Some(MessageContent::Parts(parts))
    };

    let usage = turn.token_usage.as_ref().map(|u| Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_creation_input_tokens: u.cache_write_tokens,
        cache_read_input_tokens: u.cache_read_tokens,
    });

    ConversationEntry {
        uuid: turn.id.clone(),
        entry_type: "assistant".into(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.into()),
        parent_uuid: turn.parent_id.clone(),
        is_sidechain: false,
        message: Some(Message {
            role: MessageRole::Assistant,
            content,
            model: turn.model.clone(),
            id: None,
            message_type: None,
            stop_reason: turn.stop_reason.clone(),
            stop_sequence: None,
            usage,
        }),
        cwd: None,
        git_branch: None,
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    }
}

fn project_tool_results(turn: &Turn, session_id: &str) -> Option<ConversationEntry> {
    let results: Vec<ContentPart> = turn
        .tool_uses
        .iter()
        .filter_map(|tu| {
            tu.result.as_ref().map(|r| ContentPart::ToolResult {
                tool_use_id: tu.id.clone(),
                content: ToolResultContent::Text(r.content.clone()),
                is_error: r.is_error,
            })
        })
        .collect();

    if results.is_empty() {
        return None;
    }

    // Generate a UUID for the tool-result entry
    let result_uuid = format!("{}-result", turn.id);

    Some(ConversationEntry {
        uuid: result_uuid,
        entry_type: "user".into(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.into()),
        parent_uuid: Some(turn.id.clone()),
        is_sidechain: false,
        message: Some(Message {
            role: MessageRole::User,
            content: Some(MessageContent::Parts(results)),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        }),
        cwd: None,
        git_branch: None,
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    })
}

fn project_other_turn(turn: &Turn, session_id: &str) -> ConversationEntry {
    let role = match &turn.role {
        Role::System => MessageRole::System,
        _ => MessageRole::User,
    };

    ConversationEntry {
        uuid: turn.id.clone(),
        entry_type: turn.role.to_string(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.into()),
        parent_uuid: turn.parent_id.clone(),
        is_sidechain: false,
        message: Some(Message {
            role,
            content: Some(MessageContent::Text(turn.text.clone())),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        }),
        cwd: None,
        git_branch: None,
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath_convo::EnvironmentSnapshot;

    fn simple_view() -> ConversationView {
        ConversationView {
            id: "session-123".into(),
            started_at: None,
            last_activity: None,
            turns: vec![
                Turn {
                    id: "u1".into(),
                    parent_id: None,
                    role: Role::User,
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    text: "Fix the bug".into(),
                    thinking: None,
                    tool_uses: vec![],
                    model: None,
                    stop_reason: None,
                    token_usage: None,
                    environment: Some(EnvironmentSnapshot {
                        working_dir: Some("/project".into()),
                        vcs_branch: Some("main".into()),
                        vcs_revision: None,
                    }),
                    delegations: vec![],
                    extra: HashMap::new(),
                },
                Turn {
                    id: "a1".into(),
                    parent_id: Some("u1".into()),
                    role: Role::Assistant,
                    timestamp: "2026-01-01T00:00:01Z".into(),
                    text: "I'll fix that.".into(),
                    thinking: Some("The bug is in auth".into()),
                    tool_uses: vec![ToolInvocation {
                        id: "t1".into(),
                        name: "Read".into(),
                        input: serde_json::json!({"file_path": "src/main.rs"}),
                        result: Some(ToolResult {
                            content: "fn main() {}".into(),
                            is_error: false,
                        }),
                        category: Some(ToolCategory::FileRead),
                    }],
                    model: Some("claude-opus-4-6".into()),
                    stop_reason: Some("tool_use".into()),
                    token_usage: Some(TokenUsage {
                        input_tokens: Some(100),
                        output_tokens: Some(50),
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                    }),
                    environment: None,
                    delegations: vec![],
                    extra: HashMap::new(),
                },
            ],
            total_usage: None,
            provider_id: Some("claude".into()),
            files_changed: vec![],
            session_ids: vec![],
        }
    }

    #[test]
    fn test_project_basic_conversation() {
        let projector = ClaudeProjector;
        let convo = projector.project(&simple_view()).unwrap();

        assert_eq!(convo.session_id, "session-123");
        // 2 turns + 1 tool-result entry = 3 entries
        assert_eq!(convo.entries.len(), 3);
    }

    #[test]
    fn test_project_user_turn() {
        let projector = ClaudeProjector;
        let convo = projector.project(&simple_view()).unwrap();

        let entry = &convo.entries[0];
        assert_eq!(entry.uuid, "u1");
        assert_eq!(entry.entry_type, "user");
        assert_eq!(entry.message.as_ref().unwrap().role, MessageRole::User);
        assert_eq!(entry.cwd.as_deref(), Some("/project"));
        assert_eq!(entry.git_branch.as_deref(), Some("main"));
    }

    #[test]
    fn test_project_assistant_turn_with_tools() {
        let projector = ClaudeProjector;
        let convo = projector.project(&simple_view()).unwrap();

        let entry = &convo.entries[1];
        assert_eq!(entry.uuid, "a1");
        assert_eq!(entry.entry_type, "assistant");
        assert_eq!(entry.parent_uuid.as_deref(), Some("u1"));

        let msg = entry.message.as_ref().unwrap();
        assert_eq!(msg.role, MessageRole::Assistant);
        assert_eq!(msg.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(msg.stop_reason.as_deref(), Some("tool_use"));

        // Should have thinking + text + tool_use parts
        match &msg.content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 3);
                assert!(matches!(&parts[0], ContentPart::Thinking { thinking, .. } if thinking == "The bug is in auth"));
                assert!(matches!(&parts[1], ContentPart::Text { text } if text == "I'll fix that."));
                assert!(matches!(&parts[2], ContentPart::ToolUse { name, .. } if name == "Read"));
            }
            other => panic!("Expected Parts, got {:?}", other),
        }

        // Usage
        let usage = msg.usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
    }

    #[test]
    fn test_project_tool_result_entry() {
        let projector = ClaudeProjector;
        let convo = projector.project(&simple_view()).unwrap();

        let entry = &convo.entries[2];
        assert_eq!(entry.entry_type, "user");
        assert_eq!(entry.parent_uuid.as_deref(), Some("a1"));

        match &entry.message.as_ref().unwrap().content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    ContentPart::ToolResult { tool_use_id, content, is_error } => {
                        assert_eq!(tool_use_id, "t1");
                        assert_eq!(content.text(), "fn main() {}");
                        assert!(!is_error);
                    }
                    other => panic!("Expected ToolResult, got {:?}", other),
                }
            }
            other => panic!("Expected Parts, got {:?}", other),
        }
    }

    #[test]
    fn test_project_simple_text_assistant() {
        let view = ConversationView {
            id: "s1".into(),
            started_at: None,
            last_activity: None,
            turns: vec![Turn {
                id: "a1".into(),
                parent_id: None,
                role: Role::Assistant,
                timestamp: "2026-01-01T00:00:00Z".into(),
                text: "Hello!".into(),
                thinking: None,
                tool_uses: vec![],
                model: None,
                stop_reason: Some("end_turn".into()),
                token_usage: None,
                environment: None,
                delegations: vec![],
                extra: HashMap::new(),
            }],
            total_usage: None,
            provider_id: None,
            files_changed: vec![],
            session_ids: vec![],
        };

        let projector = ClaudeProjector;
        let convo = projector.project(&view).unwrap();

        // Simple text assistant → MessageContent::Text (not Parts)
        let msg = convo.entries[0].message.as_ref().unwrap();
        assert!(matches!(&msg.content, Some(MessageContent::Text(t)) if t == "Hello!"));
        // No tool result entries
        assert_eq!(convo.entries.len(), 1);
    }

    #[test]
    fn test_project_no_tool_result_entry_when_no_results() {
        let view = ConversationView {
            id: "s1".into(),
            started_at: None,
            last_activity: None,
            turns: vec![Turn {
                id: "a1".into(),
                parent_id: None,
                role: Role::Assistant,
                timestamp: "2026-01-01T00:00:00Z".into(),
                text: "Reading...".into(),
                thinking: None,
                tool_uses: vec![ToolInvocation {
                    id: "t1".into(),
                    name: "Read".into(),
                    input: serde_json::json!({}),
                    result: None, // No result yet
                    category: Some(ToolCategory::FileRead),
                }],
                model: None,
                stop_reason: None,
                token_usage: None,
                environment: None,
                delegations: vec![],
                extra: HashMap::new(),
            }],
            total_usage: None,
            provider_id: None,
            files_changed: vec![],
            session_ids: vec![],
        };

        let projector = ClaudeProjector;
        let convo = projector.project(&view).unwrap();

        // Tool use with no result → no tool-result entry generated
        assert_eq!(convo.entries.len(), 1);
    }
}
```

- [ ] **Step 2: Wire up the module**

In `crates/toolpath-claude/src/lib.rs`, add after `pub mod provider;`:

```rust
pub mod project;
```

And add to the re-exports:

```rust
pub use project::ClaudeProjector;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p toolpath-claude project`
Expected: All 6 new projection tests pass.

- [ ] **Step 4: Run full workspace tests**

Run: `cargo test --workspace`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/toolpath-claude/src/project.rs crates/toolpath-claude/src/lib.rs
git commit -m "feat: add ClaudeProjector for ConversationView to Claude Conversation"
```

---

### Task 6: Round-trip integration test

**Files:**
- Create: `crates/toolpath-cli/tests/roundtrip.rs`

This test verifies the full pipeline: Claude JSONL → derive → Path → extract → ConversationView → project → Claude JSONL, asserting semantic equivalence.

- [ ] **Step 1: Write the round-trip test**

Create `crates/toolpath-cli/tests/roundtrip.rs`:

```rust
//! Round-trip test: Claude JSONL → derive → Path → extract → project → Claude JSONL
//!
//! Verifies that a Claude conversation can be derived into a toolpath Path,
//! extracted back to a ConversationView, and projected into Claude JSONL
//! with semantic equivalence.

use std::fs;
use tempfile::TempDir;

#[test]
fn test_claude_roundtrip() {
    // 1. Create a Claude session with realistic content
    let temp = TempDir::new().unwrap();
    let claude_dir = temp.path().join(".claude");
    let project_dir = claude_dir.join("projects/-test-project");
    fs::create_dir_all(&project_dir).unwrap();

    let entries = [
        r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-rt","cwd":"/test/project","gitBranch":"main","message":{"role":"user","content":"Fix the authentication bug in login.rs"}}"#,
        r#"{"uuid":"uuid-2","type":"assistant","parentUuid":"uuid-1","timestamp":"2024-01-01T00:00:01Z","sessionId":"session-rt","message":{"role":"assistant","content":[{"type":"thinking","thinking":"The bug is in the token validation"},{"type":"text","text":"I'll fix that. Let me read the file first."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"src/login.rs"}}],"model":"claude-opus-4-6","stop_reason":"tool_use","usage":{"input_tokens":100,"output_tokens":50}}}"#,
        r#"{"uuid":"uuid-3","type":"user","parentUuid":"uuid-2","timestamp":"2024-01-01T00:00:02Z","sessionId":"session-rt","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"fn login() { validate_token(); }","is_error":false}]}}"#,
        r#"{"uuid":"uuid-4","type":"assistant","parentUuid":"uuid-3","timestamp":"2024-01-01T00:00:03Z","sessionId":"session-rt","message":{"role":"assistant","content":[{"type":"text","text":"I see the issue. Let me fix it."},{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"src/login.rs","old_string":"validate_token()","new_string":"validate_token_v2()"}}],"model":"claude-opus-4-6","stop_reason":"tool_use","usage":{"input_tokens":200,"output_tokens":100}}}"#,
        r#"{"uuid":"uuid-5","type":"user","parentUuid":"uuid-4","timestamp":"2024-01-01T00:00:04Z","sessionId":"session-rt","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"File written successfully","is_error":false}]}}"#,
        r#"{"uuid":"uuid-6","type":"assistant","parentUuid":"uuid-5","timestamp":"2024-01-01T00:00:05Z","sessionId":"session-rt","message":{"role":"assistant","content":"Done! The authentication bug is fixed.","model":"claude-opus-4-6","stop_reason":"end_turn","usage":{"input_tokens":150,"output_tokens":30}}}"#,
        r#"{"uuid":"uuid-7","type":"user","parentUuid":"uuid-6","timestamp":"2024-01-01T00:00:06Z","sessionId":"session-rt","message":{"role":"user","content":"Thanks!"}}"#,
    ];
    fs::write(project_dir.join("session-rt.jsonl"), entries.join("\n")).unwrap();

    // 2. Derive: Claude JSONL → Conversation → Path
    let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
    let manager = toolpath_claude::ClaudeConvo::with_resolver(resolver);
    let original_convo = manager
        .read_conversation("/test/project", "session-rt")
        .unwrap();

    let config = toolpath_claude::derive::DeriveConfig {
        project_path: Some("/test/project".into()),
        include_thinking: true,
    };
    let path = toolpath_claude::derive::derive_path(&original_convo, &config);

    // 3. Extract: Path → ConversationView
    let view = toolpath_convo::extract_conversation(&path);

    // 4. Project: ConversationView → Conversation
    use toolpath_convo::ConversationProjector;
    let projector = toolpath_claude::ClaudeProjector;
    let projected_convo = projector.project(&view).unwrap();

    // 5. Assert semantic equivalence

    // Same number of user-visible turns (tool-result-only entries are
    // separate entries but not separate turns)
    let original_view = toolpath_claude::provider::to_view(&original_convo);
    assert_eq!(view.turns.len(), original_view.turns.len(),
        "Turn count mismatch: extracted {} vs original {}",
        view.turns.len(), original_view.turns.len());

    // Compare turns pairwise
    for (extracted, original) in view.turns.iter().zip(original_view.turns.iter()) {
        assert_eq!(extracted.role, original.role, "Role mismatch for turn {}", extracted.id);
        assert_eq!(extracted.text, original.text, "Text mismatch for turn {}", extracted.id);

        // Tool invocations
        assert_eq!(
            extracted.tool_uses.len(),
            original.tool_uses.len(),
            "Tool use count mismatch for turn {}",
            extracted.id
        );
        for (ext_tu, orig_tu) in extracted.tool_uses.iter().zip(original.tool_uses.iter()) {
            assert_eq!(ext_tu.name, orig_tu.name, "Tool name mismatch");
            // Results should match (if present)
            match (&ext_tu.result, &orig_tu.result) {
                (Some(ext_r), Some(orig_r)) => {
                    assert_eq!(ext_r.content, orig_r.content, "Tool result content mismatch");
                    assert_eq!(ext_r.is_error, orig_r.is_error, "Tool result error flag mismatch");
                }
                (None, None) => {}
                _ => panic!("Tool result presence mismatch for tool {}", ext_tu.name),
            }
        }
    }

    // Token usage preserved
    if let (Some(ext_total), Some(orig_total)) = (&view.total_usage, &original_view.total_usage) {
        assert_eq!(ext_total.input_tokens, orig_total.input_tokens, "Input tokens mismatch");
        assert_eq!(ext_total.output_tokens, orig_total.output_tokens, "Output tokens mismatch");
    }

    // Projected conversation has correct structure
    assert_eq!(projected_convo.session_id, "session-rt");
    // Should have entries for all turns + tool result entries
    assert!(projected_convo.entries.len() >= view.turns.len());
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p toolpath-cli --test roundtrip`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/toolpath-cli/tests/roundtrip.rs
git commit -m "test: add round-trip integration test for conversation projection"
```

---

### Task 7: CLI `project` subcommand

**Files:**
- Create: `crates/toolpath-cli/src/cmd_project.rs`
- Modify: `crates/toolpath-cli/src/main.rs` (add module, subcommand, dispatch)

- [ ] **Step 1: Write the command module**

Create `crates/toolpath-cli/src/cmd_project.rs`:

```rust
use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum ProjectTarget {
    /// Project a toolpath document into Claude JSONL format
    Claude {
        /// Input toolpath document (JSON)
        #[arg(short, long)]
        input: PathBuf,

        /// Output file (JSONL). Prints to stdout if omitted.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

pub fn run(target: ProjectTarget) -> Result<()> {
    match target {
        ProjectTarget::Claude { input, output } => run_claude(input, output),
    }
}

fn run_claude(input: PathBuf, output: Option<PathBuf>) -> Result<()> {
    // Read the toolpath document
    let json = std::fs::read_to_string(&input)?;
    let doc = toolpath::v1::Document::from_json(&json)?;

    let path = match doc {
        toolpath::v1::Document::Path(p) => p,
        _ => anyhow::bail!("Expected a Path document, got a different document type"),
    };

    // Extract conversation from toolpath Path
    let view = toolpath_convo::extract_conversation(&path);

    // Project into Claude format
    use toolpath_convo::ConversationProjector;
    let projector = toolpath_claude::project::ClaudeProjector;
    let convo = projector.project(&view)?;

    // Serialize entries as JSONL
    let mut lines = Vec::new();
    for entry in &convo.entries {
        lines.push(serde_json::to_string(entry)?);
    }
    let jsonl = lines.join("\n");

    match output {
        Some(path) => {
            std::fs::write(&path, &jsonl)?;
            eprintln!("Wrote {} entries to {}", convo.entries.len(), path.display());
        }
        None => {
            println!("{}", jsonl);
        }
    }

    Ok(())
}
```

- [ ] **Step 2: Wire into main.rs**

In `crates/toolpath-cli/src/main.rs`:

Add the module declaration:
```rust
mod cmd_project;
```

Add the subcommand to `Commands`:
```rust
/// Project a toolpath document into a provider's conversation format
Project {
    #[command(subcommand)]
    target: cmd_project::ProjectTarget,
},
```

Add the dispatch in `main()`:
```rust
Commands::Project { target } => cmd_project::run(target),
```

- [ ] **Step 3: Test manually**

Run:
```bash
# Derive a toolpath doc first
cargo run -p toolpath-cli -- derive claude --project /some/project --session some-session --pretty > /tmp/test-doc.json

# Project it back
cargo run -p toolpath-cli -- project claude --input /tmp/test-doc.json
```

Expected: JSONL output to stdout with valid Claude conversation entries.

- [ ] **Step 4: Add a CLI integration test**

Add to `crates/toolpath-cli/tests/roundtrip.rs` (or a new test file):

```rust
#[test]
fn test_cli_project_command() {
    // Create a minimal toolpath Path document
    use toolpath::v1::*;
    use std::collections::HashMap;

    let mut changes = HashMap::new();
    changes.insert(
        "agent://claude/test-session".to_string(),
        ArtifactChange {
            raw: None,
            structural: Some(StructuralChange {
                change_type: "conversation.append".to_string(),
                extra: {
                    let mut e = HashMap::new();
                    e.insert("role".to_string(), serde_json::json!("user"));
                    e.insert("text".to_string(), serde_json::json!("Hello"));
                    e
                },
            }),
        },
    );

    let path = Path {
        path: PathIdentity {
            id: "test-path".into(),
            base: None,
            head: "step-1".into(),
        },
        steps: vec![Step {
            step: StepIdentity {
                id: "step-1".into(),
                parents: vec![],
                actor: "human:user".into(),
                timestamp: "2026-01-01T00:00:00Z".into(),
            },
            change: changes,
            meta: None,
        }],
        meta: None,
    };

    let doc = Document::Path(path);
    let input_path = std::env::temp_dir().join("test-project-input.json");
    fs::write(&input_path, doc.to_json_pretty().unwrap()).unwrap();

    let output_path = std::env::temp_dir().join("test-project-output.jsonl");

    // Run the CLI command
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_path"))
        .args(["project", "claude", "--input", input_path.to_str().unwrap(), "--output", output_path.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());

    // Verify output
    let output = fs::read_to_string(&output_path).unwrap();
    assert!(!output.is_empty());
    // Parse each line as JSON
    for line in output.lines() {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(entry.get("uuid").is_some());
    }

    // Cleanup
    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p toolpath-cli`
Expected: All tests pass including the new CLI test.

- [ ] **Step 6: Commit**

```bash
git add crates/toolpath-cli/src/cmd_project.rs crates/toolpath-cli/src/main.rs crates/toolpath-cli/tests/roundtrip.rs
git commit -m "feat: add 'path project claude' CLI command"
```

---

### Task 8: Version bumps, docs, and changelog

**Files:**
- Modify: `crates/toolpath-convo/Cargo.toml` (version bump)
- Modify: `crates/toolpath-claude/Cargo.toml` (version bump)
- Modify: `crates/toolpath-cli/Cargo.toml` (version bump)
- Modify: `Cargo.toml` (workspace dep versions)
- Modify: `CHANGELOG.md`
- Modify: `site/_data/crates.json`

Per CLAUDE.md versioning checklist: minor bumps for `toolpath-convo` (new dependency + new public API) and `toolpath-claude` (breaking derive output + new public API). Patch bump for `toolpath-cli` (new command, no breaking changes).

- [ ] **Step 1: Bump `toolpath-convo` to 0.6.0**

In `crates/toolpath-convo/Cargo.toml`:
```toml
version = "0.6.0"
```

In `Cargo.toml` (workspace root):
```toml
toolpath-convo = { version = "0.6.0", path = "crates/toolpath-convo" }
```

- [ ] **Step 2: Bump `toolpath-claude` to 0.7.0**

In `crates/toolpath-claude/Cargo.toml`:
```toml
version = "0.7.0"
```

In `Cargo.toml` (workspace root):
```toml
toolpath-claude = { version = "0.7.0", path = "crates/toolpath-claude", default-features = false }
```

- [ ] **Step 3: Bump `toolpath-cli` to 0.3.1**

In `crates/toolpath-cli/Cargo.toml`:
```toml
version = "0.3.1"
```

- [ ] **Step 4: Update `site/_data/crates.json`**

Update the version fields for `toolpath-convo`, `toolpath-claude`, and `toolpath-cli`.

- [ ] **Step 5: Update `CHANGELOG.md`**

Add a new section at the top:

```markdown
## toolpath-convo 0.6.0, toolpath-claude 0.7.0, toolpath-cli 0.3.1

### toolpath-convo

- **Added:** `ConversationProjector` trait and `AnyProjector` type-erasing wrapper for projecting `ConversationView` into provider-native formats.
- **Added:** `extract_conversation()` function to reconstruct a `ConversationView` from a toolpath `Path` following the conversation sub-protocol.
- **Added:** Dependency on `toolpath` core crate.
- **Added:** Conversation sub-protocol documentation: `conversation.init`, `conversation.append`, `tool.invoke` structural change types, `agent://` artifact URN scheme, actor patterns.

### toolpath-claude

- **Breaking:** Conversation artifact key changed from `claude://<session-id>` to `agent://claude/<session-id>`.
- **Breaking:** Tool invocations are now separate steps with `agent:claude-code/tool:<Name>` actors (previously inlined as name lists).
- **Breaking:** Step IDs use full UUIDs (previously truncated to 8-char prefixes).
- **Added:** `ClaudeProjector` implementing `ConversationProjector` for projecting `ConversationView` back to Claude `Conversation`.
- **Added:** Enriched derive output: full untruncated text, thinking blocks, token usage, stop_reason, model per turn.

### toolpath-cli

- **Added:** `path project claude` command for projecting toolpath documents into Claude JSONL format.
```

- [ ] **Step 6: Verify everything builds and tests pass**

Run: `cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings`
Expected: All pass.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/toolpath-convo/Cargo.toml crates/toolpath-claude/Cargo.toml crates/toolpath-cli/Cargo.toml CHANGELOG.md site/_data/crates.json
git commit -m "chore: version bumps for conversation projection

toolpath-convo 0.5.0 → 0.6.0 (new API + new dependency)
toolpath-claude 0.6.2 → 0.7.0 (breaking derive changes + new API)
toolpath-cli 0.3.0 → 0.3.1 (new project command)"
```

---

### Task 9: Final verification

- [ ] **Step 1: Clean build**

Run: `cargo clean && cargo build --workspace`
Expected: Compiles with no warnings.

- [ ] **Step 2: Full test suite**

Run: `cargo test --workspace`
Expected: All tests pass.

- [ ] **Step 3: Clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings.

- [ ] **Step 4: Validate examples still work**

Run: `for f in examples/*.json; do cargo run -p toolpath-cli -- validate --input "$f"; done`
Expected: All validate successfully.
