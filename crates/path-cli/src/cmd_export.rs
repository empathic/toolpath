//! `path export <target>` — emit toolpath documents into external formats.
//!
//! - `export claude` projects a Path document into Claude Code's JSONL
//!   format (either into `~/.claude/projects/<sanitized>/<session>.jsonl`
//!   for a resumable session, or to a file / stdout).
//! - `export gemini` projects a Path document into Gemini CLI's chat-file
//!   layout under `~/.gemini/tmp/<slot>/chats/`, ready for
//!   `gemini --resume <uuid>`.
//! - `export pathbase` uploads the document to a Pathbase server.

use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::PathBuf;

use crate::cmd_cache::cache_ref;

#[derive(Subcommand, Debug)]
pub enum ExportTarget {
    /// Project a toolpath document into a Claude Code session
    Claude {
        /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target project directory. With this flag, writes the JSONL into
        /// `~/.claude/projects/<sanitized>/<session>.jsonl` so `claude -r <id>`
        /// can resume it. Defaults to cwd when no `--output` is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output JSONL to this file. Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Project a toolpath document into a Gemini CLI session
    Gemini {
        /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target project directory. With this flag, writes the chat
        /// files into `~/.gemini/tmp/<slot>/chats/` so `gemini --resume
        /// <uuid>` from the same directory can resume it. Defaults to
        /// cwd when neither --project nor --output is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output the main chat file to this path. Sub-agents (if any)
        /// land in a sibling `<session-uuid>/` directory next to it.
        /// Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Upload a toolpath document to Pathbase.
    ///
    /// Default behavior depends on whether you're logged in:
    /// - Logged in (default): writes a secret/unlisted path under your
    ///   `pathstash` repo. Listable from your account; not publicly visible.
    /// - Not logged in: falls through to the public anonymous endpoint.
    ///   Anon paths are not listable and capped at 5 MB.
    ///
    /// Use `--repo`/`--slug`/`--public` to override the pathstash default
    /// when authenticated. Use `--anon` to force the anonymous endpoint
    /// even when credentials are present.
    Pathbase {
        /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Pathbase server URL (defaults to the stored session's server)
        #[arg(long)]
        url: Option<String>,

        /// Force the anonymous endpoint, ignoring any stored credentials
        #[arg(long, conflicts_with_all = ["repo", "public"])]
        anon: bool,

        /// Target a specific repo as `owner/name` instead of `<you>/pathstash`
        #[arg(long, value_parser = parse_repo_spec)]
        repo: Option<RepoSpec>,

        /// Override the auto-derived slug (defaults to the toolpath document id)
        #[arg(long)]
        slug: Option<String>,

        /// Make the uploaded path publicly listable (default: secret/unlisted)
        #[arg(long)]
        public: bool,
    },
}

/// `owner/name` pair for `--repo`.
#[derive(Debug, Clone)]
pub struct RepoSpec {
    pub owner: String,
    pub name: String,
}

fn parse_repo_spec(s: &str) -> std::result::Result<RepoSpec, String> {
    let (owner, name) = s
        .split_once('/')
        .ok_or_else(|| format!("expected owner/name, got `{s}`"))?;
    if owner.is_empty() || name.is_empty() {
        return Err(format!("expected owner/name, got `{s}`"));
    }
    Ok(RepoSpec {
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

pub fn run(target: ExportTarget) -> Result<()> {
    match target {
        ExportTarget::Claude {
            input,
            project,
            output,
        } => run_claude(input, project, output),
        ExportTarget::Gemini {
            input,
            project,
            output,
        } => run_gemini(input, project, output),
        ExportTarget::Pathbase {
            input,
            url,
            anon,
            repo,
            slug,
            public,
        } => run_pathbase(PathbaseExportArgs {
            input,
            url,
            anon,
            repo,
            slug,
            public,
        }),
    }
}

#[derive(Debug)]
struct PathbaseExportArgs {
    input: String,
    url: Option<String>,
    anon: bool,
    repo: Option<RepoSpec>,
    slug: Option<String>,
    public: bool,
}

fn run_claude(input: String, project: Option<PathBuf>, output: Option<PathBuf>) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output);
        anyhow::bail!("'path export claude' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let path = load_path_doc(&input)?;
        let conversation = build_claude_conversation(&path)?;
        let jsonl = serialize_jsonl(&conversation)?;

        match (project, output) {
            (Some(project_dir), None) => {
                let out_path = write_into_claude_project(&conversation, &jsonl, &project_dir)?;
                let session_id = &conversation.session_id;
                eprintln!(
                    "Exported session {} ({} entries) → {}",
                    session_id,
                    conversation.preamble.len() + conversation.entries.len(),
                    out_path.display()
                );
                eprintln!();
                eprintln!("Resume with:");
                eprintln!("  cd {} && claude -r {}", project_dir.display(), session_id);
            }
            (None, Some(out_path)) => {
                std::fs::write(&out_path, &jsonl)
                    .with_context(|| format!("write {}", out_path.display()))?;
                eprintln!("Wrote {} bytes to {}", jsonl.len(), out_path.display());
            }
            (None, None) => {
                println!("{}", jsonl);
            }
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }

        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn load_path_doc(input: &str) -> Result<toolpath::v1::Path> {
    let file = cache_ref(input)?;
    let json = std::fs::read_to_string(&file)
        .with_context(|| format!("Failed to read {}", file.display()))?;
    let doc = toolpath::v1::Graph::from_json(&json)
        .map_err(|e| anyhow::anyhow!("Failed to parse toolpath document: {}", e))?;
    doc.into_single_path().ok_or_else(|| {
        anyhow::anyhow!(
            "expected a single-path graph; the source graph holds zero or multiple paths"
        )
    })
}

#[cfg(not(target_os = "emscripten"))]
fn build_claude_conversation(path: &toolpath::v1::Path) -> Result<toolpath_claude::Conversation> {
    use toolpath_convo::ConversationProjector;
    let view = toolpath_convo::extract_conversation(path);
    let projector = toolpath_claude::ClaudeProjector;
    projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))
}

#[cfg(not(target_os = "emscripten"))]
fn serialize_jsonl(conv: &toolpath_claude::Conversation) -> Result<String> {
    let mut lines = Vec::with_capacity(conv.preamble.len() + conv.entries.len());
    for raw in &conv.preamble {
        lines.push(serde_json::to_string(raw)?);
    }
    for entry in &conv.entries {
        lines.push(serde_json::to_string(entry)?);
    }
    Ok(lines.join("\n"))
}

#[cfg(not(target_os = "emscripten"))]
fn write_into_claude_project(
    conv: &toolpath_claude::Conversation,
    jsonl: &str,
    project_dir: &std::path::Path,
) -> Result<PathBuf> {
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let project_path = project_dir.to_string_lossy();

    let resolver = toolpath_claude::PathResolver::new();
    let claude_project_dir = resolver
        .project_dir(&project_path)
        .map_err(|e| anyhow::anyhow!("Cannot resolve Claude project dir: {}", e))?;

    std::fs::create_dir_all(&claude_project_dir)
        .with_context(|| format!("create {}", claude_project_dir.display()))?;

    let session_id = &conv.session_id;
    let out_path = claude_project_dir.join(format!("{}.jsonl", session_id));
    std::fs::write(&out_path, jsonl).with_context(|| format!("write {}", out_path.display()))?;
    Ok(out_path)
}

// ── Gemini ────────────────────────────────────────────────────────────

fn run_gemini(input: String, project: Option<PathBuf>, output: Option<PathBuf>) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output);
        anyhow::bail!("'path export gemini' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        // Resolve the cwd-or-arg up front. With --output, this only
        // affects the projector's `projectHash` / `directories` payload
        // (so the emitted ChatFile carries values matching whichever
        // directory `gemini` would later be invoked from).
        let project_dir = match project.as_ref() {
            Some(p) => std::fs::canonicalize(p)
                .with_context(|| format!("resolve project path {}", p.display()))?,
            None => std::env::current_dir()?,
        };
        let project_path = project_dir.to_string_lossy().to_string();

        let conversation = build_gemini_conversation(&input, &project_path)?;

        match (project, output) {
            (Some(_), None) => write_into_gemini_project(&conversation, &project_path)?,
            (None, Some(out_path)) => write_to_output_path(&conversation, &out_path)?,
            (None, None) => write_to_stdout(&conversation)?,
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }
        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn build_gemini_conversation(
    input: &str,
    project_path: &str,
) -> Result<toolpath_gemini::types::Conversation> {
    use toolpath_convo::ConversationProjector;

    let path = load_path_doc(input)?;
    let view = toolpath_convo::extract_conversation(&path);

    // The projector bakes `projectHash` and `directories` into the
    // emitted ChatFile so they match what Gemini computes when invoked
    // from the same cwd.
    let project_hash = toolpath_gemini::paths::project_hash(project_path);
    let projector = toolpath_gemini::project::GeminiProjector::new()
        .with_project_hash(project_hash)
        .with_project_path(project_path.to_string());
    let conversation = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;

    if conversation.session_uuid.is_empty() {
        anyhow::bail!("Projected conversation has no session UUID — cannot place it on disk");
    }
    Ok(conversation)
}

/// `--project` mode: write the resume-ready layout under
/// `~/.gemini/tmp/<slot>/chats/`.
#[cfg(not(target_os = "emscripten"))]
fn write_into_gemini_project(
    conversation: &toolpath_gemini::types::Conversation,
    project_path: &str,
) -> Result<()> {
    let resolver = toolpath_gemini::PathResolver::new();
    let chats_dir = resolver
        .chats_dir(project_path)
        .map_err(|e| anyhow::anyhow!("Cannot resolve Gemini chats dir: {}", e))?;
    std::fs::create_dir_all(&chats_dir)
        .with_context(|| format!("create {}", chats_dir.display()))?;

    // Drop a `.project_root` marker so `list_project_dirs` and any
    // tooling that walks `tmp/` can pick us up even without a
    // `projects.json` entry.
    if let Some(slot_dir) = chats_dir.parent() {
        let marker = slot_dir.join(".project_root");
        if !marker.exists() {
            let _ = std::fs::write(&marker, format!("{}\n", project_path));
        }
    }

    let main_stem = gemini_main_stem(conversation);
    let main_path = chats_dir.join(format!("{}.json", main_stem));
    let written = write_main_and_subs(conversation, &main_path)?;

    print_summary(conversation, &written, &chats_dir);
    eprintln!();
    eprintln!("Resume with:");
    eprintln!(
        "  cd {} && gemini --resume {}",
        project_path, conversation.session_uuid
    );
    Ok(())
}

/// `--output` mode: write the main chat file to the caller-specified
/// path; sub-agents (if any) land in a sibling `<session-uuid>/` dir.
#[cfg(not(target_os = "emscripten"))]
fn write_to_output_path(
    conversation: &toolpath_gemini::types::Conversation,
    out_path: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let written = write_main_and_subs(conversation, out_path)?;

    let parent: PathBuf = out_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    print_summary(conversation, &written, &parent);
    Ok(())
}

/// Stdout mode: pretty-print the main ChatFile JSON. Sub-agents — which
/// don't have a single-document representation in Gemini's format — are
/// dropped with a warning so the caller knows about the loss.
#[cfg(not(target_os = "emscripten"))]
fn write_to_stdout(conversation: &toolpath_gemini::types::Conversation) -> Result<()> {
    let json = serde_json::to_string_pretty(&conversation.main)?;
    println!("{}", json);
    if !conversation.sub_agents.is_empty() {
        let n = conversation.sub_agents.len();
        eprintln!(
            "warning: {} sub-agent chat{} not emitted on stdout — Gemini's format \
             stores each sub-agent in a separate file. Use --output or --project \
             to preserve them.",
            n,
            if n == 1 { "" } else { "s" },
        );
    }
    Ok(())
}

/// Write `conversation.main` to `main_path` and any sub-agents to a
/// sibling `<main_dir>/<session-uuid>/<stem>.json`. Returns every path
/// written, in order.
#[cfg(not(target_os = "emscripten"))]
fn write_main_and_subs(
    conversation: &toolpath_gemini::types::Conversation,
    main_path: &std::path::Path,
) -> Result<Vec<PathBuf>> {
    std::fs::write(main_path, serde_json::to_string_pretty(&conversation.main)?)
        .with_context(|| format!("write {}", main_path.display()))?;
    let mut written: Vec<PathBuf> = vec![main_path.to_path_buf()];

    if !conversation.sub_agents.is_empty() {
        let parent = main_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let sub_dir = parent.join(&conversation.session_uuid);
        std::fs::create_dir_all(&sub_dir)
            .with_context(|| format!("create {}", sub_dir.display()))?;
        for (i, sub) in conversation.sub_agents.iter().enumerate() {
            let stem = if sub.session_id.is_empty() {
                format!("subagent-{}", i)
            } else {
                sub.session_id.clone()
            };
            let sub_path = sub_dir.join(format!("{}.json", stem));
            std::fs::write(&sub_path, serde_json::to_string_pretty(sub)?)
                .with_context(|| format!("write {}", sub_path.display()))?;
            written.push(sub_path);
        }
    }
    Ok(written)
}

#[cfg(not(target_os = "emscripten"))]
fn print_summary(
    conversation: &toolpath_gemini::types::Conversation,
    written: &[PathBuf],
    location: &std::path::Path,
) {
    let total_messages = conversation.main.messages.len()
        + conversation
            .sub_agents
            .iter()
            .map(|s| s.messages.len())
            .sum::<usize>();
    let sub_n = conversation.sub_agents.len();
    eprintln!(
        "Exported Gemini session {} ({} messages across main + {} sub-agent{}) → {}",
        conversation.session_uuid,
        total_messages,
        sub_n,
        if sub_n == 1 { "" } else { "s" },
        location.display()
    );
    for path in written {
        eprintln!("  wrote {}", path.display());
    }
}

/// On-disk stem for a Gemini main chat file:
/// `session-<YYYY-MM-DDTHH-MM>-<first8-of-uuid>`.
///
/// The `session-` prefix is mandatory — Gemini CLI's `--list-sessions`
/// filters on it before opening any file, so a file without it is
/// invisible to `--resume`. The short suffix matches Gemini's own
/// naming convention. Falls back to `session-<uuid>` if no timestamp is
/// available on the projected conversation.
#[cfg(not(target_os = "emscripten"))]
fn gemini_main_stem(convo: &toolpath_gemini::types::Conversation) -> String {
    let short: String = convo.session_uuid.chars().take(8).collect();
    let ts = convo
        .started_at
        .or(convo.last_activity)
        .or(convo.main.start_time)
        .or(convo.main.last_updated);
    match ts {
        Some(t) => format!("session-{}-{}", t.format("%Y-%m-%dT%H-%M"), short),
        None => format!("session-{}", convo.session_uuid),
    }
}

// ── Pathbase ──────────────────────────────────────────────────────────

fn run_pathbase(args: PathbaseExportArgs) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = args;
        anyhow::bail!("'path export pathbase' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        use crate::cmd_pathbase::{
            anon_paths_post, api_me, credentials_path, load_session, paths_post, repos_post,
            resolve_url,
        };

        let file = cache_ref(&args.input)?;
        let body = std::fs::read_to_string(&file)
            .with_context(|| format!("Failed to read {}", file.display()))?;
        // Validate locally so we give a clean error rather than relying on
        // the server to reject malformed payloads.
        let doc = toolpath::v1::Graph::from_json(&body)
            .map_err(|e| anyhow::anyhow!("Invalid toolpath document: {}", e))?;

        let stored = load_session(&credentials_path()?)?;
        let base_url = match (&args.url, &stored) {
            (Some(u), _) => resolve_url(Some(u.clone())),
            (None, Some(s)) => s.url.clone(),
            (None, None) => resolve_url(None),
        };

        // Anonymous mode: explicit --anon, or no credentials at all and no
        // override flags steering us toward an authed endpoint.
        let go_anon = args.anon || (stored.is_none() && args.repo.is_none() && args.slug.is_none());

        if go_anon {
            if !args.anon && stored.is_none() {
                eprintln!(
                    "note: not logged in — uploading anonymously (not listable). Run `path auth login --url {base_url}` for a listable upload."
                );
            }
            let resp = anon_paths_post(&base_url, &body)?;
            // Server returns either a full URL or a path-only string; in the
            // latter case prefix the base so the user gets a clickable link.
            let printable = if resp.url.starts_with("http://") || resp.url.starts_with("https://") {
                resp.url.clone()
            } else if resp.url.starts_with('/') {
                format!("{base_url}{}", resp.url)
            } else {
                format!("{base_url}/{}", resp.url)
            };
            println!("{printable}");
            eprintln!(
                "Uploaded {} → anon path {} ({} bytes)",
                file.display(),
                resp.id,
                body.len()
            );
            return Ok(());
        }

        let session = stored.ok_or_else(|| {
            anyhow::anyhow!("Not logged in. Run `path auth login` or pass `--anon`.")
        })?;
        if host_of(&base_url) != host_of(&session.url) {
            eprintln!(
                "warning: uploading to {} with a token issued by {}; expect 401 unless this is the same deployment",
                base_url, session.url
            );
        }

        let (owner, repo) = match args.repo {
            Some(spec) => (spec.owner, spec.name),
            None => {
                // Pathstash default: own the repo "pathstash" under our username,
                // creating it on demand. api_me is the source of truth for the
                // username (display name in stored.user can drift).
                let user = api_me(&base_url, &session.token)?;
                repos_post(&base_url, &session.token, "pathstash")?;
                (user.username, "pathstash".to_string())
            }
        };

        let slug = args.slug.unwrap_or_else(|| derive_slug(&doc));
        let is_public = args.public;
        let created = paths_post(
            &base_url,
            &session.token,
            &owner,
            &repo,
            &slug,
            &body,
            is_public,
        )?;

        let visibility = if is_public { "public" } else { "secret" };
        // For secret paths the slug URL is an owner-facing stub — the listing
        // doesn't surface secrets, and a non-owner hitting `/<owner>/<repo>/<slug>`
        // gets a dead end. The UUID URL `/<owner>/<repo>/paths/<id>` is the
        // canonical share token (anyone with it can fetch the path; no one
        // without it can guess at it). For public paths the slug URL is the
        // listable canonical address, so we keep it.
        let url = if is_public {
            format!("{base_url}/{owner}/{repo}/{}", created.slug)
        } else {
            format!("{base_url}/{owner}/{repo}/paths/{}", created.id)
        };
        println!("{url}");
        eprintln!(
            "Uploaded {} → {}/{}/{} ({} path, {} bytes)",
            file.display(),
            owner,
            repo,
            created.slug,
            visibility,
            body.len()
        );
        Ok(())
    }
}

/// Pick a URL-safe slug from the document. Defaults to the inner id; falls
/// back to a timestamp-stamped placeholder if the id is empty or non-ascii.
#[cfg(not(target_os = "emscripten"))]
fn derive_slug(doc: &toolpath::v1::Graph) -> String {
    let raw = match doc.single_path() {
        Some(p) => p.path.id.as_str(),
        None => doc.graph.id.as_str(),
    };
    let slug: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        format!("path-{}", chrono::Utc::now().timestamp())
    } else {
        trimmed
    }
}

/// Extract `scheme://host[:port]` from a URL, dropping any path/query.
/// Returns the input unchanged if it doesn't look like a URL.
#[cfg(not(target_os = "emscripten"))]
fn host_of(url: &str) -> &str {
    let after_scheme = match url.find("://") {
        Some(i) => i + 3,
        None => return url,
    };
    // Find the next `/` after the scheme://; everything before it is host[:port].
    match url[after_scheme..].find('/') {
        Some(off) => &url[..after_scheme + off],
        None => url,
    }
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use toolpath::v1::{ArtifactChange, PathIdentity, Step, StepIdentity, StructuralChange};

    fn make_path_doc() -> toolpath::v1::Graph {
        let artifact_key = "agent://claude/test-session";

        let init_step = Step {
            step: StepIdentity {
                id: "step-001".to_string(),
                parents: vec![],
                actor: "tool:claude-code".to_string(),
                timestamp: "2024-01-01T00:00:00Z".to_string(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact_key.to_string(),
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.init".to_string(),
                            extra: HashMap::new(),
                        }),
                    },
                );
                m
            },
            meta: None,
        };

        let append_step = Step {
            step: StepIdentity {
                id: "step-002".to_string(),
                parents: vec!["step-001".to_string()],
                actor: "human:user".to_string(),
                timestamp: "2024-01-01T00:00:01Z".to_string(),
            },
            change: {
                let mut m = HashMap::new();
                let mut extra = HashMap::new();
                extra.insert("role".to_string(), serde_json::json!("user"));
                extra.insert("text".to_string(), serde_json::json!("Hello"));
                m.insert(
                    artifact_key.to_string(),
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".to_string(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };

        let path = toolpath::v1::Path {
            path: PathIdentity {
                id: "test-path".to_string(),
                base: None,
                head: "step-002".to_string(),
                graph_ref: None,
            },
            steps: vec![init_step, append_step],
            meta: None,
        };

        toolpath::v1::Graph::from_path(path)
    }

    #[test]
    fn claude_output_to_file() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let output_path = temp.path().join("out.jsonl");

        let doc = make_path_doc();
        std::fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();

        run_claude(
            input_path.to_string_lossy().to_string(),
            None,
            Some(output_path.clone()),
        )
        .unwrap();

        let out = std::fs::read_to_string(&output_path).unwrap();
        assert!(!out.is_empty());
        for line in out.lines() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
    }

    #[test]
    fn claude_rejects_multi_path_graph() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let make_path = |id: &str| toolpath::v1::Path {
            path: PathIdentity {
                id: id.into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![Step {
                step: StepIdentity {
                    id: "s1".into(),
                    parents: vec![],
                    actor: "human:x".into(),
                    timestamp: "2024-01-01T00:00:00Z".into(),
                },
                change: HashMap::new(),
                meta: None,
            }],
            meta: None,
        };
        let multi = toolpath::v1::Graph {
            graph: toolpath::v1::GraphIdentity { id: "g".into() },
            paths: vec![
                toolpath::v1::PathOrRef::Path(Box::new(make_path("p1"))),
                toolpath::v1::PathOrRef::Path(Box::new(make_path("p2"))),
            ],
            meta: None,
        };
        std::fs::write(&input_path, serde_json::to_string(&multi).unwrap()).unwrap();

        let err = run_claude(input_path.to_string_lossy().to_string(), None, None).unwrap_err();
        assert!(err.to_string().contains("single-path graph"));
    }

    #[test]
    fn claude_invalid_json_errors() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        std::fs::write(&input_path, "not json").unwrap();
        let err = run_claude(input_path.to_string_lossy().to_string(), None, None).unwrap_err();
        assert!(err.to_string().contains("parse") || err.to_string().contains("Failed"));
    }

    #[test]
    fn host_of_strips_path() {
        assert_eq!(host_of("https://pathbase.dev"), "https://pathbase.dev");
        assert_eq!(host_of("https://pathbase.dev/"), "https://pathbase.dev");
        assert_eq!(
            host_of("https://pathbase.dev/api/v1/traces"),
            "https://pathbase.dev"
        );
        assert_eq!(
            host_of("http://127.0.0.1:9000/foo"),
            "http://127.0.0.1:9000"
        );
        assert_eq!(host_of("not-a-url"), "not-a-url");
    }

    #[test]
    fn gemini_writes_resume_ready_layout() {
        // End-to-end: a path doc whose conversation.append carries a
        // single user turn projects to a flat `session-*.json` file
        // under <slot>/chats/, with `kind: "main"` and the inner
        // `sessionId` matching the source UUID. Layout matches what
        // Gemini CLI's `--resume <uuid>` needs to find the session.
        use toolpath_gemini::{GeminiConvo, PathResolver};

        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let project_dir = temp.path().join("myproj");
        std::fs::create_dir_all(&project_dir).unwrap();

        // Build a minimal Path doc whose artifact key encodes the
        // session UUID we want to land in `sessionId`. The shared-derive
        // wire shape is `<provider>://<session-id>`, which extract
        // parses to populate `view.id` and `view.provider_id`.
        let session_uuid = "11111111-2222-3333-4444-555555555555";
        let artifact = format!("gemini-cli://{}", session_uuid);
        let mut extra = HashMap::new();
        extra.insert("role".into(), serde_json::json!("user"));
        extra.insert("text".into(), serde_json::json!("Hello from export"));
        let step = Step {
            step: StepIdentity {
                id: "step-001".into(),
                parents: vec![],
                actor: "human:alex".into(),
                timestamp: "2026-04-17T15:00:00Z".into(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact,
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".into(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };
        let doc = toolpath::v1::Graph::from_path(toolpath::v1::Path {
            path: PathIdentity {
                id: "test-path".into(),
                base: None,
                head: "step-001".into(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        });
        let input_path = temp.path().join("doc.json");
        std::fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();

        // Override HOME so `PathResolver::new()` lands in the temp dir.
        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &fake_home);
        }
        let result = run_gemini(
            input_path.to_string_lossy().to_string(),
            Some(project_dir.clone()),
            None,
        );
        unsafe {
            match prior_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        result.expect("export gemini");

        // The file landed at chats/session-*.json (flat, prefixed).
        let canon_project = std::fs::canonicalize(&project_dir).unwrap();
        let resolver = PathResolver::new().with_home(&fake_home);
        let chats_dir = resolver.chats_dir(canon_project.to_str().unwrap()).unwrap();

        let session_files: Vec<PathBuf> = std::fs::read_dir(&chats_dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|s| s.starts_with("session-") && s.ends_with(".json"))
            })
            .collect();
        assert_eq!(session_files.len(), 1, "expected one session-*.json");

        let raw = std::fs::read_to_string(&session_files[0]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["sessionId"].as_str(), Some(session_uuid));
        assert_eq!(parsed["kind"].as_str(), Some("main"));

        // `--resume <uuid>` reads via inner sessionId, so verify the
        // library's resolver (which mirrors that lookup) finds it.
        let convo = GeminiConvo::with_resolver(resolver);
        let loaded = convo
            .read_conversation(canon_project.to_str().unwrap(), session_uuid)
            .expect("read back via uuid");
        assert_eq!(loaded.main.messages.len(), 1);
        assert_eq!(loaded.main.messages[0].content.text(), "Hello from export");
    }

    #[test]
    fn gemini_rejects_multi_path_graph() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let make_path = |id: &str| toolpath::v1::Path {
            path: PathIdentity {
                id: id.into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![Step {
                step: StepIdentity {
                    id: "s1".into(),
                    parents: vec![],
                    actor: "human:x".into(),
                    timestamp: "2024-01-01T00:00:00Z".into(),
                },
                change: HashMap::new(),
                meta: None,
            }],
            meta: None,
        };
        let multi = toolpath::v1::Graph {
            graph: toolpath::v1::GraphIdentity { id: "g".into() },
            paths: vec![
                toolpath::v1::PathOrRef::Path(Box::new(make_path("p1"))),
                toolpath::v1::PathOrRef::Path(Box::new(make_path("p2"))),
            ],
            meta: None,
        };
        std::fs::write(&input_path, serde_json::to_string(&multi).unwrap()).unwrap();

        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let err = run_gemini(
            input_path.to_string_lossy().to_string(),
            Some(project),
            None,
        )
        .expect_err("should reject multi-path graph");
        assert!(err.to_string().contains("single-path graph"));
    }

    #[test]
    fn gemini_output_to_file_writes_main_at_path() {
        // `--output FILE` writes the main ChatFile to FILE. Sub-agents
        // (none in this fixture) would land in `<FILE_PARENT>/<uuid>/`.
        use toolpath_gemini::ChatFile;

        let temp = tempfile::tempdir().unwrap();
        let project_dir = temp.path().join("myproj");
        std::fs::create_dir_all(&project_dir).unwrap();
        let out_path = temp.path().join("out").join("session.json");

        let session_uuid = "33333333-4444-5555-6666-777777777777";
        let artifact = format!("gemini-cli://{}", session_uuid);
        let mut extra = HashMap::new();
        extra.insert("role".into(), serde_json::json!("user"));
        extra.insert("text".into(), serde_json::json!("Hello via output"));
        let step = Step {
            step: StepIdentity {
                id: "step-001".into(),
                parents: vec![],
                actor: "human:alex".into(),
                timestamp: "2026-04-17T15:00:00Z".into(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact,
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".into(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };
        let doc = toolpath::v1::Graph::from_path(toolpath::v1::Path {
            path: PathIdentity {
                id: "test-path".into(),
                base: None,
                head: "step-001".into(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        });
        let input_path = temp.path().join("doc.json");
        std::fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();

        // `--output` is mutually exclusive with `--project`, so leave
        // project None. The projector still uses cwd to compute
        // `projectHash` and `directories` on the emitted ChatFile.
        run_gemini(
            input_path.to_string_lossy().to_string(),
            None,
            Some(out_path.clone()),
        )
        .expect("export gemini --output");

        // Parent dir is created automatically.
        assert!(out_path.exists(), "main file at output path missing");
        // The file is a valid Gemini ChatFile and carries the source UUID.
        let raw = std::fs::read_to_string(&out_path).unwrap();
        let parsed: ChatFile = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.session_id, session_uuid);
        assert_eq!(parsed.kind.as_deref(), Some("main"));
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].content.text(), "Hello via output");

        // No sub-agents in this fixture → no sibling UUID dir.
        assert!(!out_path.parent().unwrap().join(session_uuid).exists());
    }

    #[test]
    fn gemini_project_and_output_mutually_exclusive() {
        // clap's `conflicts_with` enforces this at parse time, but the
        // function itself also panics via `unreachable!` if both are
        // somehow passed. We can't easily exercise that without going
        // through clap; this test lives mostly as a reminder that the
        // contract holds — when both are provided, clap rejects the
        // invocation before `run_gemini` runs at all.
        use clap::Parser;
        #[derive(Parser, Debug)]
        struct Cli {
            #[command(subcommand)]
            cmd: ExportTarget,
        }
        let parsed = Cli::try_parse_from([
            "test",
            "gemini",
            "--input",
            "x",
            "--project",
            "/tmp/p",
            "--output",
            "/tmp/o.json",
        ]);
        assert!(
            parsed.is_err(),
            "clap must reject simultaneous --project and --output"
        );
    }

    #[test]
    fn pathbase_repo_flag_requires_login() {
        // With explicit --repo (i.e. an authenticated upload), missing
        // credentials must surface the "Not logged in" error rather than
        // silently falling through to the anonymous endpoint.
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        std::fs::write(
            &input_path,
            serde_json::to_string(&make_path_doc()).unwrap(),
        )
        .unwrap();

        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(crate::config::CONFIG_DIR_ENV, temp.path());
        }
        let err = run_pathbase(PathbaseExportArgs {
            input: input_path.to_string_lossy().to_string(),
            url: Some("http://127.0.0.1:1".to_string()),
            anon: false,
            repo: Some(RepoSpec {
                owner: "alex".to_string(),
                name: "pathstash".to_string(),
            }),
            slug: None,
            public: false,
        })
        .unwrap_err();
        unsafe {
            std::env::remove_var(crate::config::CONFIG_DIR_ENV);
        }
        assert!(
            err.to_string().contains("Not logged in"),
            "expected `Not logged in` error, got: {err}"
        );
    }

    #[test]
    fn parse_repo_spec_accepts_owner_slash_name() {
        let spec = parse_repo_spec("alex/pathstash").unwrap();
        assert_eq!(spec.owner, "alex");
        assert_eq!(spec.name, "pathstash");
    }

    #[test]
    fn parse_repo_spec_rejects_missing_slash() {
        assert!(parse_repo_spec("alex").is_err());
        assert!(parse_repo_spec("/pathstash").is_err());
        assert!(parse_repo_spec("alex/").is_err());
    }

    #[test]
    fn derive_slug_uses_path_id() {
        let doc = make_path_doc();
        assert_eq!(derive_slug(&doc), "test-path");
    }

    #[test]
    fn derive_slug_sanitizes_non_url_safe_chars() {
        use toolpath::v1::{Graph, Path, PathIdentity};
        let doc = Graph::from_path(Path {
            path: PathIdentity {
                id: "claude/Path 42!".into(),
                base: None,
                head: "h".into(),
                graph_ref: None,
            },
            steps: vec![],
            meta: None,
        });
        assert_eq!(derive_slug(&doc), "claude-path-42");
    }
}
