use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum DeriveSource {
    /// Derive from git repository history
    Git {
        /// Path to the git repository
        #[arg(short, long, default_value = ".")]
        repo: PathBuf,

        /// Branch name(s). Format: `name` or `name:start`
        #[arg(short, long, required = true)]
        branch: Vec<String>,

        /// Global base commit (overrides per-branch starts)
        #[arg(long)]
        base: Option<String>,

        /// Remote name for URI generation
        #[arg(long, default_value = "origin")]
        remote: String,

        /// Graph title (for multi-branch output)
        #[arg(long)]
        title: Option<String>,
    },
    /// Derive from Claude conversation logs
    Claude {
        /// Project path (e.g., /Users/alex/myproject)
        #[arg(short, long)]
        project: String,

        /// Specific session ID
        #[arg(short, long)]
        session: Option<String>,

        /// Process all sessions in the project
        #[arg(long)]
        all: bool,
    },
}

pub fn run(source: DeriveSource, pretty: bool) -> Result<()> {
    match source {
        DeriveSource::Git {
            repo,
            branch,
            base,
            remote,
            title,
        } => run_git(repo, branch, base, remote, title, pretty),
        DeriveSource::Claude {
            project,
            session,
            all,
        } => run_claude(project, session, all, pretty),
    }
}

fn run_git(
    repo_path: PathBuf,
    branches: Vec<String>,
    base: Option<String>,
    remote: String,
    title: Option<String>,
    pretty: bool,
) -> Result<()> {
    let repo_path = if repo_path.is_absolute() {
        repo_path
    } else {
        std::env::current_dir()?.join(&repo_path)
    };

    let repo = git2::Repository::open(&repo_path)
        .with_context(|| format!("Failed to open repository at {:?}", repo_path))?;

    let config = toolpath_git::DeriveConfig {
        remote,
        title,
        base,
    };

    let doc = toolpath_git::derive(&repo, &branches, &config)?;

    let json = if pretty {
        doc.to_json_pretty()?
    } else {
        doc.to_json()?
    };

    println!("{}", json);
    Ok(())
}

fn run_claude(project: String, session: Option<String>, all: bool, pretty: bool) -> Result<()> {
    let manager = toolpath_claude::ClaudeConvo::new();
    run_claude_with_manager(&manager, project, session, all, pretty)
}

fn run_claude_with_manager(
    manager: &toolpath_claude::ClaudeConvo,
    project: String,
    session: Option<String>,
    all: bool,
    pretty: bool,
) -> Result<()> {
    let config = toolpath_claude::derive::DeriveConfig {
        project_path: Some(project.clone()),
        include_thinking: false,
    };

    let docs: Vec<toolpath::v1::Path> = if let Some(session_id) = session {
        let convo = manager
            .read_conversation(&project, &session_id)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        vec![toolpath_claude::derive::derive_path(&convo, &config)]
    } else if all {
        let convos = manager
            .read_all_conversations(&project)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        toolpath_claude::derive::derive_project(&convos, &config)
    } else {
        // Default: most recent conversation
        let convo = manager
            .most_recent_conversation(&project)
            .map_err(|e| anyhow::anyhow!("{}", e))?
            .ok_or_else(|| anyhow::anyhow!("No conversations found for project: {}", project))?;
        vec![toolpath_claude::derive::derive_path(&convo, &config)]
    };

    for path in &docs {
        let doc = toolpath::v1::Document::Path(path.clone());
        let json = if pretty {
            doc.to_json_pretty()?
        } else {
            doc.to_json()?
        };
        println!("{}", json);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_temp_repo() -> (tempfile::TempDir, git2::Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
        (dir, repo)
    }

    fn create_commit(
        repo: &git2::Repository,
        message: &str,
        file_name: &str,
        content: &str,
        parent: Option<&git2::Commit>,
    ) -> git2::Oid {
        let mut index = repo.index().unwrap();
        let file_path = repo.workdir().unwrap().join(file_name);
        std::fs::write(&file_path, content).unwrap();
        index.add_path(std::path::Path::new(file_name)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let parents: Vec<&git2::Commit> = parent.into_iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap()
    }

    #[test]
    fn test_run_git_single_branch() {
        let (dir, repo) = init_temp_repo();
        let oid = create_commit(&repo, "initial commit", "file.txt", "hello", None);
        let c1 = repo.find_commit(oid).unwrap();
        create_commit(&repo, "second", "file.txt", "world", Some(&c1));

        let default = toolpath_git::list_branches(&repo)
            .unwrap()
            .first()
            .unwrap()
            .name
            .clone();

        let result = run_git(
            dir.path().to_path_buf(),
            vec![default],
            None,
            "origin".to_string(),
            None,
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_git_pretty() {
        let (dir, repo) = init_temp_repo();
        create_commit(&repo, "initial", "file.txt", "hello", None);

        let default = toolpath_git::list_branches(&repo)
            .unwrap()
            .first()
            .unwrap()
            .name
            .clone();

        let result = run_git(
            dir.path().to_path_buf(),
            vec![default],
            None,
            "origin".to_string(),
            None,
            true,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_git_invalid_repo() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_git(
            dir.path().to_path_buf(),
            vec!["main".to_string()],
            None,
            "origin".to_string(),
            None,
            false,
        );
        assert!(result.is_err());
    }

    fn setup_claude_manager() -> (tempfile::TempDir, toolpath_claude::ClaudeConvo) {
        let temp = tempfile::tempdir().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        std::fs::create_dir_all(&project_dir).unwrap();

        let entry1 = r#"{"type":"user","uuid":"uuid-1","timestamp":"2024-01-01T00:00:00Z","cwd":"/test/project","message":{"role":"user","content":"Hello"}}"#;
        let entry2 = r#"{"type":"assistant","uuid":"uuid-2","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi there"}}"#;
        std::fs::write(
            project_dir.join("session-abc.jsonl"),
            format!("{}\n{}\n", entry1, entry2),
        )
        .unwrap();

        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        let manager = toolpath_claude::ClaudeConvo::with_resolver(resolver);
        (temp, manager)
    }

    #[test]
    fn test_run_claude_session() {
        let (_temp, manager) = setup_claude_manager();
        let result = run_claude_with_manager(
            &manager,
            "/test/project".to_string(),
            Some("session-abc".to_string()),
            false,
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_claude_session_pretty() {
        let (_temp, manager) = setup_claude_manager();
        let result = run_claude_with_manager(
            &manager,
            "/test/project".to_string(),
            Some("session-abc".to_string()),
            false,
            true,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_claude_most_recent() {
        let (_temp, manager) = setup_claude_manager();
        let result =
            run_claude_with_manager(&manager, "/test/project".to_string(), None, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_claude_all() {
        let (_temp, manager) = setup_claude_manager();
        let result =
            run_claude_with_manager(&manager, "/test/project".to_string(), None, true, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_claude_no_conversations() {
        let temp = tempfile::tempdir().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-empty-project");
        std::fs::create_dir_all(&project_dir).unwrap();

        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        let manager = toolpath_claude::ClaudeConvo::with_resolver(resolver);

        let result =
            run_claude_with_manager(&manager, "/empty/project".to_string(), None, false, false);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No conversations found")
        );
    }
}
