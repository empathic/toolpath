#![doc = include_str!("../README.md")]
#![recursion_limit = "256"]

pub mod agent_store;
pub mod derive;
pub mod error;
pub mod io;
pub mod paths;
pub mod project;
pub mod proto;
pub mod provider;
pub mod reader;
pub mod types;

pub use agent_store::{AgentStore, AgentStoreProjector};
pub use derive::{DeriveConfig, derive_path, derive_project};
pub use error::{CursorError, Result};
pub use io::{ComposerListing, CursorIO};
pub use paths::{EnsuredWorkspaceId, PathResolver, slug_from_abs_path};
pub use project::CursorProjector;
pub use provider::{CursorConvo, PROVIDER_ID, session_to_view, tool_category};
pub use reader::DbReader;
pub use types::{
    BUBBLE_TYPE_ASSISTANT, BUBBLE_TYPE_USER, Bubble, BubbleGrouping, BubbleHeader, BubbleThinking,
    CAPABILITY_THINKING, CAPABILITY_TOOL, ComposerData, ComposerHead, ComposerHeaders,
    CursorSession, CursorSessionMetadata, ModelConfig, ModelInfo, SelectedModel, TOOL_EDIT_FILE_V2,
    TOOL_GLOB_FILE_SEARCH, TOOL_READ_FILE_V2, TOOL_RUN_TERMINAL_COMMAND_V2, ThinkingBlock,
    TokenCount, ToolFormerData, WorkspaceIdentifier, WorkspaceUri,
};
