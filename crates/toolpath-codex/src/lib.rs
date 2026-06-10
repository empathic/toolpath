#![doc = include_str!("../README.md")]

pub mod error;
pub mod io;
pub mod paths;
pub mod project;
pub mod reader;
pub mod types;

pub use error::{ConvoError, Result};
pub use io::ConvoIO;
pub use paths::PathResolver;
pub use reader::RolloutReader;
pub use types::{
    BaseInstructions, CompactedItem, ContentPart, CustomToolCall, CustomToolCallOutput, EventMsg,
    ExecCommandEnd, FunctionCall, FunctionCallOutput, GitInfo, Message, PatchApplyEnd, PatchChange,
    Reasoning, ResponseItem, RolloutItem, RolloutLine, SandboxPolicy, Session, SessionMeta,
    SessionMetadata, TokenCountEvent, TokenCountInfo, TokenUsage, TurnContext,
};

pub mod provider;
pub use provider::{CodexConvo, to_turn, to_view, tool_category};

pub mod derive;
