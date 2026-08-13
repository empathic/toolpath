use crate::error::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PathResolver {
    home_dir: PathBuf,
    claude_dir: Option<PathBuf>,
}

impl PathResolver {
    pub fn new<P: Into<PathBuf>>(home: P) -> Self {
        Self {
            home_dir: home.into(),
            claude_dir: None,
        }
    }

    /// Override the claude directory directly (defaults to
    /// `~/.claude`).
    pub fn with_claude_dir<P: Into<PathBuf>>(mut self, claude_dir: P) -> Self {
        self.claude_dir = Some(claude_dir.into());
        self
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    pub fn claude_dir(&self) -> PathBuf {
        match &self.claude_dir {
            Some(claude_dir) => claude_dir.clone(),
            None => self.home_dir.join(".claude"),
        }
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.claude_dir().join("projects")
    }

    pub fn history_file(&self) -> PathBuf {
        self.claude_dir().join("history.jsonl")
    }

    pub fn project_dir(&self, project_path: &str) -> PathBuf {
        self.projects_dir()
            .join(sanitize_project_path(project_path))
    }

    pub fn conversation_file(&self, project_path: &str, session_id: &str) -> PathBuf {
        self.project_dir(project_path)
            .join(format!("{}.jsonl", session_id))
    }

    pub fn list_project_dirs(&self) -> Result<Vec<String>> {
        let projects_dir = self.projects_dir();
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
        let project_dir = self.project_dir(project_path);
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
        self.claude_dir().exists()
    }
}

fn sanitize_project_path(path: &str) -> String {
    // Claude Code maps '/', '_', and '.' to '-' when creating project
    // directories. Notably, paths under dotdirs like `.claude/worktrees/…`
    // double-up the dash (the leading `/.` becomes `--`).
    path.replace(['/', '_', '.'], "-")
}

pub(crate) fn unsanitize_project_path(sanitized: &str) -> String {
    sanitized.replace('-', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A resolver rooted at a temporary home, so the claude directory
    /// is `<temp>/.claude`.
    fn setup() -> (TempDir, PathResolver) {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new(temp.path());
        (temp, resolver)
    }

    #[test]
    fn test_path_resolution() {
        let (temp, resolver) = setup();

        assert_eq!(resolver.claude_dir(), temp.path().join(".claude"));
        assert_eq!(
            resolver.projects_dir(),
            temp.path().join(".claude/projects")
        );
        assert_eq!(
            resolver.history_file(),
            temp.path().join(".claude/history.jsonl")
        );
    }

    #[test]
    fn claude_dir_override_wins_against_home() {
        let resolver = PathResolver::new("/custom/home").with_claude_dir("/custom/.claude");
        assert_eq!(resolver.claude_dir(), PathBuf::from("/custom/.claude"));
        assert_eq!(resolver.home_dir(), Path::new("/custom/home"));
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
    fn test_project_path_sanitization_with_dots() {
        // Paths under dotted directories (.claude/worktrees, github.com/…) must
        // be encoded the same way Claude Code does — every '.' becomes '-'.
        assert_eq!(
            sanitize_project_path("/Users/alex/code/github.com/x/repo/.claude/worktrees/foo"),
            "-Users-alex-code-github-com-x-repo--claude-worktrees-foo"
        );
    }

    #[test]
    fn test_conversation_file_path() {
        let (temp, resolver) = setup();

        assert_eq!(
            resolver.conversation_file("/Users/alex/project", "session-123"),
            temp.path()
                .join(".claude/projects/-Users-alex-project/session-123.jsonl")
        );
    }

    #[test]
    fn test_list_projects() {
        let (_temp, resolver) = setup();
        let projects_dir = resolver.projects_dir();
        fs::create_dir_all(&projects_dir).unwrap();
        fs::create_dir(projects_dir.join("-Users-alex-project1")).unwrap();
        fs::create_dir(projects_dir.join("-Users-bob-project2")).unwrap();

        let projects = resolver.list_project_dirs().unwrap();

        assert_eq!(projects.len(), 2);
        assert!(projects.contains(&"/Users/alex/project1".to_string()));
        assert!(projects.contains(&"/Users/bob/project2".to_string()));
    }

    #[test]
    fn test_list_projects_empty() {
        let (_temp, resolver) = setup();
        fs::create_dir_all(resolver.projects_dir()).unwrap();

        let projects = resolver.list_project_dirs().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn test_list_projects_no_dir() {
        let (_temp, resolver) = setup();
        let projects = resolver.list_project_dirs().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn test_list_conversations() {
        let (_temp, resolver) = setup();
        let project_dir = resolver.project_dir("/test/project");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(project_dir.join("session-1.jsonl"), "{}").unwrap();
        fs::write(project_dir.join("session-2.jsonl"), "{}").unwrap();
        fs::write(project_dir.join("not-jsonl.txt"), "{}").unwrap();

        let sessions = resolver.list_conversations("/test/project").unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&"session-1".to_string()));
        assert!(sessions.contains(&"session-2".to_string()));
    }

    #[test]
    fn test_list_conversations_empty_project() {
        let (_temp, resolver) = setup();
        fs::create_dir_all(resolver.project_dir("/test/project")).unwrap();

        let sessions = resolver.list_conversations("/test/project").unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_list_conversations_no_project() {
        let (_temp, resolver) = setup();
        let sessions = resolver.list_conversations("/nonexistent/project").unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_exists() {
        let (_temp, resolver) = setup();
        assert!(!resolver.exists());
        fs::create_dir_all(resolver.claude_dir()).unwrap();
        assert!(resolver.exists());

        assert!(!PathResolver::new("/nonexistent/home").exists());
    }

    #[test]
    fn test_home_dir() {
        let resolver = PathResolver::new("/custom/home");
        assert_eq!(resolver.home_dir(), Path::new("/custom/home"));
    }

    #[test]
    fn test_history_file() {
        let (_temp, resolver) = setup();
        assert!(resolver.history_file().ends_with("history.jsonl"));
    }
}
