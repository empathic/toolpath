#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

pub mod derive;
pub mod error;
pub mod io;
pub mod paths;
pub mod project;
pub mod provider;
pub mod reader;
pub mod types;

pub use derive::{derive_graph, derive_path, derive_project};
pub use error::{OpenClawError, Result};
pub use io::SessionMeta;
pub use paths::{DEFAULT_AGENT_ID, ParsedKey, PathResolver};
pub use provider::{PROVIDER_ID, session_to_view};
pub use reader::OpenClawSession;
pub use toolpath_convo::DeriveConfig;
pub use types::{
    AgentMessage, ContentBlock, CostBreakdown, Entry, EntryBase, SessionHeader, Usage,
};

use toolpath_convo::ConversationView;

/// How many `parentSession` links to follow when reading a session.
const MAX_PARENT_DEPTH: usize = 32;

/// High-level interface for reading OpenClaw sessions from a state directory.
///
/// OpenClaw buckets sessions by agent persona (`agents/<agentId>/sessions/`),
/// so the "project" axis used by [`toolpath_convo::ConversationProvider`] is
/// the **agent id** here (default [`DEFAULT_AGENT_ID`]).
#[derive(Debug, Clone, Default)]
pub struct OpenClawConvo {
    resolver: PathResolver,
}

impl OpenClawConvo {
    /// Build a manager with the default resolver (`~/.openclaw`).
    pub fn new() -> Self {
        Self {
            resolver: PathResolver::new(),
        }
    }

    /// Build a manager with a custom resolver (useful for tests / `--base`).
    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self { resolver }
    }

    /// Access the underlying resolver.
    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    /// Whether the OpenClaw `agents/` directory exists on disk.
    pub fn exists(&self) -> bool {
        self.resolver.agents_dir().exists()
    }

    /// List agent ids that have a sessions directory.
    pub fn list_agents(&self) -> Result<Vec<String>> {
        Ok(self.resolver.list_agent_ids()?)
    }

    /// List session metadata for one agent, newest first.
    pub fn list_sessions(&self, agent_id: &str) -> Result<Vec<SessionMeta>> {
        io::list_sessions(&self.resolver, agent_id)
    }

    /// List session metadata across every agent, newest first.
    pub fn list_all_sessions(&self) -> Result<Vec<SessionMeta>> {
        io::list_all_sessions(&self.resolver)
    }

    /// Read a specific session by id under an agent, following `parentSession`
    /// links and attaching the routing key from `sessions.json`.
    pub fn read_session(&self, agent_id: &str, session_id: &str) -> Result<OpenClawSession> {
        let file = self.resolver.resolve_session_file(agent_id, session_id)?;
        let mut session = reader::read_session_with_parent(&file, MAX_PARENT_DEPTH)?;
        session.attach_routing_key();
        Ok(session)
    }

    /// Read the most recently active session for an agent, if any.
    pub fn most_recent_session(&self, agent_id: &str) -> Result<Option<OpenClawSession>> {
        let mut metas = self.list_sessions(agent_id)?;
        if let Some(meta) = metas.drain(..).next() {
            Ok(Some(self.read_session(agent_id, &meta.id)?))
        } else {
            Ok(None)
        }
    }

    /// Convert a session into a provider-agnostic [`ConversationView`].
    pub fn to_view(&self, session: &OpenClawSession) -> ConversationView {
        provider::session_to_view(session)
    }

    /// Read every session for an agent (skipping unreadable ones with a warning).
    pub fn read_all_sessions(&self, agent_id: &str) -> Result<Vec<OpenClawSession>> {
        let metas = self.list_sessions(agent_id)?;
        let mut sessions = Vec::new();
        for meta in metas {
            match self.read_session(agent_id, &meta.id) {
                Ok(s) => sessions.push(s),
                Err(e) => eprintln!("Warning: failed to read OpenClaw session {}: {}", meta.id, e),
            }
        }
        Ok(sessions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a temp state dir with the DM fixture under agents/main/sessions.
    fn temp_state() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents/main/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = std::fs::read("tests/fixtures/dm_session.jsonl").unwrap();
        std::fs::write(dir.join("sess-abc.jsonl"), fixture).unwrap();
        std::fs::copy("tests/fixtures/sessions.json", dir.join("sessions.json")).unwrap();
        tmp
    }

    #[test]
    fn manager_reads_and_derives() {
        let tmp = temp_state();
        let mgr = OpenClawConvo::with_resolver(PathResolver::with_state_dir(tmp.path()));

        assert_eq!(mgr.list_agents().unwrap(), vec!["main".to_string()]);
        let sessions = mgr.list_sessions("main").unwrap();
        assert_eq!(sessions.len(), 1);

        let s = mgr.read_session("main", "sess-abc").unwrap();
        assert_eq!(s.parsed_key.as_ref().unwrap().channel.as_deref(), Some("whatsapp"));

        let path = derive_path(&s, &DeriveConfig::default());
        assert_eq!(path.meta.as_ref().unwrap().source.as_deref(), Some("openclaw"));
        assert!(
            path.steps
                .iter()
                .any(|st| st.step.actor == "human:whatsapp/15555550123")
        );
    }

    #[test]
    fn most_recent_session_works() {
        let tmp = temp_state();
        let mgr = OpenClawConvo::with_resolver(PathResolver::with_state_dir(tmp.path()));
        let s = mgr.most_recent_session("main").unwrap().unwrap();
        assert_eq!(s.header.id, "sess-abc");
    }
}
