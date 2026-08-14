#![doc = include_str!("../README.md")]

pub mod derive;
pub mod error;
pub mod io;
pub mod paths;
pub mod project;
pub mod provider;
pub mod reader;
pub mod types;

pub use derive::{derive_graph, derive_path, derive_project};
pub use error::{PiError, Result};
pub use paths::PathResolver;
pub use provider::session_to_view;
pub use reader::{PiSession, SessionMeta};
pub use toolpath_convo::DeriveConfig;
pub use types::{
    AgentMessage, ContentBlock, CostBreakdown, Entry, EntryBase, SessionHeader, Usage,
};

use toolpath_convo::ConversationView;

/// High-level interface for reading Pi sessions.
#[derive(Debug, Clone)]
pub struct PiConvo {
    resolver: PathResolver,
}

impl PiConvo {
    /// Build a manager for `<home>/.pi/agent/sessions/`. The caller
    /// supplies the home directory.
    pub fn new(home: impl AsRef<std::path::Path>) -> Self {
        Self {
            resolver: PathResolver::new(home),
        }
    }

    /// Build a manager with a custom resolver (useful for tests).
    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self { resolver }
    }

    /// Access the underlying resolver.
    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    /// Whether the Pi sessions directory exists on disk.
    pub fn exists(&self) -> bool {
        self.resolver.sessions_dir().exists()
    }

    /// List known project paths (decoded cwd values).
    pub fn list_projects(&self) -> Result<Vec<String>> {
        io::list_projects(&self.resolver)
    }

    /// List session metadata for a project, newest first.
    pub fn list_sessions(&self, project: &str) -> Result<Vec<SessionMeta>> {
        io::list_sessions(&self.resolver, project)
    }

    /// Read a specific session by ID.
    pub fn read_session(&self, project: &str, session_id: &str) -> Result<PiSession> {
        reader::read_session(&self.resolver, project, session_id)
    }

    /// Read the most recently active session for a project, if any.
    pub fn most_recent_session(&self, project: &str) -> Result<Option<PiSession>> {
        let mut metas = self.list_sessions(project)?;
        if let Some(meta) = metas.drain(..).next() {
            let session = self.read_session(project, &meta.id)?;
            Ok(Some(session))
        } else {
            Ok(None)
        }
    }

    /// Convert a Pi session into a provider-agnostic `ConversationView`.
    pub fn to_view(&self, session: &PiSession) -> ConversationView {
        provider::session_to_view(session)
    }

    /// Read all sessions for a project.
    pub fn read_all_sessions(&self, project: &str) -> Result<Vec<PiSession>> {
        let metas = self.list_sessions(project)?;
        let mut sessions = Vec::new();
        for meta in metas {
            match self.read_session(project, &meta.id) {
                Ok(s) => sessions.push(s),
                Err(e) => {
                    eprintln!("Warning: failed to read Pi session {}: {}", meta.id, e);
                }
            }
        }
        Ok(sessions)
    }
}
