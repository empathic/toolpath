use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum ListSource {
    /// List git branches in a repository
    Git {
        /// Path to the git repository
        #[arg(short, long, default_value = ".")]
        repo: PathBuf,

        /// Remote name for URI generation
        #[arg(long, default_value = "origin")]
        remote: String,
    },
    /// List Claude projects or sessions
    Claude {
        /// Project path — if omitted, lists all projects
        #[arg(short, long)]
        project: Option<String>,
    },
}

pub fn run(source: ListSource, json: bool) -> Result<()> {
    match source {
        ListSource::Git { repo, remote } => run_git(repo, remote, json),
        ListSource::Claude { project } => run_claude(project, json),
    }
}

fn run_git(repo_path: PathBuf, remote: String, json: bool) -> Result<()> {
    let repo_path = if repo_path.is_absolute() {
        repo_path
    } else {
        std::env::current_dir()?.join(&repo_path)
    };

    let repo = git2::Repository::open(&repo_path)
        .with_context(|| format!("Failed to open repository at {:?}", repo_path))?;

    let uri = toolpath_git::get_repo_uri(&repo, &remote)?;
    let branches = toolpath_git::list_branches(&repo)?;

    if json {
        let items: Vec<serde_json::Value> = branches
            .iter()
            .map(|b| {
                serde_json::json!({
                    "name": b.name,
                    "head": b.head,
                    "subject": b.subject,
                    "author": b.author,
                    "timestamp": b.timestamp,
                })
            })
            .collect();
        let output = serde_json::json!({
            "source": "git",
            "uri": uri,
            "branches": items,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Repository: {}", uri);
        println!();
        if branches.is_empty() {
            println!("  (no local branches)");
        } else {
            for b in &branches {
                println!("  {} {} {}", b.head_short, b.name, truncate(&b.subject, 60));
            }
        }
    }
    Ok(())
}

fn run_claude(project: Option<String>, json: bool) -> Result<()> {
    let manager = toolpath_claude::ClaudeConvo::new();

    match project {
        None => list_claude_projects(&manager, json),
        Some(project_path) => list_claude_sessions(&manager, &project_path, json),
    }
}

fn list_claude_projects(manager: &toolpath_claude::ClaudeConvo, json: bool) -> Result<()> {
    let projects = manager
        .list_projects()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    if json {
        let items: Vec<serde_json::Value> = projects
            .iter()
            .map(|p| serde_json::json!({ "path": p }))
            .collect();
        let output = serde_json::json!({
            "source": "claude",
            "projects": items,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Claude projects:");
        println!();
        if projects.is_empty() {
            println!("  (none)");
        } else {
            for p in &projects {
                println!("  {}", p);
            }
        }
    }
    Ok(())
}

fn list_claude_sessions(
    manager: &toolpath_claude::ClaudeConvo,
    project_path: &str,
    json: bool,
) -> Result<()> {
    let metadata = manager
        .list_conversation_metadata(project_path)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    if json {
        let items: Vec<serde_json::Value> = metadata
            .iter()
            .map(|m| {
                serde_json::json!({
                    "session_id": m.session_id,
                    "messages": m.message_count,
                    "started_at": m.started_at.map(|t| t.to_rfc3339()),
                    "last_activity": m.last_activity.map(|t| t.to_rfc3339()),
                })
            })
            .collect();
        let output = serde_json::json!({
            "source": "claude",
            "project": project_path,
            "sessions": items,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Sessions for {}:", project_path);
        println!();
        if metadata.is_empty() {
            println!("  (none)");
        } else {
            for m in &metadata {
                let date = m
                    .last_activity
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                println!("  {} {:>4} msgs  {}", &m.session_id, m.message_count, date);
            }
        }
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 3).collect();
        format!("{}...", truncated)
    }
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
    fn test_run_git_human_readable() {
        let (dir, repo) = init_temp_repo();
        create_commit(&repo, "initial commit", "file.txt", "hello", None);

        let result = run_git(dir.path().to_path_buf(), "origin".to_string(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_git_json() {
        let (dir, repo) = init_temp_repo();
        create_commit(&repo, "initial commit", "file.txt", "hello", None);

        let result = run_git(dir.path().to_path_buf(), "origin".to_string(), true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_git_invalid_repo() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_git(dir.path().to_path_buf(), "origin".to_string(), false);
        assert!(result.is_err());
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let result = truncate("hello world, this is a long string", 15);
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 15);
    }

    #[test]
    fn test_truncate_multibyte() {
        let result = truncate("日本語のテスト文字列です", 8);
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 8);
    }

    fn setup_claude_manager() -> (tempfile::TempDir, toolpath_claude::ClaudeConvo) {
        let temp = tempfile::tempdir().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        std::fs::create_dir_all(&project_dir).unwrap();

        let entry1 = r#"{"type":"user","uuid":"uuid-1","timestamp":"2024-01-01T00:00:00Z","cwd":"/test/project","message":{"role":"user","content":"Hello"}}"#;
        let entry2 = r#"{"type":"assistant","uuid":"uuid-2","timestamp":"2024-01-01T00:01:00Z","message":{"role":"assistant","content":"Hi there"}}"#;
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
    fn test_list_claude_projects_human() {
        let (_temp, manager) = setup_claude_manager();
        let result = list_claude_projects(&manager, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_projects_json() {
        let (_temp, manager) = setup_claude_manager();
        let result = list_claude_projects(&manager, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_projects_empty() {
        let temp = tempfile::tempdir().unwrap();
        let claude_dir = temp.path().join(".claude");
        let projects_dir = claude_dir.join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        let manager = toolpath_claude::ClaudeConvo::with_resolver(resolver);

        let result = list_claude_projects(&manager, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_sessions_human() {
        let (_temp, manager) = setup_claude_manager();
        let result = list_claude_sessions(&manager, "/test/project", false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_sessions_json() {
        let (_temp, manager) = setup_claude_manager();
        let result = list_claude_sessions(&manager, "/test/project", true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_sessions_empty() {
        let temp = tempfile::tempdir().unwrap();
        let claude_dir = temp.path().join(".claude");
        let projects_dir = claude_dir.join("projects/-empty-project");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        let manager = toolpath_claude::ClaudeConvo::with_resolver(resolver);

        let result = list_claude_sessions(&manager, "/empty/project", false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_claude_projects() {
        let (_temp, manager) = setup_claude_manager();
        // Test the dispatch to list_claude_projects through run_claude-like path
        let result = list_claude_projects(&manager, false);
        assert!(result.is_ok());
    }
}
