#![doc = include_str!("../README.md")]

pub mod error;
pub mod io;
pub mod paths;
pub mod reader;
pub mod types;

pub use error::{ConvoError, Result};
pub use io::{CliFetcher, CliWriter, ConvoIO, DirFetcher, ThreadFetcher, ThreadWriter};
pub use paths::PathResolver;
pub use reader::ExportReader;
pub use types::{Block, KnownBlock, Message, MessageUsage, Session, SessionMetadata, ThreadExport};

pub mod provider;
pub use provider::{AmpConvo, PRODUCER_NAME, PROVIDER_ID, native_name, to_view, tool_category};

pub mod project;
pub use project::{AmpProjector, rehydration_prompt};

pub mod derive;
