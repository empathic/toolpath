#![doc = include_str!("../README.md")]

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use git2::{Commit, DiffOptions, Oid, Repository};
use std::collections::HashMap;
use toolpath::v1::{
    ActorDefinition, ArtifactChange, Base, Document, Graph, GraphIdentity, GraphMeta, Identity,
    Path, PathIdentity, PathMeta, PathOrRef, Step, StepIdentity, StepMeta, VcsSource,
};

// ============================================================================
// Public configuration and types
// ============================================================================

/// Configuration for deriving Toolpath documents from a git repository.
pub struct DeriveConfig {
    /// Remote name for URI generation (e.g., "origin").
    pub remote: String,
    /// Optional title for graph output.
    pub title: Option<String>,
    /// Global base commit override (overrides per-branch starts).
    pub base: Option<String>,
}

/// Parsed branch specification.
///
/// Branches can be specified as `"name"` or `"name:start"` where `start` is a
/// revision expression indicating where the path should begin.
#[derive(Debug, Clone)]
pub struct BranchSpec {
    pub name: String,
    pub start: Option<String>,
}

impl BranchSpec {
    /// Parse a branch specification string.
    ///
    /// Format: `"name"` or `"name:start"`.
    pub fn parse(s: &str) -> Self {
        if let Some((name, start)) = s.split_once(':') {
            BranchSpec {
                name: name.to_string(),
                start: Some(start.to_string()),
            }
        } else {
            BranchSpec {
                name: s.to_string(),
                start: None,
            }
        }
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Derive a Toolpath [`Document`] from the given repository and branch names.
///
/// Branch strings are parsed as [`BranchSpec`]s (supporting `"name:start"` syntax).
/// A single branch produces a [`Document::Path`]; multiple branches produce a
/// [`Document::Graph`].
pub fn derive(repo: &Repository, branches: &[String], config: &DeriveConfig) -> Result<Document> {
    let branch_specs: Vec<BranchSpec> = branches.iter().map(|s| BranchSpec::parse(s)).collect();

    if branch_specs.len() == 1 {
        let path_doc = derive_path(repo, &branch_specs[0], config)?;
        Ok(Document::Path(path_doc))
    } else {
        let graph_doc = derive_graph(repo, &branch_specs, config)?;
        Ok(Document::Graph(graph_doc))
    }
}

/// Derive a Toolpath [`Path`] from a single branch specification.
pub fn derive_path(repo: &Repository, spec: &BranchSpec, config: &DeriveConfig) -> Result<Path> {
    let repo_uri = get_repo_uri(repo, &config.remote)?;

    let branch_ref = repo
        .find_branch(&spec.name, git2::BranchType::Local)
        .with_context(|| format!("Branch '{}' not found", spec.name))?;
    let branch_commit = branch_ref.get().peel_to_commit()?;

    // Determine base commit
    let base_oid = if let Some(global_base) = &config.base {
        // Global base overrides per-branch
        let obj = repo
            .revparse_single(global_base)
            .with_context(|| format!("Failed to parse base ref '{}'", global_base))?;
        obj.peel_to_commit()?.id()
    } else if let Some(start) = &spec.start {
        // Per-branch start commit - resolve relative to the branch
        // e.g., "main:HEAD~5" means 5 commits before main's HEAD
        let start_ref = if let Some(rest) = start.strip_prefix("HEAD") {
            // Replace HEAD with the branch name for relative refs
            format!("{}{}", spec.name, rest)
        } else {
            start.clone()
        };
        let obj = repo.revparse_single(&start_ref).with_context(|| {
            format!(
                "Failed to parse start ref '{}' (resolved to '{}') for branch '{}'",
                start, start_ref, spec.name
            )
        })?;
        obj.peel_to_commit()?.id()
    } else {
        // Default: find merge-base with default branch
        find_base_for_branch(repo, &branch_commit)?
    };

    let base_commit = repo.find_commit(base_oid)?;

    // Collect commits from base to head
    let commits = collect_commits(repo, base_oid, branch_commit.id())?;

    // Generate steps and collect actor definitions
    let mut actors: HashMap<String, ActorDefinition> = HashMap::new();
    let steps = generate_steps(repo, &commits, base_oid, &mut actors)?;

    // Build path document
    let head_step_id = if steps.is_empty() {
        format!("step-{}", short_oid(branch_commit.id()))
    } else {
        steps.last().unwrap().step.id.clone()
    };

    Ok(Path {
        path: PathIdentity {
            id: format!("path-{}", spec.name.replace('/', "-")),
            base: Some(Base {
                uri: repo_uri,
                ref_str: Some(base_commit.id().to_string()),
            }),
            head: head_step_id,
        },
        steps,
        meta: Some(PathMeta {
            title: Some(format!("Branch: {}", spec.name)),
            actors: if actors.is_empty() {
                None
            } else {
                Some(actors)
            },
            ..Default::default()
        }),
    })
}

/// Derive a Toolpath [`Graph`] from multiple branch specifications.
pub fn derive_graph(
    repo: &Repository,
    branch_specs: &[BranchSpec],
    config: &DeriveConfig,
) -> Result<Graph> {
    // Find the default branch name
    let default_branch = find_default_branch(repo);

    // If the default branch is included without an explicit start, compute the earliest
    // merge-base among all other branches to use as its starting point
    let default_branch_start = compute_default_branch_start(repo, branch_specs, &default_branch)?;

    // Generate paths for each branch with its own base
    let mut paths = Vec::new();
    for spec in branch_specs {
        // Check if this is the default branch and needs special handling
        let effective_spec = if default_branch_start.is_some()
            && spec.start.is_none()
            && default_branch.as_ref() == Some(&spec.name)
        {
            BranchSpec {
                name: spec.name.clone(),
                start: default_branch_start.clone(),
            }
        } else {
            spec.clone()
        };
        let path_doc = derive_path(repo, &effective_spec, config)?;
        paths.push(PathOrRef::Path(Box::new(path_doc)));
    }

    // Create graph ID from branch names
    let branch_names: Vec<&str> = branch_specs.iter().map(|s| s.name.as_str()).collect();
    let graph_id = if branch_names.len() <= 3 {
        format!(
            "graph-{}",
            branch_names
                .iter()
                .map(|b| b.replace('/', "-"))
                .collect::<Vec<_>>()
                .join("-")
        )
    } else {
        format!("graph-{}-branches", branch_names.len())
    };

    let title = config
        .title
        .clone()
        .unwrap_or_else(|| format!("Branches: {}", branch_names.join(", ")));

    Ok(Graph {
        graph: GraphIdentity { id: graph_id },
        paths,
        meta: Some(GraphMeta {
            title: Some(title),
            ..Default::default()
        }),
    })
}

// ============================================================================
// Public utility functions
// ============================================================================

/// Get the repository URI from a remote, falling back to a file:// URI.
pub fn get_repo_uri(repo: &Repository, remote_name: &str) -> Result<String> {
    if let Ok(remote) = repo.find_remote(remote_name)
        && let Some(url) = remote.url()
    {
        return Ok(normalize_git_url(url));
    }

    // Fall back to file path
    if let Some(path) = repo.path().parent() {
        return Ok(format!("file://{}", path.display()));
    }

    Ok("file://unknown".to_string())
}

/// Normalize a git remote URL to a canonical short form.
///
/// Converts common hosting URLs to compact identifiers:
/// - `git@github.com:org/repo.git` -> `github:org/repo`
/// - `https://github.com/org/repo.git` -> `github:org/repo`
/// - `git@gitlab.com:org/repo.git` -> `gitlab:org/repo`
/// - `https://gitlab.com/org/repo.git` -> `gitlab:org/repo`
///
/// # Examples
///
/// ```
/// use toolpath_git::normalize_git_url;
///
/// assert_eq!(normalize_git_url("git@github.com:org/repo.git"), "github:org/repo");
/// assert_eq!(normalize_git_url("https://gitlab.com/org/repo"), "gitlab:org/repo");
///
/// // Unknown hosts pass through unchanged
/// assert_eq!(
///     normalize_git_url("https://bitbucket.org/org/repo"),
///     "https://bitbucket.org/org/repo",
/// );
/// ```
pub fn normalize_git_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let repo = rest.trim_end_matches(".git");
        return format!("github:{}", repo);
    }

    if let Some(rest) = url.strip_prefix("https://github.com/") {
        let repo = rest.trim_end_matches(".git");
        return format!("github:{}", repo);
    }

    if let Some(rest) = url.strip_prefix("git@gitlab.com:") {
        let repo = rest.trim_end_matches(".git");
        return format!("gitlab:{}", repo);
    }

    if let Some(rest) = url.strip_prefix("https://gitlab.com/") {
        let repo = rest.trim_end_matches(".git");
        return format!("gitlab:{}", repo);
    }

    // Return as-is for other URLs
    url.to_string()
}

/// Create a URL-safe slug from a git author name and email.
///
/// Prefers the email username; falls back to the name.
///
/// # Examples
///
/// ```
/// use toolpath_git::slugify_author;
///
/// assert_eq!(slugify_author("Alex Smith", "asmith@example.com"), "asmith");
/// assert_eq!(slugify_author("Alex Smith", "unknown"), "alex-smith");
/// ```
pub fn slugify_author(name: &str, email: &str) -> String {
    // Try to extract username from email
    if let Some(username) = email.split('@').next()
        && !username.is_empty()
        && username != email
    {
        return username
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
    }

    // Fall back to name
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// ============================================================================
// Listing / discovery
// ============================================================================

/// Summary information about a local branch.
#[derive(Debug, Clone)]
pub struct BranchInfo {
    /// Branch name (e.g., "main", "feature/foo").
    pub name: String,
    /// Short (8-char) hex of the tip commit.
    pub head_short: String,
    /// Full hex OID of the tip commit.
    pub head: String,
    /// First line of the tip commit message.
    pub subject: String,
    /// Author name of the tip commit.
    pub author: String,
    /// ISO 8601 timestamp of the tip commit.
    pub timestamp: String,
}

/// List local branches with summary metadata.
pub fn list_branches(repo: &Repository) -> Result<Vec<BranchInfo>> {
    let mut branches = Vec::new();

    for branch_result in repo.branches(Some(git2::BranchType::Local))? {
        let (branch, _) = branch_result?;
        let name = branch.name()?.unwrap_or("<invalid utf-8>").to_string();

        let commit = branch.get().peel_to_commit()?;

        let author = commit.author();
        let author_name = author.name().unwrap_or("unknown").to_string();

        let time = commit.time();
        let timestamp = DateTime::<Utc>::from_timestamp(time.seconds(), 0)
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

        let subject = commit
            .message()
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .to_string();

        branches.push(BranchInfo {
            name,
            head_short: short_oid(commit.id()),
            head: commit.id().to_string(),
            subject,
            author: author_name,
            timestamp,
        });
    }

    branches.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(branches)
}

// ============================================================================
// Private helpers
// ============================================================================

/// When the default branch is included in a multi-branch graph without an explicit start,
/// compute the earliest merge-base among all feature branches to use as main's start.
/// This ensures we see main's commits back to where the earliest feature diverged.
fn compute_default_branch_start(
    repo: &Repository,
    branch_specs: &[BranchSpec],
    default_branch: &Option<String>,
) -> Result<Option<String>> {
    let default_name = match default_branch {
        Some(name) => name,
        None => return Ok(None),
    };

    // Check if the default branch is in the list and doesn't have an explicit start
    let default_in_list = branch_specs
        .iter()
        .any(|s| &s.name == default_name && s.start.is_none());
    if !default_in_list {
        return Ok(None);
    }

    // Get the default branch commit
    let default_ref = repo.find_branch(default_name, git2::BranchType::Local)?;
    let default_commit = default_ref.get().peel_to_commit()?;

    // Find the earliest merge-base among all non-default branches
    let mut earliest_base: Option<Oid> = None;

    for spec in branch_specs {
        if &spec.name == default_name {
            continue;
        }

        let branch_ref = match repo.find_branch(&spec.name, git2::BranchType::Local) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let branch_commit = match branch_ref.get().peel_to_commit() {
            Ok(c) => c,
            Err(_) => continue,
        };

        if let Ok(merge_base) = repo.merge_base(default_commit.id(), branch_commit.id()) {
            // Check if this merge-base is earlier (ancestor of) current earliest
            match earliest_base {
                None => earliest_base = Some(merge_base),
                Some(current) => {
                    // If merge_base is an ancestor of current, use merge_base
                    // (it's "earlier" in the commit history)
                    if repo.merge_base(merge_base, current).ok() == Some(merge_base)
                        && merge_base != current
                    {
                        earliest_base = Some(merge_base);
                    }
                }
            }
        }
    }

    // Use the GRANDPARENT of the earliest merge-base so both the merge-base and its parent
    // are included in main's steps. This avoids showing an orphan BASE node.
    if let Some(base_oid) = earliest_base
        && let Ok(base_commit) = repo.find_commit(base_oid)
        && base_commit.parent_count() > 0
        && let Ok(parent) = base_commit.parent(0)
    {
        // Try to get grandparent
        if parent.parent_count() > 0
            && let Ok(grandparent) = parent.parent(0)
        {
            return Ok(Some(grandparent.id().to_string()));
        }
        // Fall back to parent if no grandparent
        return Ok(Some(parent.id().to_string()));
    }

    Ok(earliest_base.map(|oid| oid.to_string()))
}

fn find_base_for_branch(repo: &Repository, branch_commit: &Commit) -> Result<Oid> {
    // Try to find merge-base with default branch, but only if the branch
    // being derived is *not* the default branch itself (merge-base of a
    // branch with itself is its own tip, which yields zero commits).
    if let Some(default_branch) = find_default_branch(repo)
        && let Ok(default_ref) = repo.find_branch(&default_branch, git2::BranchType::Local)
        && let Ok(default_commit) = default_ref.get().peel_to_commit()
        && default_commit.id() != branch_commit.id()
        && let Ok(merge_base) = repo.merge_base(default_commit.id(), branch_commit.id())
        && merge_base != branch_commit.id()
    {
        return Ok(merge_base);
    }

    // Fall back to first commit in history (root of the branch)
    let mut walker = repo.revwalk()?;
    walker.push(branch_commit.id())?;
    walker.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)?;

    if let Some(Ok(oid)) = walker.next() {
        return Ok(oid);
    }

    Ok(branch_commit.id())
}

fn find_default_branch(repo: &Repository) -> Option<String> {
    // Try common default branch names
    for name in &["main", "master", "trunk", "develop"] {
        if repo.find_branch(name, git2::BranchType::Local).is_ok() {
            return Some(name.to_string());
        }
    }
    None
}

fn collect_commits<'a>(
    repo: &'a Repository,
    base_oid: Oid,
    head_oid: Oid,
) -> Result<Vec<Commit<'a>>> {
    let mut walker = repo.revwalk()?;
    walker.push(head_oid)?;
    walker.hide(base_oid)?;
    walker.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)?;

    let mut commits = Vec::new();
    for oid_result in walker {
        let oid = oid_result?;
        let commit = repo.find_commit(oid)?;
        commits.push(commit);
    }

    Ok(commits)
}

fn generate_steps(
    repo: &Repository,
    commits: &[Commit],
    base_oid: Oid,
    actors: &mut HashMap<String, ActorDefinition>,
) -> Result<Vec<Step>> {
    let mut steps = Vec::new();

    for commit in commits {
        let step = commit_to_step(repo, commit, base_oid, actors)?;
        steps.push(step);
    }

    Ok(steps)
}

fn commit_to_step(
    repo: &Repository,
    commit: &Commit,
    base_oid: Oid,
    actors: &mut HashMap<String, ActorDefinition>,
) -> Result<Step> {
    let step_id = format!("step-{}", short_oid(commit.id()));

    // Filter parents to only include those that aren't the base commit
    let parents: Vec<String> = commit
        .parent_ids()
        .filter(|pid| *pid != base_oid)
        .map(|pid| format!("step-{}", short_oid(pid)))
        .collect();

    // Get author info
    let author = commit.author();
    let author_name = author.name().unwrap_or("unknown");
    let author_email = author.email().unwrap_or("unknown");
    let actor = format!("human:{}", slugify_author(author_name, author_email));

    // Register actor definition
    actors.entry(actor.clone()).or_insert_with(|| {
        let mut identities = Vec::new();
        if author_email != "unknown" {
            identities.push(Identity {
                system: "email".to_string(),
                id: author_email.to_string(),
            });
        }
        ActorDefinition {
            name: Some(author_name.to_string()),
            identities,
            ..Default::default()
        }
    });

    // Get timestamp
    let time = commit.time();
    let timestamp = DateTime::<Utc>::from_timestamp(time.seconds(), 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

    // Generate diff
    let change = generate_diff(repo, commit)?;

    // Get commit message as intent
    let message = commit.message().unwrap_or("").trim();
    let intent = if message.is_empty() {
        None
    } else {
        // Use first line of commit message
        Some(message.lines().next().unwrap_or(message).to_string())
    };

    // VCS source reference
    let source = VcsSource {
        vcs_type: "git".to_string(),
        revision: commit.id().to_string(),
        change_id: None,
    };

    Ok(Step {
        step: StepIdentity {
            id: step_id,
            parents,
            actor,
            timestamp,
        },
        change,
        meta: Some(StepMeta {
            intent,
            source: Some(source),
            ..Default::default()
        }),
    })
}

fn generate_diff(repo: &Repository, commit: &Commit) -> Result<HashMap<String, ArtifactChange>> {
    let tree = commit.tree()?;

    let parent_tree = if commit.parent_count() > 0 {
        Some(commit.parent(0)?.tree()?)
    } else {
        None
    };

    let mut diff_opts = DiffOptions::new();
    diff_opts.context_lines(3);

    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut diff_opts))?;

    let mut changes: HashMap<String, ArtifactChange> = HashMap::new();
    let mut current_file: Option<String> = None;
    let mut current_diff = String::new();

    diff.print(git2::DiffFormat::Patch, |delta, _hunk, line| {
        let file_path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().to_string());

        if let Some(path) = file_path {
            // Check if we're starting a new file
            if current_file.as_ref() != Some(&path) {
                // Save previous file's diff
                if let Some(prev_file) = current_file.take()
                    && !current_diff.is_empty()
                {
                    changes.insert(prev_file, ArtifactChange::raw(&current_diff));
                }
                current_file = Some(path);
                current_diff.clear();
            }
        }

        // Append line to current diff
        let prefix = match line.origin() {
            '+' => "+",
            '-' => "-",
            ' ' => " ",
            '>' => ">",
            '<' => "<",
            'F' => "",  // File header
            'H' => "@", // Hunk header - we'll handle this specially
            'B' => "",
            _ => "",
        };

        if line.origin() == 'H' {
            // Hunk header
            if let Ok(content) = std::str::from_utf8(line.content()) {
                current_diff.push_str("@@");
                current_diff.push_str(content.trim_start_matches('@'));
            }
        } else if (!prefix.is_empty() || line.origin() == ' ')
            && let Ok(content) = std::str::from_utf8(line.content())
        {
            current_diff.push_str(prefix);
            current_diff.push_str(content);
        }

        true
    })?;

    // Don't forget the last file
    if let Some(file) = current_file
        && !current_diff.is_empty()
    {
        changes.insert(file, ArtifactChange::raw(&current_diff));
    }

    Ok(changes)
}

fn short_oid(oid: Oid) -> String {
    safe_prefix(&oid.to_string(), 8)
}

/// Return the first `n` characters of a string, safe for any UTF-8 content.
fn safe_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalize_git_url ──────────────────────────────────────────────

    #[test]
    fn test_normalize_github_ssh() {
        assert_eq!(
            normalize_git_url("git@github.com:org/repo.git"),
            "github:org/repo"
        );
    }

    #[test]
    fn test_normalize_github_https() {
        assert_eq!(
            normalize_git_url("https://github.com/org/repo.git"),
            "github:org/repo"
        );
    }

    #[test]
    fn test_normalize_github_https_no_suffix() {
        assert_eq!(
            normalize_git_url("https://github.com/org/repo"),
            "github:org/repo"
        );
    }

    #[test]
    fn test_normalize_gitlab_ssh() {
        assert_eq!(
            normalize_git_url("git@gitlab.com:org/repo.git"),
            "gitlab:org/repo"
        );
    }

    #[test]
    fn test_normalize_gitlab_https() {
        assert_eq!(
            normalize_git_url("https://gitlab.com/org/repo.git"),
            "gitlab:org/repo"
        );
    }

    #[test]
    fn test_normalize_unknown_url_passthrough() {
        let url = "https://bitbucket.org/org/repo.git";
        assert_eq!(normalize_git_url(url), url);
    }

    // ── slugify_author ─────────────────────────────────────────────────

    #[test]
    fn test_slugify_prefers_email_username() {
        assert_eq!(slugify_author("Alex Smith", "asmith@example.com"), "asmith");
    }

    #[test]
    fn test_slugify_falls_back_to_name() {
        assert_eq!(slugify_author("Alex Smith", "unknown"), "alex-smith");
    }

    #[test]
    fn test_slugify_lowercases() {
        assert_eq!(slugify_author("Alex", "Alex@example.com"), "alex");
    }

    #[test]
    fn test_slugify_replaces_special_chars() {
        assert_eq!(slugify_author("A.B", "a.b@example.com"), "a-b");
    }

    #[test]
    fn test_slugify_empty_email_username() {
        // email with no @ — the split returns the full string, same as email
        assert_eq!(slugify_author("Test User", "noreply"), "test-user");
    }

    // ── BranchSpec::parse ──────────────────────────────────────────────

    #[test]
    fn test_branch_spec_simple() {
        let spec = BranchSpec::parse("main");
        assert_eq!(spec.name, "main");
        assert!(spec.start.is_none());
    }

    #[test]
    fn test_branch_spec_with_start() {
        let spec = BranchSpec::parse("feature:HEAD~5");
        assert_eq!(spec.name, "feature");
        assert_eq!(spec.start.as_deref(), Some("HEAD~5"));
    }

    #[test]
    fn test_branch_spec_with_commit_start() {
        let spec = BranchSpec::parse("main:abc1234");
        assert_eq!(spec.name, "main");
        assert_eq!(spec.start.as_deref(), Some("abc1234"));
    }

    // ── safe_prefix / short_oid ────────────────────────────────────────

    #[test]
    fn test_safe_prefix_ascii() {
        assert_eq!(safe_prefix("abcdef12345", 8), "abcdef12");
    }

    #[test]
    fn test_safe_prefix_short_string() {
        assert_eq!(safe_prefix("abc", 8), "abc");
    }

    #[test]
    fn test_safe_prefix_empty() {
        assert_eq!(safe_prefix("", 8), "");
    }

    #[test]
    fn test_safe_prefix_multibyte() {
        // Ensure we don't panic on multi-byte chars
        assert_eq!(safe_prefix("café", 3), "caf");
        assert_eq!(safe_prefix("日本語テスト", 3), "日本語");
    }

    #[test]
    fn test_short_oid() {
        let oid = Oid::from_str("abcdef1234567890abcdef1234567890abcdef12").unwrap();
        assert_eq!(short_oid(oid), "abcdef12");
    }

    // ── DeriveConfig default ───────────────────────────────────────────

    #[test]
    fn test_derive_config_fields() {
        let config = DeriveConfig {
            remote: "origin".to_string(),
            title: Some("My Graph".to_string()),
            base: None,
        };
        assert_eq!(config.remote, "origin");
        assert_eq!(config.title.as_deref(), Some("My Graph"));
        assert!(config.base.is_none());
    }

    // ── Integration tests with temp git repo ───────────────────────────

    fn init_temp_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Configure author for commits
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();

        (dir, repo)
    }

    fn create_commit(
        repo: &Repository,
        message: &str,
        file_name: &str,
        content: &str,
        parent: Option<&git2::Commit>,
    ) -> Oid {
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
    fn test_list_branches_on_repo() {
        let (_dir, repo) = init_temp_repo();
        // Create initial commit so a branch exists
        create_commit(&repo, "initial", "file.txt", "hello", None);

        let branches = list_branches(&repo).unwrap();
        assert!(!branches.is_empty());
        // Should contain "main" or "master" depending on git config
        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        assert!(
            names.contains(&"main") || names.contains(&"master"),
            "Expected main or master in {:?}",
            names
        );
    }

    #[test]
    fn test_list_branches_sorted() {
        let (_dir, repo) = init_temp_repo();
        let oid = create_commit(&repo, "initial", "file.txt", "hello", None);
        let commit = repo.find_commit(oid).unwrap();

        // Create additional branches
        repo.branch("b-beta", &commit, false).unwrap();
        repo.branch("a-alpha", &commit, false).unwrap();

        let branches = list_branches(&repo).unwrap();
        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        // Should be sorted alphabetically
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn test_get_repo_uri_no_remote() {
        let (_dir, repo) = init_temp_repo();
        let uri = get_repo_uri(&repo, "origin").unwrap();
        assert!(
            uri.starts_with("file://"),
            "Expected file:// URI, got {}",
            uri
        );
    }

    #[test]
    fn test_derive_single_branch() {
        let (_dir, repo) = init_temp_repo();
        let oid1 = create_commit(&repo, "first commit", "file.txt", "v1", None);
        let commit1 = repo.find_commit(oid1).unwrap();
        create_commit(&repo, "second commit", "file.txt", "v2", Some(&commit1));

        let config = DeriveConfig {
            remote: "origin".to_string(),
            title: None,
            base: None,
        };

        // Get the default branch name
        let default = find_default_branch(&repo).unwrap_or("main".to_string());
        let result = derive(&repo, &[default], &config).unwrap();

        match result {
            Document::Path(path) => {
                assert!(!path.steps.is_empty(), "Expected at least one step");
                assert!(path.path.base.is_some());
            }
            _ => panic!("Expected Document::Path for single branch"),
        }
    }

    #[test]
    fn test_derive_multiple_branches_produces_graph() {
        let (_dir, repo) = init_temp_repo();
        let oid1 = create_commit(&repo, "initial", "file.txt", "v1", None);
        let commit1 = repo.find_commit(oid1).unwrap();
        let _oid2 = create_commit(&repo, "on default", "file.txt", "v2", Some(&commit1));

        let default_branch = find_default_branch(&repo).unwrap();

        // Create a feature branch from commit1
        repo.branch("feature", &commit1, false).unwrap();
        repo.set_head("refs/heads/feature").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let commit1_again = repo.find_commit(oid1).unwrap();
        create_commit(
            &repo,
            "feature work",
            "feature.txt",
            "feat",
            Some(&commit1_again),
        );

        // Go back to default branch
        repo.set_head(&format!("refs/heads/{}", default_branch))
            .unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();

        let config = DeriveConfig {
            remote: "origin".to_string(),
            title: Some("Test Graph".to_string()),
            base: None,
        };

        let result = derive(&repo, &[default_branch, "feature".to_string()], &config).unwrap();

        match result {
            Document::Graph(graph) => {
                assert_eq!(graph.paths.len(), 2);
                assert!(graph.meta.is_some());
                assert_eq!(graph.meta.unwrap().title.unwrap(), "Test Graph");
            }
            _ => panic!("Expected Document::Graph for multiple branches"),
        }
    }

    #[test]
    fn test_find_default_branch() {
        let (_dir, repo) = init_temp_repo();
        create_commit(&repo, "initial", "file.txt", "hello", None);

        let default = find_default_branch(&repo);
        assert!(default.is_some());
        // git init creates "main" or "master" depending on git config
        let name = default.unwrap();
        assert!(name == "main" || name == "master");
    }

    #[test]
    fn test_branch_info_fields() {
        let (_dir, repo) = init_temp_repo();
        create_commit(&repo, "test subject line", "file.txt", "hello", None);

        let branches = list_branches(&repo).unwrap();
        let branch = &branches[0];

        assert!(!branch.head.is_empty());
        assert_eq!(branch.head_short.len(), 8);
        assert_eq!(branch.subject, "test subject line");
        assert_eq!(branch.author, "Test User");
        assert!(branch.timestamp.ends_with('Z'));
    }

    #[test]
    fn test_derive_with_global_base() {
        let (_dir, repo) = init_temp_repo();
        let oid1 = create_commit(&repo, "first commit", "file.txt", "v1", None);
        let commit1 = repo.find_commit(oid1).unwrap();
        let oid2 = create_commit(&repo, "second commit", "file.txt", "v2", Some(&commit1));
        let commit2 = repo.find_commit(oid2).unwrap();
        create_commit(&repo, "third commit", "file.txt", "v3", Some(&commit2));

        let default = find_default_branch(&repo).unwrap();
        let config = DeriveConfig {
            remote: "origin".to_string(),
            title: None,
            base: Some(oid1.to_string()),
        };

        let result = derive(&repo, &[default], &config).unwrap();
        match result {
            Document::Path(path) => {
                // Should only include commits after oid1
                assert!(path.steps.len() >= 1);
            }
            _ => panic!("Expected Document::Path"),
        }
    }

    #[test]
    fn test_derive_path_with_branch_start() {
        let (_dir, repo) = init_temp_repo();
        let oid1 = create_commit(&repo, "first", "file.txt", "v1", None);
        let commit1 = repo.find_commit(oid1).unwrap();
        let oid2 = create_commit(&repo, "second", "file.txt", "v2", Some(&commit1));
        let commit2 = repo.find_commit(oid2).unwrap();
        create_commit(&repo, "third", "file.txt", "v3", Some(&commit2));

        let default = find_default_branch(&repo).unwrap();
        let spec = BranchSpec {
            name: default,
            start: Some(oid1.to_string()),
        };
        let config = DeriveConfig {
            remote: "origin".to_string(),
            title: None,
            base: None,
        };

        let path = derive_path(&repo, &spec, &config).unwrap();
        assert!(path.steps.len() >= 1);
    }

    #[test]
    fn test_generate_diff_initial_commit() {
        let (_dir, repo) = init_temp_repo();
        let oid = create_commit(&repo, "initial", "file.txt", "hello world", None);
        let commit = repo.find_commit(oid).unwrap();

        let changes = generate_diff(&repo, &commit).unwrap();
        // Initial commit should have a diff for the new file
        assert!(!changes.is_empty());
        assert!(changes.contains_key("file.txt"));
    }

    #[test]
    fn test_collect_commits_range() {
        let (_dir, repo) = init_temp_repo();
        let oid1 = create_commit(&repo, "first", "file.txt", "v1", None);
        let commit1 = repo.find_commit(oid1).unwrap();
        let oid2 = create_commit(&repo, "second", "file.txt", "v2", Some(&commit1));
        let commit2 = repo.find_commit(oid2).unwrap();
        let oid3 = create_commit(&repo, "third", "file.txt", "v3", Some(&commit2));

        let commits = collect_commits(&repo, oid1, oid3).unwrap();
        assert_eq!(commits.len(), 2); // second and third, not first
    }

    #[test]
    fn test_graph_id_many_branches() {
        let (_dir, repo) = init_temp_repo();
        let oid1 = create_commit(&repo, "initial", "file.txt", "v1", None);
        let commit1 = repo.find_commit(oid1).unwrap();

        // Create 4 branches
        repo.branch("b1", &commit1, false).unwrap();
        repo.branch("b2", &commit1, false).unwrap();
        repo.branch("b3", &commit1, false).unwrap();
        repo.branch("b4", &commit1, false).unwrap();

        let config = DeriveConfig {
            remote: "origin".to_string(),
            title: None,
            base: Some(oid1.to_string()),
        };

        let result = derive(
            &repo,
            &[
                "b1".to_string(),
                "b2".to_string(),
                "b3".to_string(),
                "b4".to_string(),
            ],
            &config,
        )
        .unwrap();

        match result {
            Document::Graph(g) => {
                assert!(g.graph.id.contains("4-branches"));
            }
            _ => panic!("Expected Graph"),
        }
    }

    #[test]
    fn test_commit_to_step_creates_actor() {
        let (_dir, repo) = init_temp_repo();
        let oid = create_commit(&repo, "a commit", "file.txt", "content", None);
        let commit = repo.find_commit(oid).unwrap();

        let mut actors = HashMap::new();
        let step = commit_to_step(&repo, &commit, Oid::zero(), &mut actors).unwrap();

        assert!(step.step.actor.starts_with("human:"));
        assert!(!actors.is_empty());
        let actor_def = actors.values().next().unwrap();
        assert_eq!(actor_def.name.as_deref(), Some("Test User"));
    }
}
