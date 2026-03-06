#![doc = include_str!("../README.md")]

// ============================================================================
// Public configuration and types (available on all targets)
// ============================================================================

/// Configuration for deriving Toolpath documents from a GitHub pull request.
pub struct DeriveConfig {
    /// GitHub API token.
    pub token: String,
    /// GitHub API base URL (default: `https://api.github.com`).
    pub api_url: String,
    /// Include CI check runs as Steps (default: true).
    pub include_ci: bool,
    /// Include reviews and comments as Steps (default: true).
    pub include_comments: bool,
}

impl Default for DeriveConfig {
    fn default() -> Self {
        Self {
            token: String::new(),
            api_url: "https://api.github.com".to_string(),
            include_ci: true,
            include_comments: true,
        }
    }
}

/// Summary information about a pull request.
#[derive(Debug, Clone)]
pub struct PullRequestInfo {
    /// PR number.
    pub number: u64,
    /// PR title.
    pub title: String,
    /// PR state (open, closed, merged).
    pub state: String,
    /// PR author login.
    pub author: String,
    /// Head branch name.
    pub head_branch: String,
    /// Base branch name.
    pub base_branch: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 last update timestamp.
    pub updated_at: String,
}

// ============================================================================
// Public pure-data helpers (available on all targets)
// ============================================================================

/// Parsed components of a GitHub PR URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrUrl {
    /// Repository owner.
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// Pull request number.
    pub number: u64,
}

/// Parse a GitHub PR URL into its components.
///
/// Accepts URLs like `https://github.com/owner/repo/pull/42` or
/// `github.com/owner/repo/pull/42` (without protocol prefix).
/// Returns `None` if the URL doesn't match the expected format.
///
/// # Examples
///
/// ```
/// use toolpath_github::parse_pr_url;
///
/// let pr = parse_pr_url("https://github.com/empathic/toolpath/pull/6").unwrap();
/// assert_eq!(pr.owner, "empathic");
/// assert_eq!(pr.repo, "toolpath");
/// assert_eq!(pr.number, 6);
///
/// // Works without protocol prefix too
/// let pr = parse_pr_url("github.com/empathic/toolpath/pull/6").unwrap();
/// assert_eq!(pr.number, 6);
///
/// assert!(parse_pr_url("not a url").is_none());
/// ```
pub fn parse_pr_url(url: &str) -> Option<PrUrl> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("github.com/"))?;
    let parts: Vec<&str> = rest.splitn(4, '/').collect();
    if parts.len() >= 4 && parts[2] == "pull" {
        let number = parts[3].split(&['/', '?', '#'][..]).next()?.parse().ok()?;
        Some(PrUrl {
            owner: parts[0].to_string(),
            repo: parts[1].to_string(),
            number,
        })
    } else {
        None
    }
}

/// Extract issue references from PR body text.
///
/// Recognizes "Fixes #N", "Closes #N", "Resolves #N" (case-insensitive).
///
/// # Examples
///
/// ```
/// use toolpath_github::extract_issue_refs;
///
/// let refs = extract_issue_refs("This PR fixes #42 and closes #99.");
/// assert_eq!(refs, vec![42, 99]);
/// ```
pub fn extract_issue_refs(body: &str) -> Vec<u64> {
    let mut refs = Vec::new();
    let lower = body.to_lowercase();
    for keyword in &["fixes", "closes", "resolves"] {
        let mut search_from = 0;
        while let Some(pos) = lower[search_from..].find(keyword) {
            let after = search_from + pos + keyword.len();
            // Skip optional whitespace and '#'
            let rest = &body[after..];
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('#') {
                let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = num_str.parse::<u64>()
                    && !refs.contains(&n)
                {
                    refs.push(n);
                }
            }
            search_from = after;
        }
    }
    refs
}

// ============================================================================
// reqwest-dependent code (native targets only)
// ============================================================================

#[cfg(not(target_os = "emscripten"))]
mod native {
    use anyhow::{Context, Result, bail};
    use std::collections::HashMap;
    use toolpath::v1::{
        ActorDefinition, ArtifactChange, Base, Identity, Path, PathIdentity, PathMeta, Ref, Step,
        StepIdentity, StepMeta, StructuralChange,
    };

    use super::{DeriveConfig, PullRequestInfo, extract_issue_refs};

    // ====================================================================
    // Auth
    // ====================================================================

    /// Resolve a GitHub API token.
    ///
    /// Checks `GITHUB_TOKEN` environment variable first, then falls back to
    /// `gh auth token` subprocess. Returns an error if neither works.
    pub fn resolve_token() -> Result<String> {
        if let Ok(token) = std::env::var("GITHUB_TOKEN")
            && !token.is_empty()
        {
            return Ok(token);
        }

        let output = std::process::Command::new("gh")
            .args(["auth", "token"])
            .output()
            .context(
                "Failed to run 'gh auth token'. Set GITHUB_TOKEN or install the GitHub CLI (gh).",
            )?;

        if output.status.success() {
            let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !token.is_empty() {
                return Ok(token);
            }
        }

        bail!(
            "No GitHub token found. Set GITHUB_TOKEN environment variable \
             or authenticate with 'gh auth login'."
        )
    }

    // ====================================================================
    // API Client
    // ====================================================================

    struct GitHubClient {
        client: reqwest::blocking::Client,
        token: String,
        base_url: String,
    }

    impl GitHubClient {
        fn new(config: &DeriveConfig) -> Result<Self> {
            let client = reqwest::blocking::Client::builder()
                .user_agent("toolpath-github")
                .build()
                .context("Failed to build HTTP client")?;

            Ok(Self {
                client,
                token: config.token.clone(),
                base_url: config.api_url.clone(),
            })
        }

        fn get_json(&self, endpoint: &str) -> Result<serde_json::Value> {
            let url = format!("{}{}", self.base_url, endpoint);
            let resp = self
                .client
                .get(&url)
                .header("Authorization", format!("Bearer {}", self.token))
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .send()
                .with_context(|| format!("Request failed: GET {}", url))?;

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().unwrap_or_default();
                bail!("GitHub API error {}: {}", status, body);
            }

            resp.json::<serde_json::Value>()
                .with_context(|| format!("Failed to parse JSON from {}", url))
        }

        fn get_paginated(&self, endpoint: &str) -> Result<Vec<serde_json::Value>> {
            let mut all = Vec::new();
            let mut url = format!("{}{}?per_page=100", self.base_url, endpoint);

            loop {
                let resp = self
                    .client
                    .get(&url)
                    .header("Authorization", format!("Bearer {}", self.token))
                    .header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", "2022-11-28")
                    .send()
                    .with_context(|| format!("Request failed: GET {}", url))?;

                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().unwrap_or_default();
                    bail!("GitHub API error {}: {}", status, body);
                }

                // Parse Link header for next page
                let next_url = resp
                    .headers()
                    .get("link")
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_next_link);

                let page: Vec<serde_json::Value> = resp
                    .json()
                    .with_context(|| format!("Failed to parse JSON from {}", url))?;

                all.extend(page);

                match next_url {
                    Some(next) => url = next,
                    None => break,
                }
            }

            Ok(all)
        }
    }

    fn parse_next_link(header: &str) -> Option<String> {
        for part in header.split(',') {
            let part = part.trim();
            if part.ends_with("rel=\"next\"") {
                // Extract URL between < and >
                if let Some(start) = part.find('<')
                    && let Some(end) = part.find('>')
                {
                    return Some(part[start + 1..end].to_string());
                }
            }
        }
        None
    }

    // ====================================================================
    // Public API
    // ====================================================================

    /// Derive a Toolpath [`Path`] from a GitHub pull request.
    ///
    /// Fetches PR metadata, commits, reviews, comments, and CI checks from the
    /// GitHub API, then maps them into a Toolpath Path document where every
    /// event becomes a Step in the DAG.
    pub fn derive_pull_request(
        owner: &str,
        repo: &str,
        pr_number: u64,
        config: &DeriveConfig,
    ) -> Result<Path> {
        let client = GitHubClient::new(config)?;
        let prefix = format!("/repos/{}/{}", owner, repo);

        // Fetch all data
        let pr = client.get_json(&format!("{}/pulls/{}", prefix, pr_number))?;
        let commits = client.get_paginated(&format!("{}/pulls/{}/commits", prefix, pr_number))?;

        // Fetch full commit details (for file patches)
        let mut commit_details = Vec::new();
        for c in &commits {
            let sha = c["sha"].as_str().unwrap_or_default();
            if !sha.is_empty() {
                let detail = client.get_json(&format!("{}/commits/{}", prefix, sha))?;
                commit_details.push(detail);
            }
        }

        let reviews = if config.include_comments {
            client.get_paginated(&format!("{}/pulls/{}/reviews", prefix, pr_number))?
        } else {
            Vec::new()
        };

        let pr_comments = if config.include_comments {
            client.get_paginated(&format!("{}/issues/{}/comments", prefix, pr_number))?
        } else {
            Vec::new()
        };

        let review_comments = if config.include_comments {
            client.get_paginated(&format!("{}/pulls/{}/comments", prefix, pr_number))?
        } else {
            Vec::new()
        };

        // Fetch CI checks for each commit
        let mut check_runs_by_sha: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
        if config.include_ci {
            for c in &commits {
                let sha = c["sha"].as_str().unwrap_or_default();
                if !sha.is_empty() {
                    let checks =
                        client.get_json(&format!("{}/commits/{}/check-runs", prefix, sha))?;
                    if let Some(runs) = checks["check_runs"].as_array() {
                        check_runs_by_sha.insert(sha.to_string(), runs.clone());
                    }
                }
            }
        }

        let data = PrData {
            pr: &pr,
            commit_details: &commit_details,
            reviews: &reviews,
            pr_comments: &pr_comments,
            review_comments: &review_comments,
            check_runs_by_sha: &check_runs_by_sha,
        };

        derive_from_data(&data, owner, repo, config)
    }

    /// List open pull requests for a repository.
    pub fn list_pull_requests(
        owner: &str,
        repo: &str,
        config: &DeriveConfig,
    ) -> Result<Vec<PullRequestInfo>> {
        let client = GitHubClient::new(config)?;
        let prs = client.get_paginated(&format!("/repos/{}/{}/pulls?state=all", owner, repo))?;

        let mut result = Vec::new();
        for pr in &prs {
            result.push(PullRequestInfo {
                number: pr["number"].as_u64().unwrap_or(0),
                title: str_field(pr, "title"),
                state: str_field(pr, "state"),
                author: pr["user"]["login"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
                head_branch: pr["head"]["ref"].as_str().unwrap_or("unknown").to_string(),
                base_branch: pr["base"]["ref"].as_str().unwrap_or("unknown").to_string(),
                created_at: str_field(pr, "created_at"),
                updated_at: str_field(pr, "updated_at"),
            });
        }

        Ok(result)
    }

    // ====================================================================
    // Pure derivation (testable without network)
    // ====================================================================

    struct PrData<'a> {
        pr: &'a serde_json::Value,
        commit_details: &'a [serde_json::Value],
        reviews: &'a [serde_json::Value],
        pr_comments: &'a [serde_json::Value],
        review_comments: &'a [serde_json::Value],
        check_runs_by_sha: &'a HashMap<String, Vec<serde_json::Value>>,
    }

    fn derive_from_data(
        data: &PrData<'_>,
        owner: &str,
        repo: &str,
        config: &DeriveConfig,
    ) -> Result<Path> {
        let pr = data.pr;
        let commit_details = data.commit_details;
        let reviews = data.reviews;
        let pr_comments = data.pr_comments;
        let review_comments = data.review_comments;
        let check_runs_by_sha = data.check_runs_by_sha;
        let pr_number = pr["number"].as_u64().unwrap_or(0);

        // ── Commit steps ─────────────────────────────────────────────
        let mut steps: Vec<Step> = Vec::new();
        let mut actors: HashMap<String, ActorDefinition> = HashMap::new();

        for detail in commit_details {
            let step = commit_to_step(detail, &mut actors)?;
            steps.push(step);
        }

        // ── Review comment steps ─────────────────────────────────────
        if config.include_comments {
            for rc in review_comments {
                let step = review_comment_to_step(rc, &mut actors)?;
                steps.push(step);
            }

            for pc in pr_comments {
                let step = pr_comment_to_step(pc, &mut actors)?;
                steps.push(step);
            }

            for review in reviews {
                let state = review["state"].as_str().unwrap_or("");
                if state.is_empty() || state == "PENDING" {
                    continue;
                }
                let step = review_to_step(review, &mut actors)?;
                steps.push(step);
            }
        }

        // ── CI check steps ───────────────────────────────────────────
        if config.include_ci {
            for runs in check_runs_by_sha.values() {
                for run in runs {
                    let step = check_run_to_step(run, &mut actors)?;
                    steps.push(step);
                }
            }
        }

        // ── Sort by timestamp, then chain into a single trunk ────────
        // Everything in a PR is part of one timeline. Commits, comments,
        // reviews, and CI checks all chain linearly — none are dead ends
        // or alternate explorations. Sort by time, then re-parent each
        // step to point at the previous one.
        steps.sort_by(|a, b| a.step.timestamp.cmp(&b.step.timestamp));

        let mut prev_id: Option<String> = None;
        for step in &mut steps {
            if let Some(ref prev) = prev_id {
                step.step.parents = vec![prev.clone()];
            } else {
                step.step.parents = vec![];
            }
            prev_id = Some(step.step.id.clone());
        }

        // ── Build path head ──────────────────────────────────────────
        let head = steps
            .last()
            .map(|s| s.step.id.clone())
            .unwrap_or_else(|| format!("pr-{}", pr_number));

        // ── Build path metadata ──────────────────────────────────────
        let meta = build_path_meta(pr, &actors)?;

        Ok(Path {
            path: PathIdentity {
                id: format!("pr-{}", pr_number),
                base: Some(Base {
                    uri: format!("github:{}/{}", owner, repo),
                    ref_str: Some(pr["base"]["ref"].as_str().unwrap_or("main").to_string()),
                }),
                head,
            },
            steps,
            meta: Some(meta),
        })
    }

    // ====================================================================
    // Mapping helpers
    // ====================================================================

    fn commit_to_step(
        detail: &serde_json::Value,
        actors: &mut HashMap<String, ActorDefinition>,
    ) -> Result<Step> {
        let sha = detail["sha"].as_str().unwrap_or_default();
        let short_sha = &sha[..sha.len().min(8)];
        let step_id = format!("step-{}", short_sha);

        // Actor
        let login = detail["author"]["login"].as_str().unwrap_or("unknown");
        let actor = format!("human:{}", login);
        register_actor(actors, &actor, login, None);

        // Timestamp
        let timestamp = detail["commit"]["committer"]["date"]
            .as_str()
            .unwrap_or("1970-01-01T00:00:00Z")
            .to_string();

        // Changes: per-file raw diffs
        let mut change: HashMap<String, ArtifactChange> = HashMap::new();
        if let Some(files) = detail["files"].as_array() {
            for file in files {
                let filename = file["filename"].as_str().unwrap_or("unknown");
                if let Some(patch) = file["patch"].as_str() {
                    change.insert(filename.to_string(), ArtifactChange::raw(patch));
                }
            }
        }

        // Intent: first line of commit message
        let message = detail["commit"]["message"].as_str().unwrap_or("");
        let intent = message.lines().next().unwrap_or("").to_string();

        let mut step = Step {
            step: StepIdentity {
                id: step_id,
                parents: vec![],
                actor,
                timestamp,
            },
            change,
            meta: None,
        };

        if !intent.is_empty() {
            step.meta = Some(StepMeta {
                intent: Some(intent),
                source: Some(toolpath::v1::VcsSource {
                    vcs_type: "git".to_string(),
                    revision: sha.to_string(),
                    change_id: None,
                    extra: HashMap::new(),
                }),
                ..Default::default()
            });
        }

        Ok(step)
    }

    fn review_comment_to_step(
        rc: &serde_json::Value,
        actors: &mut HashMap<String, ActorDefinition>,
    ) -> Result<Step> {
        let id = rc["id"].as_u64().unwrap_or(0);
        let step_id = format!("step-rc-{}", id);

        let login = rc["user"]["login"].as_str().unwrap_or("unknown");
        let actor = format!("human:{}", login);
        register_actor(actors, &actor, login, None);

        let timestamp = rc["created_at"]
            .as_str()
            .unwrap_or("1970-01-01T00:00:00Z")
            .to_string();

        let path = rc["path"].as_str().unwrap_or("unknown");
        let line = rc["line"]
            .as_u64()
            .or_else(|| rc["original_line"].as_u64())
            .unwrap_or(0);
        let artifact_uri = format!("review://{}#L{}", path, line);

        let body = rc["body"].as_str().unwrap_or("").to_string();

        let mut extra = HashMap::new();
        extra.insert("body".to_string(), serde_json::Value::String(body));

        let change = HashMap::from([(
            artifact_uri,
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "review.comment".to_string(),
                    extra,
                }),
            },
        )]);

        Ok(Step {
            step: StepIdentity {
                id: step_id,
                parents: vec![],
                actor,
                timestamp,
            },
            change,
            meta: None,
        })
    }

    fn pr_comment_to_step(
        pc: &serde_json::Value,
        actors: &mut HashMap<String, ActorDefinition>,
    ) -> Result<Step> {
        let id = pc["id"].as_u64().unwrap_or(0);
        let step_id = format!("step-ic-{}", id);

        let timestamp = pc["created_at"]
            .as_str()
            .unwrap_or("1970-01-01T00:00:00Z")
            .to_string();

        let login = pc["user"]["login"].as_str().unwrap_or("unknown");
        let actor = format!("human:{}", login);
        register_actor(actors, &actor, login, None);

        let body = pc["body"].as_str().unwrap_or("").to_string();

        let change = HashMap::from([(
            "review://conversation".to_string(),
            ArtifactChange {
                raw: Some(body),
                structural: None,
            },
        )]);

        Ok(Step {
            step: StepIdentity {
                id: step_id,
                parents: vec![],
                actor,
                timestamp,
            },
            change,
            meta: None,
        })
    }

    fn review_to_step(
        review: &serde_json::Value,
        actors: &mut HashMap<String, ActorDefinition>,
    ) -> Result<Step> {
        let id = review["id"].as_u64().unwrap_or(0);
        let step_id = format!("step-rv-{}", id);

        let timestamp = review["submitted_at"]
            .as_str()
            .unwrap_or("1970-01-01T00:00:00Z")
            .to_string();

        let login = review["user"]["login"].as_str().unwrap_or("unknown");
        let actor = format!("human:{}", login);
        register_actor(actors, &actor, login, None);

        let state = review["state"].as_str().unwrap_or("COMMENTED").to_string();
        let body = review["body"].as_str().unwrap_or("").to_string();

        let mut extra = HashMap::new();
        extra.insert("state".to_string(), serde_json::Value::String(state));

        let change = HashMap::from([(
            "review://decision".to_string(),
            ArtifactChange {
                raw: if body.is_empty() { None } else { Some(body) },
                structural: Some(StructuralChange {
                    change_type: "review.decision".to_string(),
                    extra,
                }),
            },
        )]);

        Ok(Step {
            step: StepIdentity {
                id: step_id,
                parents: vec![],
                actor,
                timestamp,
            },
            change,
            meta: None,
        })
    }

    fn check_run_to_step(
        run: &serde_json::Value,
        actors: &mut HashMap<String, ActorDefinition>,
    ) -> Result<Step> {
        let id = run["id"].as_u64().unwrap_or(0);
        let step_id = format!("step-ci-{}", id);

        let name = run["name"].as_str().unwrap_or("unknown");
        let app_slug = run["app"]["slug"].as_str().unwrap_or("ci");
        let actor = format!("ci:{}", app_slug);

        actors
            .entry(actor.clone())
            .or_insert_with(|| ActorDefinition {
                name: Some(app_slug.to_string()),
                ..Default::default()
            });

        let timestamp = run["completed_at"]
            .as_str()
            .or_else(|| run["started_at"].as_str())
            .unwrap_or("1970-01-01T00:00:00Z")
            .to_string();

        let conclusion = run["conclusion"].as_str().unwrap_or("unknown").to_string();

        let mut extra = HashMap::new();
        extra.insert(
            "conclusion".to_string(),
            serde_json::Value::String(conclusion),
        );

        let artifact_uri = format!("ci://checks/{}", name);
        let change = HashMap::from([(
            artifact_uri,
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "ci.run".to_string(),
                    extra,
                }),
            },
        )]);

        Ok(Step {
            step: StepIdentity {
                id: step_id,
                parents: vec![],
                actor,
                timestamp,
            },
            change,
            meta: None,
        })
    }

    fn build_path_meta(
        pr: &serde_json::Value,
        actors: &HashMap<String, ActorDefinition>,
    ) -> Result<PathMeta> {
        let title = pr["title"].as_str().map(|s| s.to_string());
        let body = pr["body"].as_str().unwrap_or("");
        let intent = if body.is_empty() {
            None
        } else {
            Some(body.to_string())
        };

        // Parse issue refs
        let issue_numbers = extract_issue_refs(body);
        let refs: Vec<Ref> = issue_numbers
            .into_iter()
            .map(|n| {
                let owner = pr["base"]["repo"]["owner"]["login"]
                    .as_str()
                    .unwrap_or("unknown");
                let repo = pr["base"]["repo"]["name"].as_str().unwrap_or("unknown");
                Ref {
                    rel: "fixes".to_string(),
                    href: format!("https://github.com/{}/{}/issues/{}", owner, repo, n),
                }
            })
            .collect();

        // Labels in extra
        let mut extra: HashMap<String, serde_json::Value> = HashMap::new();
        if let Some(labels) = pr["labels"].as_array() {
            let label_names: Vec<serde_json::Value> = labels
                .iter()
                .filter_map(|l| l["name"].as_str())
                .map(|s| serde_json::Value::String(s.to_string()))
                .collect();
            if !label_names.is_empty() {
                let mut github_meta = serde_json::Map::new();
                github_meta.insert("labels".to_string(), serde_json::Value::Array(label_names));
                extra.insert("github".to_string(), serde_json::Value::Object(github_meta));
            }
        }

        Ok(PathMeta {
            title,
            intent,
            refs,
            actors: if actors.is_empty() {
                None
            } else {
                Some(actors.clone())
            },
            extra,
            ..Default::default()
        })
    }

    // ====================================================================
    // Helpers
    // ====================================================================

    fn register_actor(
        actors: &mut HashMap<String, ActorDefinition>,
        actor_key: &str,
        login: &str,
        _email: Option<&str>,
    ) {
        actors
            .entry(actor_key.to_string())
            .or_insert_with(|| ActorDefinition {
                name: Some(login.to_string()),
                identities: vec![Identity {
                    system: "github".to_string(),
                    id: login.to_string(),
                }],
                ..Default::default()
            });
    }

    fn str_field(val: &serde_json::Value, key: &str) -> String {
        val[key].as_str().unwrap_or("").to_string()
    }

    // ====================================================================
    // Tests
    // ====================================================================

    #[cfg(test)]
    mod tests {
        use super::*;

        fn sample_pr() -> serde_json::Value {
            serde_json::json!({
                "number": 42,
                "title": "Add feature X",
                "body": "This PR adds feature X.\n\nFixes #10\nCloses #20",
                "state": "open",
                "user": { "login": "alice" },
                "head": { "ref": "feature-x" },
                "base": {
                    "ref": "main",
                    "repo": {
                        "owner": { "login": "acme" },
                        "name": "widgets"
                    }
                },
                "labels": [
                    { "name": "enhancement" },
                    { "name": "reviewed" }
                ],
                "created_at": "2026-01-15T10:00:00Z",
                "updated_at": "2026-01-16T14:00:00Z"
            })
        }

        fn sample_commit_detail(
            sha: &str,
            parent_sha: Option<&str>,
            msg: &str,
        ) -> serde_json::Value {
            let parents: Vec<serde_json::Value> = parent_sha
                .into_iter()
                .map(|s| serde_json::json!({ "sha": s }))
                .collect();
            serde_json::json!({
                "sha": sha,
                "commit": {
                    "message": msg,
                    "committer": {
                        "date": "2026-01-15T12:00:00Z"
                    }
                },
                "author": { "login": "alice" },
                "parents": parents,
                "files": [
                    {
                        "filename": "src/main.rs",
                        "patch": "@@ -1,3 +1,4 @@\n fn main() {\n+    println!(\"hello\");\n }"
                    }
                ]
            })
        }

        fn sample_review_comment(
            id: u64,
            commit_sha: &str,
            path: &str,
            line: u64,
        ) -> serde_json::Value {
            serde_json::json!({
                "id": id,
                "user": { "login": "bob" },
                "commit_id": commit_sha,
                "path": path,
                "line": line,
                "body": "Consider using a constant here.",
                "created_at": "2026-01-15T14:00:00Z",
                "pull_request_review_id": 100,
                "in_reply_to_id": null
            })
        }

        fn sample_pr_comment(id: u64) -> serde_json::Value {
            serde_json::json!({
                "id": id,
                "user": { "login": "carol" },
                "body": "Looks good overall!",
                "created_at": "2026-01-15T16:00:00Z"
            })
        }

        fn sample_review(id: u64, state: &str) -> serde_json::Value {
            serde_json::json!({
                "id": id,
                "user": { "login": "dave" },
                "state": state,
                "body": "Approved with minor comments.",
                "submitted_at": "2026-01-15T17:00:00Z"
            })
        }

        fn sample_check_run(id: u64, name: &str, conclusion: &str) -> serde_json::Value {
            serde_json::json!({
                "id": id,
                "name": name,
                "app": { "slug": "github-actions" },
                "conclusion": conclusion,
                "completed_at": "2026-01-15T13:00:00Z",
                "started_at": "2026-01-15T12:30:00Z"
            })
        }

        #[test]
        fn test_commit_to_step() {
            let detail = sample_commit_detail("abc12345deadbeef", None, "Initial commit");
            let mut actors = HashMap::new();

            let step = commit_to_step(&detail, &mut actors).unwrap();

            assert_eq!(step.step.id, "step-abc12345");
            assert_eq!(step.step.actor, "human:alice");
            assert!(step.step.parents.is_empty());
            assert!(step.change.contains_key("src/main.rs"));
            assert_eq!(
                step.meta.as_ref().unwrap().intent.as_deref(),
                Some("Initial commit")
            );
            assert!(actors.contains_key("human:alice"));
        }

        #[test]
        fn test_review_comment_to_step() {
            let rc = sample_review_comment(200, "abc12345deadbeef", "src/main.rs", 42);
            let mut actors = HashMap::new();

            let step = review_comment_to_step(&rc, &mut actors).unwrap();

            assert_eq!(step.step.id, "step-rc-200");
            assert_eq!(step.step.actor, "human:bob");
            // Parents are empty — set later by the trunk chain pass
            assert!(step.step.parents.is_empty());
            assert!(step.change.contains_key("review://src/main.rs#L42"));
            assert!(actors.contains_key("human:bob"));
        }

        #[test]
        fn test_pr_comment_to_step() {
            let pc = sample_pr_comment(300);
            let mut actors = HashMap::new();

            let step = pr_comment_to_step(&pc, &mut actors).unwrap();

            assert_eq!(step.step.id, "step-ic-300");
            assert_eq!(step.step.actor, "human:carol");
            assert!(step.step.parents.is_empty());
            assert!(step.change.contains_key("review://conversation"));
            let change = &step.change["review://conversation"];
            assert_eq!(change.raw.as_deref(), Some("Looks good overall!"));
        }

        #[test]
        fn test_review_to_step() {
            let review = sample_review(400, "APPROVED");
            let mut actors = HashMap::new();

            let step = review_to_step(&review, &mut actors).unwrap();

            assert_eq!(step.step.id, "step-rv-400");
            assert_eq!(step.step.actor, "human:dave");
            assert!(step.step.parents.is_empty());
            assert!(step.change.contains_key("review://decision"));
            let change = &step.change["review://decision"];
            assert!(change.structural.is_some());
            let structural = change.structural.as_ref().unwrap();
            assert_eq!(structural.change_type, "review.decision");
            assert_eq!(structural.extra["state"], "APPROVED");
        }

        #[test]
        fn test_check_run_to_step() {
            let run = sample_check_run(500, "build", "success");
            let mut actors = HashMap::new();

            let step = check_run_to_step(&run, &mut actors).unwrap();

            assert_eq!(step.step.id, "step-ci-500");
            assert_eq!(step.step.actor, "ci:github-actions");
            assert!(step.step.parents.is_empty());
            assert!(step.change.contains_key("ci://checks/build"));
            let change = &step.change["ci://checks/build"];
            let structural = change.structural.as_ref().unwrap();
            assert_eq!(structural.change_type, "ci.run");
            assert_eq!(structural.extra["conclusion"], "success");
        }

        #[test]
        fn test_build_path_meta() {
            let pr = sample_pr();
            let mut actors = HashMap::new();
            register_actor(&mut actors, "human:alice", "alice", None);

            let meta = build_path_meta(&pr, &actors).unwrap();

            assert_eq!(meta.title.as_deref(), Some("Add feature X"));
            assert!(meta.intent.as_deref().unwrap().contains("feature X"));
            assert_eq!(meta.refs.len(), 2);
            assert_eq!(meta.refs[0].rel, "fixes");
            assert!(meta.refs[0].href.contains("/issues/10"));
            assert!(meta.refs[1].href.contains("/issues/20"));
            assert!(meta.actors.is_some());

            // Labels in extra
            let github = meta.extra.get("github").unwrap();
            let labels = github["labels"].as_array().unwrap();
            assert_eq!(labels.len(), 2);
        }

        #[test]
        fn test_derive_from_data_full() {
            let pr = sample_pr();
            let commit1 = sample_commit_detail("abc12345deadbeef", None, "Initial commit");
            let commit2 =
                sample_commit_detail("def67890cafebabe", Some("abc12345deadbeef"), "Add tests");
            // Fix second commit timestamp to be after first
            let mut commit2 = commit2;
            commit2["commit"]["committer"]["date"] = serde_json::json!("2026-01-15T13:00:00Z");

            let review_comments = vec![sample_review_comment(
                200,
                "abc12345deadbeef",
                "src/main.rs",
                42,
            )];
            let pr_comments = vec![sample_pr_comment(300)];
            let reviews = vec![sample_review(400, "APPROVED")];

            let mut check_runs = HashMap::new();
            check_runs.insert(
                "abc12345deadbeef".to_string(),
                vec![sample_check_run(500, "build", "success")],
            );

            let config = DeriveConfig {
                token: "test".to_string(),
                api_url: "https://api.github.com".to_string(),
                include_ci: true,
                include_comments: true,
            };

            let data = PrData {
                pr: &pr,
                commit_details: &[commit1, commit2],
                reviews: &reviews,
                pr_comments: &pr_comments,
                review_comments: &review_comments,
                check_runs_by_sha: &check_runs,
            };
            let path = derive_from_data(&data, "acme", "widgets", &config).unwrap();

            assert_eq!(path.path.id, "pr-42");
            assert_eq!(path.path.base.as_ref().unwrap().uri, "github:acme/widgets");
            assert_eq!(
                path.path.base.as_ref().unwrap().ref_str.as_deref(),
                Some("main")
            );

            // Should have 2 commits + 1 review comment + 1 PR comment + 1 review + 1 CI = 6 steps
            assert_eq!(path.steps.len(), 6);

            // All steps form a single trunk chain sorted by timestamp
            assert!(path.steps[0].step.parents.is_empty());
            for i in 1..path.steps.len() {
                assert!(
                    path.steps[i].step.timestamp >= path.steps[i - 1].step.timestamp,
                    "Steps not sorted: {} < {}",
                    path.steps[i].step.timestamp,
                    path.steps[i - 1].step.timestamp,
                );
                assert_eq!(
                    path.steps[i].step.parents,
                    vec![path.steps[i - 1].step.id.clone()],
                    "Step {} should parent off step {}",
                    path.steps[i].step.id,
                    path.steps[i - 1].step.id,
                );
            }

            // Path meta
            let meta = path.meta.as_ref().unwrap();
            assert_eq!(meta.title.as_deref(), Some("Add feature X"));
            assert_eq!(meta.refs.len(), 2);
        }

        #[test]
        fn test_derive_from_data_no_ci() {
            let pr = sample_pr();
            let commit = sample_commit_detail("abc12345deadbeef", None, "Commit");

            let config = DeriveConfig {
                token: "test".to_string(),
                api_url: "https://api.github.com".to_string(),
                include_ci: false,
                include_comments: false,
            };

            let data = PrData {
                pr: &pr,
                commit_details: &[commit],
                reviews: &[],
                pr_comments: &[],
                review_comments: &[],
                check_runs_by_sha: &HashMap::new(),
            };
            let path = derive_from_data(&data, "acme", "widgets", &config).unwrap();

            // Only commit steps
            assert_eq!(path.steps.len(), 1);
            assert_eq!(path.steps[0].step.id, "step-abc12345");
        }

        #[test]
        fn test_derive_from_data_pending_review_skipped() {
            let pr = sample_pr();
            let commit = sample_commit_detail("abc12345deadbeef", None, "Commit");
            let pending_review = sample_review(999, "PENDING");

            let config = DeriveConfig {
                token: "test".to_string(),
                api_url: "https://api.github.com".to_string(),
                include_ci: false,
                include_comments: true,
            };

            let data = PrData {
                pr: &pr,
                commit_details: &[commit],
                reviews: &[pending_review],
                pr_comments: &[],
                review_comments: &[],
                check_runs_by_sha: &HashMap::new(),
            };
            let path = derive_from_data(&data, "acme", "widgets", &config).unwrap();

            // Only commit step, pending review skipped
            assert_eq!(path.steps.len(), 1);
        }

        #[test]
        fn test_parse_next_link() {
            let header = r#"<https://api.github.com/repos/foo/bar/pulls?page=2>; rel="next", <https://api.github.com/repos/foo/bar/pulls?page=5>; rel="last""#;
            assert_eq!(
                parse_next_link(header),
                Some("https://api.github.com/repos/foo/bar/pulls?page=2".to_string())
            );

            assert_eq!(
                parse_next_link(r#"<https://example.com>; rel="prev""#),
                None
            );
        }

        #[test]
        fn test_str_field() {
            let val = serde_json::json!({"name": "hello", "missing": null});
            assert_eq!(str_field(&val, "name"), "hello");
            assert_eq!(str_field(&val, "missing"), "");
            assert_eq!(str_field(&val, "nonexistent"), "");
        }

        #[test]
        fn test_register_actor_idempotent() {
            let mut actors = HashMap::new();
            register_actor(&mut actors, "human:alice", "alice", None);
            register_actor(&mut actors, "human:alice", "alice", None);
            assert_eq!(actors.len(), 1);
        }

        #[test]
        fn test_ci_steps_chain_inline() {
            let pr = sample_pr();
            let commit = sample_commit_detail("abc12345deadbeef", None, "Commit");

            let mut check_runs = HashMap::new();
            check_runs.insert(
                "abc12345deadbeef".to_string(),
                vec![
                    sample_check_run(501, "build", "success"),
                    sample_check_run(502, "test", "success"),
                    sample_check_run(503, "lint", "success"),
                ],
            );

            let config = DeriveConfig {
                token: "test".to_string(),
                api_url: "https://api.github.com".to_string(),
                include_ci: true,
                include_comments: false,
            };

            let data = PrData {
                pr: &pr,
                commit_details: &[commit],
                reviews: &[],
                pr_comments: &[],
                review_comments: &[],
                check_runs_by_sha: &check_runs,
            };
            let path = derive_from_data(&data, "acme", "widgets", &config).unwrap();

            // 1 commit + 3 CI steps = 4 steps on a single trunk
            assert_eq!(path.steps.len(), 4);

            // All steps chain linearly by timestamp
            assert!(path.steps[0].step.parents.is_empty()); // first step: no parent
            for i in 1..path.steps.len() {
                assert_eq!(
                    path.steps[i].step.parents,
                    vec![path.steps[i - 1].step.id.clone()]
                );
            }
        }

        #[test]
        fn test_review_comment_artifact_uri_format() {
            let rc = sample_review_comment(700, "abc12345", "src/lib.rs", 100);
            let mut actors = HashMap::new();

            let step = review_comment_to_step(&rc, &mut actors).unwrap();

            assert!(step.change.contains_key("review://src/lib.rs#L100"));
        }

        #[test]
        fn test_derive_from_data_empty_commits() {
            let pr = sample_pr();
            let config = DeriveConfig {
                token: "test".to_string(),
                api_url: "https://api.github.com".to_string(),
                include_ci: false,
                include_comments: false,
            };

            let data = PrData {
                pr: &pr,
                commit_details: &[],
                reviews: &[],
                pr_comments: &[],
                review_comments: &[],
                check_runs_by_sha: &HashMap::new(),
            };
            let path = derive_from_data(&data, "acme", "widgets", &config).unwrap();

            assert_eq!(path.path.id, "pr-42");
            assert!(path.steps.is_empty());
            assert_eq!(path.path.head, "pr-42");
        }

        #[test]
        fn test_review_empty_body() {
            let mut review = sample_review(800, "APPROVED");
            review["body"] = serde_json::json!("");
            let mut actors = HashMap::new();

            let step = review_to_step(&review, &mut actors).unwrap();
            let change = &step.change["review://decision"];
            assert!(change.raw.is_none());
            assert!(change.structural.is_some());
        }

        #[test]
        fn test_commit_no_files() {
            let detail = serde_json::json!({
                "sha": "aabbccdd11223344",
                "commit": {
                    "message": "Empty commit",
                    "committer": { "date": "2026-01-15T12:00:00Z" }
                },
                "author": { "login": "alice" },
                "parents": [],
                "files": []
            });
            let mut actors = HashMap::new();

            let step = commit_to_step(&detail, &mut actors).unwrap();
            assert!(step.change.is_empty());
        }

        #[test]
        fn test_multiple_commits_chain() {
            let pr = sample_pr();
            let c1 = {
                let mut c = sample_commit_detail("1111111100000000", None, "First");
                c["commit"]["committer"]["date"] = serde_json::json!("2026-01-15T10:00:00Z");
                c
            };
            let c2 = {
                let mut c =
                    sample_commit_detail("2222222200000000", Some("1111111100000000"), "Second");
                c["commit"]["committer"]["date"] = serde_json::json!("2026-01-15T11:00:00Z");
                c
            };
            let c3 = {
                let mut c =
                    sample_commit_detail("3333333300000000", Some("2222222200000000"), "Third");
                c["commit"]["committer"]["date"] = serde_json::json!("2026-01-15T12:00:00Z");
                c
            };

            let config = DeriveConfig {
                token: "test".to_string(),
                api_url: "https://api.github.com".to_string(),
                include_ci: false,
                include_comments: false,
            };

            let data = PrData {
                pr: &pr,
                commit_details: &[c1, c2, c3],
                reviews: &[],
                pr_comments: &[],
                review_comments: &[],
                check_runs_by_sha: &HashMap::new(),
            };
            let path = derive_from_data(&data, "acme", "widgets", &config).unwrap();

            // Trunk chain: each step parents off the previous by timestamp
            assert_eq!(path.steps.len(), 3);
            assert!(path.steps[0].step.parents.is_empty());
            assert_eq!(path.steps[1].step.parents, vec!["step-11111111"]);
            assert_eq!(path.steps[2].step.parents, vec!["step-22222222"]);
            assert_eq!(path.path.head, "step-33333333");
        }
    }
}

// Re-export native-only functions at crate root for API compatibility
#[cfg(not(target_os = "emscripten"))]
pub use native::{derive_pull_request, list_pull_requests, resolve_token};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_issue_refs_basic() {
        let refs = extract_issue_refs("Fixes #42");
        assert_eq!(refs, vec![42]);
    }

    #[test]
    fn test_extract_issue_refs_multiple() {
        let refs = extract_issue_refs("Fixes #10 and Closes #20");
        assert_eq!(refs, vec![10, 20]);
    }

    #[test]
    fn test_extract_issue_refs_case_insensitive() {
        let refs = extract_issue_refs("FIXES #1, closes #2, Resolves #3");
        assert_eq!(refs, vec![1, 2, 3]);
    }

    #[test]
    fn test_extract_issue_refs_no_refs() {
        let refs = extract_issue_refs("Just a regular PR description.");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_extract_issue_refs_dedup() {
        let refs = extract_issue_refs("Fixes #5 and also fixes #5");
        assert_eq!(refs, vec![5]);
    }

    #[test]
    fn test_extract_issue_refs_multiline() {
        let body = "This is a PR.\n\nFixes #100\nCloses #200\n\nSome more text.";
        let refs = extract_issue_refs(body);
        assert_eq!(refs, vec![100, 200]);
    }

    #[test]
    fn test_derive_config_default() {
        let config = DeriveConfig::default();
        assert_eq!(config.api_url, "https://api.github.com");
        assert!(config.include_ci);
        assert!(config.include_comments);
        assert!(config.token.is_empty());
    }

    #[test]
    fn test_parse_pr_url_https() {
        let pr = parse_pr_url("https://github.com/empathic/toolpath/pull/6").unwrap();
        assert_eq!(pr.owner, "empathic");
        assert_eq!(pr.repo, "toolpath");
        assert_eq!(pr.number, 6);
    }

    #[test]
    fn test_parse_pr_url_no_protocol() {
        let pr = parse_pr_url("github.com/empathic/toolpath/pull/42").unwrap();
        assert_eq!(pr.owner, "empathic");
        assert_eq!(pr.repo, "toolpath");
        assert_eq!(pr.number, 42);
    }

    #[test]
    fn test_parse_pr_url_http() {
        let pr = parse_pr_url("http://github.com/org/repo/pull/1").unwrap();
        assert_eq!(pr.owner, "org");
        assert_eq!(pr.repo, "repo");
        assert_eq!(pr.number, 1);
    }

    #[test]
    fn test_parse_pr_url_with_trailing_parts() {
        let pr = parse_pr_url("https://github.com/org/repo/pull/99/files").unwrap();
        assert_eq!(pr.number, 99);
    }

    #[test]
    fn test_parse_pr_url_with_query_string() {
        let pr = parse_pr_url("https://github.com/org/repo/pull/5?diff=unified").unwrap();
        assert_eq!(pr.number, 5);
    }

    #[test]
    fn test_parse_pr_url_invalid() {
        assert!(parse_pr_url("not a url").is_none());
        assert!(parse_pr_url("https://github.com/org/repo").is_none());
        assert!(parse_pr_url("https://github.com/org/repo/issues/1").is_none());
        assert!(parse_pr_url("https://gitlab.com/org/repo/pull/1").is_none());
    }
}
