//! [`ConversationProjector`] trait and [`AnyProjector`] type-erasing wrapper.
//!
//! A projector is the "serialize" half of a serde-like pattern for conversation
//! portability — the inverse of [`ConversationProvider`](crate::ConversationProvider).
//! Where a provider reads provider-specific data and produces a [`ConversationView`],
//! a projector consumes a [`ConversationView`] and produces some output type.

use crate::{ConversationView, ConvoError, Result};
use std::any::Any;

// ── Trait ─────────────────────────────────────────────────────────────

/// Convert a [`ConversationView`] into an output type.
///
/// Implement this trait to serialize, render, or transform a conversation
/// into any target representation (e.g. Toolpath `Path`, Markdown, JSON-LD).
///
/// # Example
///
/// ```
/// use toolpath_convo::{ConversationView, ConversationProjector, Result};
///
/// struct TurnCounter;
///
/// impl ConversationProjector for TurnCounter {
///     type Output = usize;
///
///     fn project(&self, view: &ConversationView) -> Result<usize> {
///         Ok(view.turns.len())
///     }
/// }
/// ```
pub trait ConversationProjector {
    /// The type produced by projecting a [`ConversationView`].
    type Output;

    /// Project `view` into `Self::Output`.
    fn project(&self, view: &ConversationView) -> Result<Self::Output>;
}

// ── Internal erased trait ─────────────────────────────────────────────

trait ErasedProjector: Send + Sync {
    fn project_erased(&self, view: &ConversationView) -> Result<Box<dyn Any>>;
}

struct ErasedWrapper<P>(P);

impl<P> ErasedProjector for ErasedWrapper<P>
where
    P: ConversationProjector + Send + Sync,
    P::Output: 'static,
{
    fn project_erased(&self, view: &ConversationView) -> Result<Box<dyn Any>> {
        self.0.project(view).map(|out| Box::new(out) as Box<dyn Any>)
    }
}

// ── AnyProjector ─────────────────────────────────────────────────────

/// A type-erased [`ConversationProjector`] for dynamic dispatch.
///
/// Wraps any concrete projector so it can be stored in trait objects,
/// passed across module boundaries, or held in collections alongside
/// projectors with different output types.
///
/// # Example
///
/// ```
/// use toolpath_convo::{ConversationView, ConversationProjector, Result};
/// use toolpath_convo::project::AnyProjector;
/// use std::collections::HashMap;
///
/// struct TurnCounter;
/// impl ConversationProjector for TurnCounter {
///     type Output = usize;
///     fn project(&self, view: &ConversationView) -> Result<usize> {
///         Ok(view.turns.len())
///     }
/// }
///
/// let view = ConversationView {
///     id: "s1".into(),
///     started_at: None,
///     last_activity: None,
///     turns: vec![],
///     total_usage: None,
///     provider_id: None,
///     files_changed: vec![],
///     session_ids: vec![],
/// };
///
/// let projector = AnyProjector::new(TurnCounter);
/// let count = projector.project_as::<usize>(&view).unwrap();
/// assert_eq!(count, 0);
/// ```
pub struct AnyProjector {
    inner: Box<dyn ErasedProjector>,
}

impl AnyProjector {
    /// Wrap a concrete [`ConversationProjector`] for type-erased dispatch.
    pub fn new<P>(projector: P) -> Self
    where
        P: ConversationProjector + Send + Sync + 'static,
        P::Output: 'static,
    {
        Self {
            inner: Box::new(ErasedWrapper(projector)),
        }
    }

    /// Project `view` and return the result as `Box<dyn Any>`.
    ///
    /// Use [`project_as`](AnyProjector::project_as) when the concrete output
    /// type is known at the call site.
    pub fn project(&self, view: &ConversationView) -> Result<Box<dyn Any>> {
        self.inner.project_erased(view)
    }

    /// Project `view` and downcast the result to `T`.
    ///
    /// Returns `Err(ConvoError::Provider(...))` if the downcast fails, which
    /// means `T` does not match the projector's actual `Output` type.
    pub fn project_as<T: 'static>(&self, view: &ConversationView) -> Result<T> {
        let boxed = self.project(view)?;
        boxed.downcast::<T>().map(|b| *b).map_err(|_| {
            ConvoError::Provider(format!(
                "AnyProjector::project_as: output is not of type {}",
                std::any::type_name::<T>()
            ))
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Role, TokenUsage, ToolInvocation, ToolResult, Turn};
    use std::collections::HashMap;

    // ── helpers ──────────────────────────────────────────────────────

    fn empty_view() -> ConversationView {
        ConversationView {
            id: "sess-1".into(),
            started_at: None,
            last_activity: None,
            turns: vec![],
            total_usage: None,
            provider_id: None,
            files_changed: vec![],
            session_ids: vec![],
        }
    }

    fn make_turn(id: &str, role: Role, text: &str) -> Turn {
        Turn {
            id: id.into(),
            parent_id: None,
            role,
            timestamp: "2026-01-01T00:00:00Z".into(),
            text: text.into(),
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

    fn view_with_turns() -> ConversationView {
        ConversationView {
            id: "sess-2".into(),
            started_at: None,
            last_activity: None,
            turns: vec![
                make_turn("t1", Role::User, "hello"),
                make_turn("t2", Role::Assistant, "world"),
                make_turn("t3", Role::User, "done"),
            ],
            total_usage: None,
            provider_id: Some("test-provider".into()),
            files_changed: vec![],
            session_ids: vec![],
        }
    }

    // ── concrete projectors used in tests ────────────────────────────

    struct TurnCounter;
    impl ConversationProjector for TurnCounter {
        type Output = usize;
        fn project(&self, view: &ConversationView) -> Result<usize> {
            Ok(view.turns.len())
        }
    }

    struct ProviderIdExtractor;
    impl ConversationProjector for ProviderIdExtractor {
        type Output = Option<String>;
        fn project(&self, view: &ConversationView) -> Result<Option<String>> {
            Ok(view.provider_id.clone())
        }
    }

    struct AlwaysFails;
    impl ConversationProjector for AlwaysFails {
        type Output = String;
        fn project(&self, _view: &ConversationView) -> Result<String> {
            Err(ConvoError::Provider("intentional failure".into()))
        }
    }

    // ── Test 1: concrete projector usage ────────────────────────────

    #[test]
    fn test_concrete_projector_empty() {
        let proj = TurnCounter;
        let count = proj.project(&empty_view()).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_concrete_projector_with_turns() {
        let proj = TurnCounter;
        let count = proj.project(&view_with_turns()).unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_concrete_projector_option_output() {
        let proj = ProviderIdExtractor;
        let id = proj.project(&view_with_turns()).unwrap();
        assert_eq!(id.as_deref(), Some("test-provider"));

        let id_none = proj.project(&empty_view()).unwrap();
        assert!(id_none.is_none());
    }

    // ── Test 2: AnyProjector::project (returns Box<dyn Any>) ────────

    #[test]
    fn test_any_projector_project_returns_box_any() {
        let any = AnyProjector::new(TurnCounter);
        let boxed = any.project(&view_with_turns()).unwrap();
        // We can downcast it manually
        let count = boxed.downcast::<usize>().unwrap();
        assert_eq!(*count, 3);
    }

    #[test]
    fn test_any_projector_project_empty() {
        let any = AnyProjector::new(TurnCounter);
        let boxed = any.project(&empty_view()).unwrap();
        let count = boxed.downcast::<usize>().unwrap();
        assert_eq!(*count, 0);
    }

    // ── Test 3: AnyProjector::project_as (successful downcast) ──────

    #[test]
    fn test_any_projector_project_as_success() {
        let any = AnyProjector::new(TurnCounter);
        let count: usize = any.project_as(&view_with_turns()).unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_any_projector_project_as_option_output() {
        let any = AnyProjector::new(ProviderIdExtractor);
        let id: Option<String> = any.project_as(&view_with_turns()).unwrap();
        assert_eq!(id.as_deref(), Some("test-provider"));
    }

    // ── Test 4: AnyProjector::project_as with wrong type (error) ────

    #[test]
    fn test_any_projector_project_as_wrong_type() {
        let any = AnyProjector::new(TurnCounter); // Output = usize
        let result: Result<String> = any.project_as(&view_with_turns()); // ask for String
        assert!(result.is_err());
        let err = result.unwrap_err();
        // Should be a Provider error describing the type mismatch
        assert!(matches!(err, ConvoError::Provider(_)));
        let msg = err.to_string();
        assert!(msg.contains("AnyProjector::project_as"), "msg was: {}", msg);
    }

    #[test]
    fn test_any_projector_project_as_wrong_type_bool() {
        let any = AnyProjector::new(ProviderIdExtractor); // Output = Option<String>
        let result: Result<bool> = any.project_as(&view_with_turns());
        assert!(result.is_err());
    }

    // ── Test 5: AnyProjector with actual turn data ───────────────────

    struct TextCollector;
    impl ConversationProjector for TextCollector {
        type Output = Vec<String>;
        fn project(&self, view: &ConversationView) -> Result<Vec<String>> {
            Ok(view.turns.iter().map(|t| t.text.clone()).collect())
        }
    }

    struct ToolNameCollector;
    impl ConversationProjector for ToolNameCollector {
        type Output = Vec<String>;
        fn project(&self, view: &ConversationView) -> Result<Vec<String>> {
            Ok(view
                .turns
                .iter()
                .flat_map(|t| t.tool_uses.iter().map(|u| u.name.clone()))
                .collect())
        }
    }

    #[test]
    fn test_any_projector_with_turn_text_data() {
        let any = AnyProjector::new(TextCollector);
        let texts: Vec<String> = any.project_as(&view_with_turns()).unwrap();
        assert_eq!(texts, vec!["hello", "world", "done"]);
    }

    #[test]
    fn test_any_projector_with_tool_use_data() {
        let view = ConversationView {
            id: "s3".into(),
            started_at: None,
            last_activity: None,
            turns: vec![Turn {
                id: "t1".into(),
                parent_id: None,
                role: Role::Assistant,
                timestamp: "2026-01-01T00:00:00Z".into(),
                text: "reading file".into(),
                thinking: None,
                tool_uses: vec![
                    ToolInvocation {
                        id: "u1".into(),
                        name: "Read".into(),
                        input: serde_json::json!({"file": "src/main.rs"}),
                        result: Some(ToolResult {
                            content: "fn main() {}".into(),
                            is_error: false,
                        }),
                        category: None,
                    },
                    ToolInvocation {
                        id: "u2".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "cargo test"}),
                        result: None,
                        category: None,
                    },
                ],
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

        let any = AnyProjector::new(ToolNameCollector);
        let names: Vec<String> = any.project_as(&view).unwrap();
        assert_eq!(names, vec!["Read", "Bash"]);
    }

    #[test]
    fn test_any_projector_propagates_projector_error() {
        let any = AnyProjector::new(AlwaysFails);
        let result: Result<String> = any.project_as(&empty_view());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConvoError::Provider(_)));
    }

    #[test]
    fn test_any_projector_with_token_usage() {
        struct TotalInputTokens;
        impl ConversationProjector for TotalInputTokens {
            type Output = u32;
            fn project(&self, view: &ConversationView) -> Result<u32> {
                Ok(view
                    .turns
                    .iter()
                    .filter_map(|t| t.token_usage.as_ref())
                    .filter_map(|u| u.input_tokens)
                    .sum())
            }
        }

        let view = ConversationView {
            id: "s4".into(),
            started_at: None,
            last_activity: None,
            turns: vec![
                Turn {
                    id: "t1".into(),
                    parent_id: None,
                    role: Role::Assistant,
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    text: "turn 1".into(),
                    thinking: None,
                    tool_uses: vec![],
                    model: None,
                    stop_reason: None,
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
                Turn {
                    id: "t2".into(),
                    parent_id: Some("t1".into()),
                    role: Role::Assistant,
                    timestamp: "2026-01-01T00:00:01Z".into(),
                    text: "turn 2".into(),
                    thinking: None,
                    tool_uses: vec![],
                    model: None,
                    stop_reason: None,
                    token_usage: Some(TokenUsage {
                        input_tokens: Some(200),
                        output_tokens: Some(75),
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                    }),
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

        let any = AnyProjector::new(TotalInputTokens);
        let total: u32 = any.project_as(&view).unwrap();
        assert_eq!(total, 300);
    }
}
