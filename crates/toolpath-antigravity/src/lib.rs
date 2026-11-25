#![doc = include_str!("../README.md")]

pub mod error;
pub mod paths;
pub mod proto;
pub mod provider;
pub mod reader;
pub mod types;

pub use error::{ConvoError, Result};
pub use paths::PathResolver;
pub use provider::{to_view, tool_category};
pub use reader::ConvoIO;
pub use types::{Conversation, ConversationSummary, ModelUsage, Step, ToolCallRaw};

/// High-level entry point for reading Antigravity CLI (`agy`)
/// conversations.
///
/// Derive-only, by design: `agy`'s on-disk schema is reverse-engineered
/// from a closed binary (see `docs/agents/formats/antigravity.md`), its
/// enums are closed, and every write-back path needs a real model turn
/// to validate — there is no safe way to confirm a projected session on
/// this machine (see the PR description / task report for what was
/// tried). A `project`/writer module can follow once that's unblocked.
///
/// # Example
///
/// ```rust,no_run
/// use toolpath_antigravity::AntigravityConvo;
///
/// let manager = AntigravityConvo::new();
/// let ids = manager.list_conversations()?;
/// if let Some(id) = ids.first() {
///     let convo = manager.read_conversation(id)?;
///     println!("{} steps", convo.steps.len());
/// }
/// # Ok::<(), toolpath_antigravity::ConvoError>(())
/// ```
#[derive(Debug, Clone)]
pub struct AntigravityConvo {
    io: ConvoIO,
}

impl Default for AntigravityConvo {
    fn default() -> Self {
        Self::new()
    }
}

impl AntigravityConvo {
    pub fn new() -> Self {
        Self { io: ConvoIO::new() }
    }

    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self {
            io: ConvoIO::with_resolver(resolver),
        }
    }

    pub fn io(&self) -> &ConvoIO {
        &self.io
    }

    pub fn resolver(&self) -> &PathResolver {
        self.io.resolver()
    }

    pub fn exists(&self) -> bool {
        self.io.exists()
    }

    /// List every conversation id under `conversations/` (flat — no
    /// per-project slot).
    pub fn list_conversations(&self) -> Result<Vec<String>> {
        self.io.list_conversations()
    }

    pub fn list_summaries(&self) -> Result<Vec<ConversationSummary>> {
        self.io.list_summaries()
    }

    pub fn read_conversation(&self, id: &str) -> Result<Conversation> {
        self.io.read_conversation(id)
    }

    /// The conversation id `agy -c`/`--continue` would resume for `cwd`,
    /// via `cache/last_conversations.json`.
    pub fn last_conversation_for_cwd(&self, cwd: &str) -> Result<Option<String>> {
        self.io.last_conversation_for_cwd(cwd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_constructs() {
        let _m = AntigravityConvo::default();
    }

    #[test]
    fn list_conversations_empty_without_a_store() {
        let resolver = PathResolver::new().with_gemini_dir("/never/exists/.gemini");
        let manager = AntigravityConvo::with_resolver(resolver);
        assert!(!manager.exists());
        assert_eq!(manager.list_conversations().unwrap(), Vec::<String>::new());
    }
}
