use crate::error::{ConvoError, Result};
use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PathResolver {
    home_dir: Option<PathBuf>,
    claude_dir: Option<PathBuf>,
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PathResolver {
    pub fn new() -> Self {
        let home_dir = dirs::home_dir();
        Self {
            home_dir,
            claude_dir: None,
        }
    }

    pub fn with_home<P: Into<PathBuf>>(mut self, home: P) -> Self {
        self.home_dir = Some(home.into());
        self
    }

    pub fn with_claude_dir<P: Into<PathBuf>>(mut self, claude_dir: P) -> Self {
        self.claude_dir = Some(claude_dir.into());
        self
    }

    pub fn home_dir(&self) -> Result<&Path> {
        self.home_dir.as_deref().ok_or(ConvoError::NoHomeDirectory)
    }

    pub fn claude_dir(&self) -> Result<PathBuf> {
        if let Some(ref claude_dir) = self.claude_dir {
            return Ok(claude_dir.clone());
        }

        let home = self.home_dir()?;
        Ok(home.join(".claude"))
    }

    pub fn projects_dir(&self) -> Result<PathBuf> {
        Ok(self.claude_dir()?.join("projects"))
    }

    pub fn history_file(&self) -> Result<PathBuf> {
        Ok(self.claude_dir()?.join("history.jsonl"))
    }

    pub fn project_dir(&self, project_path: &str) -> Result<PathBuf> {
        let sanitized = sanitize_project_path(project_path);
        Ok(self.projects_dir()?.join(sanitized))
    }

    pub fn conversation_file(&self, project_path: &str, session_id: &str) -> Result<PathBuf> {
        Ok(self
            .project_dir(project_path)?
            .join(format!("{}.jsonl", session_id)))
    }

    pub fn list_project_dirs(&self) -> Result<Vec<String>> {
        let projects_dir = self.projects_dir()?;
        if !projects_dir.exists() {
            return Ok(Vec::new());
        }

        let mut projects = Vec::new();
        for entry in std::fs::read_dir(&projects_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                projects.push(unsanitize_project_path(name));
            }
        }
        Ok(projects)
    }

    pub fn list_conversations(&self, project_path: &str) -> Result<Vec<String>> {
        let project_dir = self.project_dir(project_path)?;
        if !project_dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();
        for entry in std::fs::read_dir(&project_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("jsonl")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                sessions.push(stem.to_string());
            }
        }
        Ok(sessions)
    }

    pub fn exists(&self) -> bool {
        self.claude_dir().map(|p| p.exists()).unwrap_or(false)
    }
}

fn sanitize_project_path(path: &str) -> String {
    // Claude Code converts both '/' and '_' to '-' when creating project directories
    path.replace(['/', '_'], "-")
}

fn unsanitize_project_path(sanitized: &str) -> String {
    sanitized.replace('-', "/")
}

mod dirs {
    use super::*;

    pub fn home_dir() -> Option<PathBuf> {
        env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_path_resolution() {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new()
            .with_home(temp.path())
            .with_claude_dir(temp.path().join(".claude"));

        let claude_dir = resolver.claude_dir().unwrap();
        assert_eq!(claude_dir, temp.path().join(".claude"));

        let projects_dir = resolver.projects_dir().unwrap();
        assert_eq!(projects_dir, temp.path().join(".claude/projects"));

        let history = resolver.history_file().unwrap();
        assert_eq!(history, temp.path().join(".claude/history.jsonl"));
    }

    #[test]
    fn test_project_path_sanitization() {
        assert_eq!(
            sanitize_project_path("/Users/alex/project"),
            "-Users-alex-project"
        );
        assert_eq!(
            unsanitize_project_path("-Users-alex-project"),
            "/Users/alex/project"
        );
    }

    #[test]
    fn test_conversation_file_path() {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new().with_claude_dir(temp.path());

        let convo_file = resolver
            .conversation_file("/Users/alex/project", "session-123")
            .unwrap();

        assert_eq!(
            convo_file,
            temp.path()
                .join("projects/-Users-alex-project/session-123.jsonl")
        );
    }

    #[test]
    fn test_list_projects() {
        let temp = TempDir::new().unwrap();
        let projects_dir = temp.path().join("projects");
        fs::create_dir_all(&projects_dir).unwrap();
        fs::create_dir(projects_dir.join("-Users-alex-project1")).unwrap();
        fs::create_dir(projects_dir.join("-Users-bob-project2")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(temp.path());
        let projects = resolver.list_project_dirs().unwrap();

        assert_eq!(projects.len(), 2);
        assert!(projects.contains(&"/Users/alex/project1".to_string()));
        assert!(projects.contains(&"/Users/bob/project2".to_string()));
    }

    #[test]
    fn test_list_projects_empty() {
        let temp = TempDir::new().unwrap();
        let projects_dir = temp.path().join("projects");
        fs::create_dir_all(&projects_dir).unwrap();

        let resolver = PathResolver::new().with_claude_dir(temp.path());
        let projects = resolver.list_project_dirs().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn test_list_projects_no_dir() {
        let temp = TempDir::new().unwrap();
        // Don't create projects dir
        let resolver = PathResolver::new().with_claude_dir(temp.path());
        let projects = resolver.list_project_dirs().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn test_list_conversations() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(project_dir.join("session-1.jsonl"), "{}").unwrap();
        fs::write(project_dir.join("session-2.jsonl"), "{}").unwrap();
        fs::write(project_dir.join("not-jsonl.txt"), "{}").unwrap();

        let resolver = PathResolver::new().with_claude_dir(temp.path());
        let sessions = resolver.list_conversations("/test/project").unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&"session-1".to_string()));
        assert!(sessions.contains(&"session-2".to_string()));
    }

    #[test]
    fn test_list_conversations_empty_project() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let resolver = PathResolver::new().with_claude_dir(temp.path());
        let sessions = resolver.list_conversations("/test/project").unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_list_conversations_no_project() {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new().with_claude_dir(temp.path());
        let sessions = resolver.list_conversations("/nonexistent/project").unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_exists() {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new().with_claude_dir(temp.path());
        assert!(resolver.exists());

        let resolver2 = PathResolver::new().with_claude_dir("/nonexistent/dir");
        assert!(!resolver2.exists());
    }

    #[test]
    fn test_with_home() {
        let resolver = PathResolver::new().with_home("/custom/home");
        assert_eq!(
            resolver.home_dir().unwrap().to_str().unwrap(),
            "/custom/home"
        );
    }

    #[test]
    fn test_history_file() {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new().with_claude_dir(temp.path());
        let hist = resolver.history_file().unwrap();
        assert!(hist.ends_with("history.jsonl"));
    }

    #[test]
    fn test_default_impl() {
        let resolver = PathResolver::default();
        // Should not panic, just use system home dir
        let _ = resolver.claude_dir();
    }
}
