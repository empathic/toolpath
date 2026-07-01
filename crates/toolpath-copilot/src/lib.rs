#![doc = include_str!("../README.md")]

pub mod error;
pub mod io;
pub mod paths;
pub mod reader;
pub mod types;

pub use error::{ConvoError, Result};
pub use io::ConvoIO;
pub use paths::PathResolver;
pub use reader::EventReader;
pub use types::{
    CopilotEvent, EventLine, MessageEvent, Session, SessionMetadata, SessionShutdown, SessionStart,
    Subagent, ToolExecution, Workspace, parse_workspace,
};

pub mod provider;
pub use provider::{CopilotConvo, PRODUCER_NAME, PROVIDER_ID, to_view, tool_category};

pub mod derive;
