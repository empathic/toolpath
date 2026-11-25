//! Filesystem layout for the Antigravity CLI (`agy`) state tree.
//!
//! Antigravity shares its root with Gemini CLI (`~/.gemini`) but keeps its
//! own state flat under `~/.gemini/antigravity-cli/`: there is no
//! per-project slot (unlike `toolpath-gemini`'s `tmp/<project>/chats/`).
//! Conversations are tied to a workspace only loosely, through
//! `conversation_summaries.workspace_uris` (a text column whose encoding
//! is unconfirmed — see `docs/agents/formats/antigravity.md`) and
//! `cache/last_conversations.json` (`{<abs cwd>: <uuid>}`). See
//! `docs/agents/formats/antigravity.md` for the full layout and its
//! sourcing.

use std::path::{Path, PathBuf};

const GEMINI_DIR: &str = ".gemini";
const CLI_SUBDIR: &str = "antigravity-cli";
const CONVERSATIONS_SUBDIR: &str = "conversations";
const CACHE_SUBDIR: &str = "cache";
const SUMMARIES_DB: &str = "conversation_summaries.db";
const LAST_CONVERSATIONS_JSON: &str = "last_conversations.json";

/// Builder-style resolver over the Antigravity CLI data directory.
#[derive(Debug, Clone)]
pub struct PathResolver {
    home_dir: Option<PathBuf>,
    gemini_dir: Option<PathBuf>,
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PathResolver {
    pub fn new() -> Self {
        Self {
            home_dir: home_dir(),
            gemini_dir: None,
        }
    }

    pub fn with_home<P: Into<PathBuf>>(mut self, home: P) -> Self {
        self.home_dir = Some(home.into());
        self
    }

    /// Override `~/.gemini` directly.
    pub fn with_gemini_dir<P: Into<PathBuf>>(mut self, dir: P) -> Self {
        self.gemini_dir = Some(dir.into());
        self
    }

    pub fn home_dir(&self) -> crate::error::Result<&Path> {
        self.home_dir
            .as_deref()
            .ok_or(crate::error::ConvoError::NoHomeDirectory)
    }

    pub fn gemini_dir(&self) -> crate::error::Result<PathBuf> {
        if let Some(d) = &self.gemini_dir {
            return Ok(d.clone());
        }
        Ok(self.home_dir()?.join(GEMINI_DIR))
    }

    pub fn antigravity_dir(&self) -> crate::error::Result<PathBuf> {
        Ok(self.gemini_dir()?.join(CLI_SUBDIR))
    }

    pub fn conversations_dir(&self) -> crate::error::Result<PathBuf> {
        Ok(self.antigravity_dir()?.join(CONVERSATIONS_SUBDIR))
    }

    pub fn summaries_db_path(&self) -> crate::error::Result<PathBuf> {
        Ok(self.antigravity_dir()?.join(SUMMARIES_DB))
    }

    pub fn cache_dir(&self) -> crate::error::Result<PathBuf> {
        Ok(self.antigravity_dir()?.join(CACHE_SUBDIR))
    }

    pub fn last_conversations_path(&self) -> crate::error::Result<PathBuf> {
        Ok(self.cache_dir()?.join(LAST_CONVERSATIONS_JSON))
    }

    /// Path to a single conversation's store. Prefers the current
    /// `.db` (SQLite) layout; falls back to the older `.pb` (raw
    /// serialized `Conversation`) layout when only that exists.
    pub fn conversation_path(&self, conversation_id: &str) -> crate::error::Result<PathBuf> {
        let dir = self.conversations_dir()?;
        let db = dir.join(format!("{conversation_id}.db"));
        if db.exists() {
            return Ok(db);
        }
        let pb = dir.join(format!("{conversation_id}.pb"));
        if pb.exists() {
            return Ok(pb);
        }
        Ok(db) // caller's open() produces a clean NotFound error
    }

    pub fn exists(&self) -> bool {
        self.antigravity_dir().map(|p| p.exists()).unwrap_or(false)
    }

    pub fn summaries_db_exists(&self) -> bool {
        self.summaries_db_path()
            .map(|p| p.exists())
            .unwrap_or(false)
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathResolver) {
        let temp = TempDir::new().unwrap();
        let cli = temp.path().join(".gemini/antigravity-cli");
        fs::create_dir_all(&cli).unwrap();
        let resolver = PathResolver::new().with_home(temp.path());
        (temp, resolver)
    }

    #[test]
    fn gemini_dir_defaults_under_home() {
        let (t, r) = setup();
        assert_eq!(r.gemini_dir().unwrap(), t.path().join(".gemini"));
    }

    #[test]
    fn antigravity_dir_is_the_cli_subdir() {
        let (t, r) = setup();
        assert_eq!(
            r.antigravity_dir().unwrap(),
            t.path().join(".gemini/antigravity-cli")
        );
    }

    #[test]
    fn summaries_db_path_under_antigravity_dir() {
        let (_t, r) = setup();
        assert!(
            r.summaries_db_path()
                .unwrap()
                .ends_with("antigravity-cli/conversation_summaries.db")
        );
    }

    #[test]
    fn last_conversations_path_under_cache() {
        let (_t, r) = setup();
        assert!(
            r.last_conversations_path()
                .unwrap()
                .ends_with("antigravity-cli/cache/last_conversations.json")
        );
    }

    #[test]
    fn conversation_path_prefers_db_over_pb() {
        let (t, r) = setup();
        let dir = t.path().join(".gemini/antigravity-cli/conversations");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("abc.db"), b"").unwrap();
        fs::write(dir.join("abc.pb"), b"").unwrap();
        assert!(
            r.conversation_path("abc")
                .unwrap()
                .to_string_lossy()
                .ends_with("abc.db")
        );
    }

    #[test]
    fn conversation_path_falls_back_to_pb() {
        let (t, r) = setup();
        let dir = t.path().join(".gemini/antigravity-cli/conversations");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("xyz.pb"), b"").unwrap();
        assert!(
            r.conversation_path("xyz")
                .unwrap()
                .to_string_lossy()
                .ends_with("xyz.pb")
        );
    }

    #[test]
    fn exists_reflects_antigravity_dir() {
        let (_t, r) = setup();
        assert!(r.exists());
        let missing = PathResolver::new().with_gemini_dir("/never/exists/.gemini");
        assert!(!missing.exists());
    }

    #[test]
    fn with_gemini_dir_overrides_home_derivation() {
        let r = PathResolver::new().with_gemini_dir("/custom/.gemini");
        assert_eq!(
            r.antigravity_dir().unwrap(),
            PathBuf::from("/custom/.gemini/antigravity-cli")
        );
    }
}
