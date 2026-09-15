//! `path export <target>` — emit toolpath documents into external formats.
//!
//! - `export claude` projects a Path document into Claude Code's JSONL
//!   format (either into `~/.claude/projects/<sanitized>/<session>.jsonl`
//!   for a resumable session, or to a file / stdout).
//! - `export gemini` projects a Path document into Gemini CLI's chat-file
//!   layout under `~/.gemini/tmp/<slot>/chats/`, ready for
//!   `gemini --resume <uuid>`.
//! - `export pi` projects a Path document into Pi's JSONL session
//!   format under `~/.pi/agent/sessions/--<encoded-cwd>--/<id>.jsonl`.
//! - `export codex` projects a Path document into Codex CLI's rollout
//!   JSONL under `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`,
//!   loadable via `codex resume <session-uuid>`.
//! - `export opencode` projects a Path document into rows in opencode's
//!   `~/.local/share/opencode/opencode.db` so the session is visible to
//!   the `opencode` CLI; or to a JSON file / stdout.
//! - `export pathbase` uploads the document to a Pathbase server.

#[cfg(not(target_os = "emscripten"))]
use anyhow::Context;
use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;

#[cfg(not(target_os = "emscripten"))]
use crate::cache::cache_ref;
#[cfg(not(target_os = "emscripten"))]
use crate::projection::{
    claude::{build_claude_conversation, serialize_jsonl, write_into_claude_project},
    codex::{serialize_codex_jsonl, write_into_codex_project},
    copilot::{build_copilot_session, project_copilot},
    cursor::{build_cursor_session, write_into_cursor_db},
    gemini::{print_gemini_summary, write_into_gemini_project, write_main_and_subs},
    opencode::{build_opencode_session, write_into_opencode_db},
    pi::{serialize_pi_jsonl, write_into_pi_project},
};
use crate::remote::RepoSpec;

#[cfg(all(feature = "resume-remote", not(target_os = "emscripten")))]
mod remote_session;

/// Arguments of `p export claude`.
#[derive(clap::Args, Debug, Default)]
pub struct ClaudeExportArgs {
    /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
    #[arg(short, long)]
    pub(crate) input: String,

    /// Target project directory. With this flag, writes the JSONL into
    /// `~/.claude/projects/<sanitized>/<session>.jsonl` so `claude -r <id>`
    /// can resume it. Defaults to cwd when no `--output` is given.
    #[arg(short, long)]
    pub(crate) project: Option<PathBuf>,

    /// Output JSONL to this file. Mutually exclusive with --project.
    #[arg(short, long, conflicts_with = "project")]
    pub(crate) output: Option<PathBuf>,

    /// Overwrite the session file if this session id already exists in
    /// the target project. Without it the export refuses rather than
    /// clobbering local history.
    #[arg(long)]
    pub(crate) force: bool,

    /// Rename the session to this ID (a UUID). The document's own
    /// session is not touched, so one document can be exported as
    /// several sessions. Mutually exclusive with --new-session-id.
    #[cfg(not(target_os = "emscripten"))]
    #[arg(
        long,
        value_name = "UUID",
        conflicts_with = "new_session_id",
        value_parser = crate::claude_session::parse_uuid_arg
    )]
    pub(crate) session_id: Option<String>,

    /// Rename the session to a fresh random UUID. Every export mints
    /// a different ID; the export prints it. Mutually exclusive with
    /// --session-id.
    #[arg(long)]
    pub(crate) new_session_id: bool,

    #[cfg(all(feature = "resume-remote", not(target_os = "emscripten")))]
    #[command(flatten)]
    pub(crate) remote: remote_session::RemoteSessionArgs,
}

#[derive(Subcommand, Debug)]
pub enum ExportTarget {
    /// Project a toolpath document into a Claude Code session
    Claude(ClaudeExportArgs),
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
    /// Project a toolpath document into a Pi (pi.dev) session
    Pi {
        /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target project directory. With this flag, writes the JSONL
        /// into `~/.pi/agent/sessions/--<encoded-cwd>--/<session>.jsonl`
        /// under the encoding scheme Pi uses for project directories.
        /// Defaults to cwd when neither --project nor --output is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output JSONL to this file. Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Project a toolpath document into a Codex CLI session
    Codex {
        /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target project directory. With this flag, writes the JSONL
        /// into `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (Codex's
        /// date-bucketed layout). The session_meta line carries this
        /// path as `cwd`, so `codex resume <session-uuid>` invoked from
        /// the same directory will resolve it correctly. Defaults to
        /// cwd when neither --project nor --output is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output JSONL to this file. Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Project a toolpath document into an opencode session
    Opencode {
        /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target project directory. With this flag, inserts session,
        /// message, and part rows into `~/.local/share/opencode/opencode.db`
        /// so the `opencode` CLI sees the session in its picker.
        /// Defaults to cwd when neither --project nor --output is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output the projected `Session` (with messages and parts)
        /// as pretty JSON to this file. Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Project a toolpath document into a GitHub Copilot CLI session
    Copilot {
        /// Input: cache id (e.g. `copilot-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target project directory. With this flag, writes the session into
        /// `~/.copilot/session-state/<id>/` (+ a `session-store.db` row) with
        /// this directory as the session cwd, so `copilot --resume <id>` can
        /// load it. Only ever INSERTs a fresh id — never touches existing
        /// sessions.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output the projected `events.jsonl` to this file. Mutually
        /// exclusive with --project. With neither, prints it to stdout.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Project a toolpath document into a Cursor (IDE) composer
    Cursor {
        /// Input: cache id (e.g. `cursor-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target workspace folder. With this flag, writes
        /// `composerData:`, `bubbleId:` and `composer.content.*` rows
        /// into `<user-data>/User/globalStorage/state.vscdb` so the
        /// composer appears in Cursor.app's chat sidebar when that
        /// folder is open. Defaults to cwd when neither --project
        /// nor --output is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output the projected `CursorSession` as pretty JSON to
        /// this file. Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Upload a toolpath document to Pathbase.
    ///
    /// Default behavior depends on whether you're logged in:
    /// - Logged in (default): writes an unlisted graph under your
    ///   `pathstash` repo. Listable from your account; not publicly visible.
    /// - Not logged in: falls through to the public anonymous endpoint.
    ///   Anon graphs are not listable.
    ///
    /// Use `--repo`/`--name`/`--public` to override the pathstash default
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
        #[arg(long, value_parser = crate::remote::parse_repo_spec)]
        repo: Option<RepoSpec>,

        /// Human-readable display label for the uploaded graph
        /// (defaults to the toolpath document id). Free-form; not used
        /// in the URL — graphs are addressed by UUID server-side.
        #[arg(long, alias = "slug")]
        name: Option<String>,

        /// Mark the uploaded graph public (default: unlisted, addressable only by UUID)
        #[arg(long)]
        public: bool,
    },
    /// Upload a toolpath document to object storage: an S3 bucket, an
    /// S3-compatible endpoint, or a plain folder.
    ///
    /// S3 credentials come from your `~/.aws` profiles, the AWS
    /// environment, or `path auth s3 login`; a folder needs none. The
    /// object is named `<date>-<topic>--<graph id>.json`, and the
    /// printed location is what `path resume` takes. The object is the
    /// full document: every turn, verbatim diffs, and tool output.
    #[command(alias = "s3")]
    Object(ObjectExportArgs),
}

pub fn run(target: ExportTarget) -> Result<()> {
    match target {
        ExportTarget::Claude(args) => run_claude(args),
        ExportTarget::Gemini {
            input,
            project,
            output,
        } => run_gemini(input, project, output),
        ExportTarget::Pi {
            input,
            project,
            output,
        } => run_pi(input, project, output),
        ExportTarget::Codex {
            input,
            project,
            output,
        } => run_codex(input, project, output),
        ExportTarget::Opencode {
            input,
            project,
            output,
        } => run_opencode(input, project, output),
        ExportTarget::Copilot {
            input,
            project,
            output,
        } => run_copilot(input, project, output),
        ExportTarget::Cursor {
            input,
            project,
            output,
        } => run_cursor(input, project, output),
        ExportTarget::Pathbase {
            input,
            url,
            anon,
            repo,
            name,
            public,
        } => run_pathbase(PathbaseExportArgs {
            input,
            url,
            anon,
            repo,
            name,
            public,
        }),
        ExportTarget::Object(args) => run_object(args),
    }
}

/// Arguments of `p export object`.
#[derive(clap::Args, Debug)]
pub(crate) struct ObjectExportArgs {
    /// Input: cache ID (e.g. `claude-abc`) or path to a toolpath JSON file
    #[arg(short, long, required_unless_present = "all", conflicts_with = "all")]
    pub input: Option<String>,

    /// Export every cached document instead of one. Documents that were
    /// themselves imported from object storage or Pathbase are skipped
    /// (see --include-imported), and documents already uploaded to this
    /// destination with the same bytes are skipped using the export
    /// ledger. Failures are reported and tallied, not fatal.
    #[arg(long)]
    pub all: bool,

    /// With --all: also export `object-` and `pathbase-` cache entries
    #[arg(long, requires = "all")]
    pub include_imported: bool,

    /// Destination: `s3://bucket/prefix`, or a folder (`~/traces`,
    /// `file:///srv/traces`).
    #[arg(long, value_name = "DESTINATION")]
    pub to: String,

    /// Upload even if the input does not validate as a toolpath document
    #[arg(long)]
    pub force: bool,

    /// Resolve everything and print what would be written, without
    /// writing: the object location, endpoint, region, credential
    /// source, and put mode.
    #[arg(long)]
    pub dry_run: bool,

    /// Refuse to replace an object that already exists at the computed
    /// key (a create-only put). Default is to overwrite, because a
    /// re-export of a session that grew should replace its own object.
    #[arg(long)]
    pub no_overwrite: bool,
}

#[derive(Debug)]
struct PathbaseExportArgs {
    input: String,
    url: Option<String>,
    anon: bool,
    repo: Option<RepoSpec>,
    name: Option<String>,
    public: bool,
}

/// Pathbase upload knobs that don't depend on where the body came from.
/// Identical to [`PathbaseExportArgs`] minus the `input` field — the body
/// is supplied by the caller (read from cache, derived in memory, …).
#[cfg(not(target_os = "emscripten"))]
#[derive(Debug)]
pub(crate) struct PathbaseUploadArgs {
    pub(crate) url: Option<String>,
    pub(crate) anon: bool,
    pub(crate) repo: Option<RepoSpec>,
    pub(crate) name: Option<String>,
    pub(crate) public: bool,
}

/// `path p export copilot` — project a document into a Copilot session on
/// disk (`--project`), to a file (`--output`), or to stdout (neither).
fn run_copilot(input: String, project: Option<PathBuf>, output: Option<PathBuf>) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output);
        anyhow::bail!("'path export copilot' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let path = load_path_doc(&input)?;
        match (project, output) {
            (Some(project_dir), None) => {
                let id = project_copilot(&path, &project_dir)?;
                eprintln!();
                eprintln!("Resume with:");
                eprintln!("  copilot --resume {id}");
            }
            (None, out) => {
                // No target dir: root the session at cwd and emit events.jsonl
                // to the file (or stdout) without touching ~/.copilot.
                let cwd = std::env::current_dir().context("resolve current directory")?;
                let session = build_copilot_session(&path, &cwd)?;
                let mut jsonl = String::new();
                for line in &session.lines {
                    jsonl.push_str(&serde_json::to_string(line)?);
                    jsonl.push('\n');
                }
                match out {
                    Some(out_path) => {
                        std::fs::write(&out_path, &jsonl)
                            .with_context(|| format!("write {}", out_path.display()))?;
                        eprintln!(
                            "Wrote {} events to {}",
                            session.lines.len(),
                            out_path.display()
                        );
                    }
                    None => print!("{jsonl}"),
                }
            }
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }
        Ok(())
    }
}

/// The content-addressed session ID when `--content-addressed-session-id`
/// is set, else `None`.
#[cfg(all(feature = "resume-remote", not(target_os = "emscripten")))]
fn resolve_content_addressed_session_id(
    args: &ClaudeExportArgs,
    document_json: &str,
) -> Result<Option<String>> {
    if !args.remote.content_addressed_session_id {
        return Ok(None);
    }
    crate::claude_session::generate_content_addressed_session_id(document_json).map(Some)
}

/// Without the `resume-remote` feature there is no `--content-addressed-session-id`.
#[cfg(all(not(feature = "resume-remote"), not(target_os = "emscripten")))]
fn resolve_content_addressed_session_id(
    _args: &ClaudeExportArgs,
    _document_json: &str,
) -> Result<Option<String>> {
    Ok(None)
}

/// The ID the exported session takes, or `None` to keep the ID the
/// document carries. clap makes the naming flags mutually exclusive,
/// so at most one arm answers.
#[cfg(not(target_os = "emscripten"))]
fn exported_session_id(args: &ClaudeExportArgs, document_json: &str) -> Result<Option<String>> {
    if let Some(id) = &args.session_id {
        return Ok(Some(id.clone()));
    }
    if args.new_session_id {
        return Ok(Some(uuid::Uuid::new_v4().to_string()));
    }
    resolve_content_addressed_session_id(args, document_json)
}

fn run_claude(args: ClaudeExportArgs) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = args;
        anyhow::bail!("'path export claude' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let document_json = read_doc_json(&args.input)?;
        let path = parse_path_doc(&document_json)?;
        let mut conversation = build_claude_conversation(&path)?;
        if let Some(id) = exported_session_id(&args, &document_json)? {
            conversation.rename_session(&id);
        }
        #[cfg(feature = "resume-remote")]
        if let Some(dir) = &args.remote.cwd {
            conversation.reroot(dir);
        }
        let jsonl = serialize_jsonl(&conversation)?;

        match (args.project, args.output) {
            (Some(project_dir), None) => {
                let out_path =
                    write_into_claude_project(&conversation, &jsonl, &project_dir, args.force)?;
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
                eprintln!(
                    "Wrote session {} ({} bytes) to {}",
                    conversation.session_id,
                    jsonl.len(),
                    out_path.display()
                );
            }
            (None, None) => {
                println!("{}", jsonl);
                eprintln!("Wrote session {} to stdout", conversation.session_id);
            }
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }

        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn load_path_doc(input: &str) -> Result<toolpath::v1::Path> {
    parse_path_doc(&read_doc_json(input)?)
}

#[cfg(not(target_os = "emscripten"))]
fn read_doc_json(input: &str) -> Result<String> {
    let file = cache_ref(input)?;
    std::fs::read_to_string(&file).with_context(|| format!("Failed to read {}", file.display()))
}

#[cfg(not(target_os = "emscripten"))]
fn parse_path_doc(json: &str) -> Result<toolpath::v1::Path> {
    let doc = toolpath::v1::Graph::from_json(json)
        .map_err(|e| anyhow::anyhow!("Failed to parse toolpath document: {}", e))?;
    doc.into_single_path().ok_or_else(|| {
        anyhow::anyhow!(
            "expected a single-path graph; the source graph holds zero or multiple paths"
        )
    })
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
    print_gemini_summary(conversation, &written, &parent);
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

// ── Pi ────────────────────────────────────────────────────────────────

fn run_pi(input: String, project: Option<PathBuf>, output: Option<PathBuf>) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output);
        anyhow::bail!("'path export pi' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        // Resolve cwd up front; with --output it only feeds the
        // SessionHeader.cwd (so a downstream Pi reading the file from
        // a different directory still sees the originating cwd).
        let project_dir = match project.as_ref() {
            Some(p) => std::fs::canonicalize(p)
                .with_context(|| format!("resolve project path {}", p.display()))?,
            None => std::env::current_dir()?,
        };
        let cwd_str = project_dir.to_string_lossy().to_string();

        let session = build_pi_session(&input, &cwd_str)?;

        match (project, output) {
            (Some(_), None) => write_into_pi_project(&session, &cwd_str)?,
            (None, Some(out_path)) => write_pi_to_output_path(&session, &out_path)?,
            (None, None) => write_pi_to_stdout(&session)?,
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }
        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn build_pi_session(input: &str, cwd: &str) -> Result<toolpath_pi::PiSession> {
    use toolpath_convo::ConversationProjector;

    let path = load_path_doc(input)?;
    let view = toolpath_convo::extract_conversation(&path);

    let projector = toolpath_pi::project::PiProjector::new().with_cwd(cwd.to_string());
    let session = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;

    if session.header.id.is_empty() {
        anyhow::bail!("Projected session has no id — cannot place it on disk");
    }
    Ok(session)
}

/// `--output` mode: write JSONL to the caller-specified path.
#[cfg(not(target_os = "emscripten"))]
fn write_pi_to_output_path(
    session: &toolpath_pi::PiSession,
    out_path: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let bytes = serialize_pi_jsonl(session)?;
    std::fs::write(out_path, &bytes).with_context(|| format!("write {}", out_path.display()))?;
    eprintln!("Wrote {} bytes to {}", bytes.len(), out_path.display());
    Ok(())
}

/// Stdout mode.
#[cfg(not(target_os = "emscripten"))]
fn write_pi_to_stdout(session: &toolpath_pi::PiSession) -> Result<()> {
    let bytes = serialize_pi_jsonl(session)?;
    print!("{}", bytes);
    Ok(())
}

// ── Codex ─────────────────────────────────────────────────────────────

fn run_codex(input: String, project: Option<PathBuf>, output: Option<PathBuf>) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output);
        anyhow::bail!("'path export codex' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        // Resolve cwd up front; with --output it only feeds the
        // SessionMeta.cwd / TurnContext.cwd payload (so a `codex
        // resume` invocation from a different directory still sees the
        // originating cwd recorded in the rollout).
        let project_dir = match project.as_ref() {
            Some(p) => std::fs::canonicalize(p)
                .with_context(|| format!("resolve project path {}", p.display()))?,
            None => std::env::current_dir()?,
        };
        let cwd_str = project_dir.to_string_lossy().to_string();

        let session = build_codex_session(&input, &cwd_str)?;

        match (project, output) {
            (Some(_), None) => write_into_codex_project(&session)?,
            (None, Some(out_path)) => write_codex_to_output_path(&session, &out_path)?,
            (None, None) => write_codex_to_stdout(&session)?,
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }
        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn build_codex_session(input: &str, cwd: &str) -> Result<toolpath_codex::Session> {
    use toolpath_convo::ConversationProjector;

    let path = load_path_doc(input)?;
    let view = toolpath_convo::extract_conversation(&path);

    let projector = toolpath_codex::project::CodexProjector::new().with_cwd(cwd.to_string());
    let session = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;

    if session.id.is_empty() {
        anyhow::bail!("Projected session has no id — cannot place it on disk");
    }
    Ok(session)
}

/// `--output` mode: write JSONL to the caller-specified path.
#[cfg(not(target_os = "emscripten"))]
fn write_codex_to_output_path(
    session: &toolpath_codex::Session,
    out_path: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let bytes = serialize_codex_jsonl(session)?;
    std::fs::write(out_path, &bytes).with_context(|| format!("write {}", out_path.display()))?;
    eprintln!("Wrote {} bytes to {}", bytes.len(), out_path.display());
    Ok(())
}

/// Stdout mode.
#[cfg(not(target_os = "emscripten"))]
fn write_codex_to_stdout(session: &toolpath_codex::Session) -> Result<()> {
    let bytes = serialize_codex_jsonl(session)?;
    print!("{}", bytes);
    Ok(())
}

// ── Opencode ──────────────────────────────────────────────────────────

fn run_opencode(input: String, project: Option<PathBuf>, output: Option<PathBuf>) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output);
        anyhow::bail!("'path export opencode' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let path = load_path_doc(&input)?;
        match (project, output) {
            (Some(project_dir), None) => {
                let session = build_opencode_session(&path, Some(&project_dir))?;
                write_into_opencode_db(&session, &project_dir)?;
            }
            (None, Some(out_path)) => {
                let session = build_opencode_session(&path, None)?;
                write_opencode_to_output_path(&session, &out_path)?;
            }
            (None, None) => {
                let cwd = std::env::current_dir().ok();
                let session = build_opencode_session(&path, cwd.as_deref())?;
                write_opencode_to_stdout(&session)?;
            }
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }
        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn write_opencode_to_output_path(
    session: &toolpath_opencode::Session,
    out_path: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(session)?;
    std::fs::write(out_path, &json).with_context(|| format!("write {}", out_path.display()))?;
    eprintln!(
        "Wrote {} bytes to {} ({} messages)",
        json.len(),
        out_path.display(),
        session.messages.len()
    );
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn write_opencode_to_stdout(session: &toolpath_opencode::Session) -> Result<()> {
    let json = serde_json::to_string_pretty(session)?;
    println!("{}", json);
    Ok(())
}

// ── Cursor ────────────────────────────────────────────────────────────

fn run_cursor(input: String, project: Option<PathBuf>, output: Option<PathBuf>) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output);
        anyhow::bail!("'path export cursor' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let path = load_path_doc(&input)?;
        match (project, output) {
            (Some(project_dir), None) => {
                let session = build_cursor_session(&path, Some(&project_dir))?;
                write_into_cursor_db(&session, &project_dir)?;
            }
            (None, Some(out_path)) => {
                let session = build_cursor_session(&path, None)?;
                write_cursor_to_output_path(&session, &out_path)?;
            }
            (None, None) => {
                let cwd = std::env::current_dir().ok();
                let session = build_cursor_session(&path, cwd.as_deref())?;
                write_cursor_to_stdout(&session)?;
            }
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }
        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn write_cursor_to_output_path(
    session: &toolpath_cursor::CursorSession,
    out_path: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(session)?;
    std::fs::write(out_path, &json).with_context(|| format!("write {}", out_path.display()))?;
    eprintln!(
        "Wrote {} bytes to {} ({} bubbles)",
        json.len(),
        out_path.display(),
        session.bubbles.len()
    );
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn write_cursor_to_stdout(session: &toolpath_cursor::CursorSession) -> Result<()> {
    let json = serde_json::to_string_pretty(session)?;
    println!("{}", json);
    Ok(())
}

// ── Pathbase ──────────────────────────────────────────────────────────

// ── Object storage ────────────────────────────────────────────────────

/// Per-call knobs for an object export, shared by `p export object` and
/// `path share --to`.
#[cfg(not(target_os = "emscripten"))]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ExportOptions {
    pub force: bool,
    pub dry_run: bool,
    pub no_overwrite: bool,
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) enum ObjectOutcome {
    Uploaded(crate::store::ObjectUri),
    Unchanged(crate::store::ObjectUri),
    DryRun(crate::store::ObjectUri),
}

fn run_object(args: ObjectExportArgs) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = args;
        anyhow::bail!("'path p export object' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let dest = crate::store::Destination::parse(&args.to)?;
        let settings = crate::store::effective_settings()?;
        let opts = ExportOptions {
            force: args.force,
            dry_run: args.dry_run,
            no_overwrite: args.no_overwrite,
        };

        // (ledger key, file) pairs. For a single export the key is the
        // cache ID or the file stem; for --all it is always the cache ID.
        // `skipped_as_imported` counts cached documents excluded by the
        // default `object-`/`pathbase-` filter, so an empty or all-excluded
        // cache can say why rather than just "0 uploaded".
        let mut skipped_as_imported = 0usize;
        let inputs: Vec<(String, std::path::PathBuf)> = if args.all {
            let all_cached = crate::cache::list_cached()?;
            let total = all_cached.len();
            let filtered: Vec<_> = all_cached
                .into_iter()
                .filter(|e| {
                    args.include_imported
                        || !(e.id.starts_with("object-") || e.id.starts_with("pathbase-"))
                })
                .collect();
            skipped_as_imported = total - filtered.len();
            filtered.into_iter().map(|e| (e.id, e.path)).collect()
        } else {
            let input = args.input.as_deref().expect("clap: --input or --all");
            let file = cache_ref(input)?;
            let key = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| input.to_string());
            vec![(key, file)]
        };
        if inputs.is_empty() {
            // Under `--all` an empty result is success, not an error: the
            // nightly cron is `path p cache sync && path p export object
            // --all --to "$DEST"` under `set -e`, and a fresh cache (or one
            // holding only imported documents) must not fail it on day one.
            if args.all {
                eprintln!(
                    "{}",
                    tally_line(0, 0, 0, opts.dry_run, &dest, skipped_as_imported)
                );
                return Ok(());
            }
            anyhow::bail!("no cached documents to export; run `path p cache sync` first");
        }

        // Only --all consults the ledger to skip: a single explicit
        // export means "ship it", even if the bytes are unchanged.
        let ledger = if args.all {
            crate::export_ledger::load(&crate::export_ledger::ledger_path()?)?
        } else {
            crate::export_ledger::Ledger::new()
        };

        let (mut uploaded, mut unchanged, mut failed) = (0usize, 0usize, 0usize);
        for (key, file) in inputs {
            let body = match std::fs::read_to_string(&file)
                .with_context(|| format!("Failed to read {}", file.display()))
            {
                Ok(b) => b,
                Err(e) if args.all => {
                    eprintln!("warning: {key}: {e:#}");
                    failed += 1;
                    continue;
                }
                Err(e) => return Err(e),
            };
            match export_body(&body, &key, &file, &dest, &settings, &ledger, &opts) {
                Ok(ObjectOutcome::Uploaded(uri)) => {
                    println!("{uri}");
                    if !args.all {
                        eprintln!("Resume it with: path resume {uri}");
                    }
                    uploaded += 1;
                }
                Ok(ObjectOutcome::Unchanged(_)) => unchanged += 1,
                Ok(ObjectOutcome::DryRun(_)) => uploaded += 1,
                Err(e) if args.all => {
                    eprintln!("warning: {key}: {e:#}");
                    failed += 1;
                }
                Err(e) => return Err(e),
            }
        }

        if args.all {
            eprintln!(
                "{}",
                tally_line(
                    uploaded,
                    unchanged,
                    failed,
                    opts.dry_run,
                    &dest,
                    skipped_as_imported
                )
            );
            if failed > 0 {
                anyhow::bail!("{failed} document(s) failed to export");
            }
        }
        Ok(())
    }
}

/// The stderr summary line for `--all`: `would upload` under `--dry-run`,
/// `uploaded` otherwise, plus a note when the default `object-`/`pathbase-`
/// filter excluded documents (so "0 uploaded" can say why).
#[cfg(not(target_os = "emscripten"))]
fn tally_line(
    uploaded: usize,
    unchanged: usize,
    failed: usize,
    dry_run: bool,
    dest: &crate::store::Destination,
    skipped_as_imported: usize,
) -> String {
    let verb = if dry_run { "would upload" } else { "uploaded" };
    let mut line = format!("{uploaded} {verb}, {unchanged} unchanged, {failed} failed → {dest}");
    if skipped_as_imported > 0 {
        line.push_str(&format!(
            " ({skipped_as_imported} skipped as imported; --include-imported to include them)"
        ));
    }
    line
}

/// Export one document body to `dest`: validate and name it, skip it if
/// the ledger says these bytes already landed there, honor --dry-run,
/// put, and record the upload. `ledger_key` is how the upload is
/// remembered (the cache ID, or the file stem for a loose file);
/// `source_label` names the input in errors.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn export_body(
    body: &str,
    ledger_key: &str,
    source_label: &std::path::Path,
    dest: &crate::store::Destination,
    settings: &crate::store::S3Settings,
    ledger: &crate::export_ledger::Ledger,
    opts: &ExportOptions,
) -> Result<ObjectOutcome> {
    let name = object_name_for(body, source_label, opts.force)?;
    let uri = dest.uri_for(&name);
    let sha256 = crate::export_ledger::sha256_hex(body.as_bytes());

    if crate::export_ledger::unchanged(ledger, &dest.to_string(), ledger_key, &sha256) {
        eprintln!("Unchanged: {uri}");
        return Ok(ObjectOutcome::Unchanged(uri));
    }

    if opts.dry_run {
        eprintln!("would write {} bytes → {uri}", body.len());
        match dest.scheme() {
            "s3" | "s3a" => {
                let resolved = settings.resolve_real()?;
                eprintln!(
                    "  endpoint:    {}",
                    settings.endpoint.as_deref().unwrap_or("AWS S3")
                );
                let region = settings
                    .region
                    .clone()
                    .or_else(|| resolved.region.clone())
                    .unwrap_or_else(|| crate::store::DEFAULT_REGION.to_string());
                eprintln!("  region:      {region}");
                eprintln!("  credentials: {}", resolved.source);
            }
            _ => eprintln!("  credentials: none needed (folder)"),
        }
        eprintln!(
            "  mode:        {}",
            if opts.no_overwrite || settings.no_overwrite.unwrap_or(false) {
                "create-only"
            } else {
                "overwrite"
            }
        );
        return Ok(ObjectOutcome::DryRun(uri));
    }

    let graph_id = crate::store::ObjectName::id_of(&name.to_string()).to_string();
    let spec = crate::store::PutSpec {
        create_only: opts.no_overwrite || settings.no_overwrite.unwrap_or(false),
        metadata: vec![
            ("toolpath-graph-id", graph_id),
            ("toolpath-sha256", sha256.clone()),
            ("toolpath-uploader", crate::export_ledger::uploader()),
            (
                "toolpath-cli-version",
                env!("CARGO_PKG_VERSION").to_string(),
            ),
            ("toolpath-uploaded-at", chrono::Utc::now().to_rfc3339()),
        ],
    };
    let outcome = uri.put(settings, body.as_bytes(), &spec)?;
    crate::export_ledger::record(
        &crate::export_ledger::ledger_path()?,
        &dest.to_string(),
        ledger_key,
        crate::export_ledger::ExportRecord {
            uri: uri.to_string(),
            sha256,
            bytes: body.len() as u64,
            uploaded_at: chrono::Utc::now(),
            uploader: crate::export_ledger::uploader(),
        },
    )?;
    if outcome.replaced {
        eprintln!("Replaced {} bytes → {uri}", body.len());
    } else {
        eprintln!("Uploaded {} bytes → {uri}", body.len());
    }
    if outcome.created_dirs > 0 {
        eprintln!("note: created {dest}");
    }
    Ok(ObjectOutcome::Uploaded(uri))
}

/// Parse and schema-check the bytes about to be uploaded, and name the
/// object from the parsed document. A body that is not a valid toolpath
/// document is an error — a bucket of "traces" must not quietly collect
/// whatever file was passed — unless `force`, which warns and falls back
/// to naming the object after the file.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn object_name_for(
    body: &str,
    source: &std::path::Path,
    force: bool,
) -> Result<crate::store::ObjectName> {
    let checked = toolpath::v1::Graph::from_json(body)
        .map_err(|e| anyhow::anyhow!("{} is not a toolpath document: {e}", source.display()))
        .and_then(|doc| {
            let value: serde_json::Value = serde_json::from_str(body)?;
            crate::schema::validate(&value).map_err(|e| {
                anyhow::anyhow!("{} is not a valid toolpath document: {e}", source.display())
            })?;
            Ok(doc)
        });
    match checked {
        Ok(doc) => {
            let name = crate::store::name_for(&doc);
            let stem = name.to_string();
            if crate::store::ObjectName::id_of(&stem).is_empty() {
                anyhow::bail!(
                    "{} has a graph id with no usable characters; cannot name the object",
                    source.display()
                );
            }
            Ok(name)
        }
        Err(e) if force => {
            eprintln!("warning: {e:#}; uploading anyway (--force)");
            let stem = source
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "document".to_string());
            Ok(crate::store::ObjectName::new(&stem, None, None))
        }
        Err(e) => Err(e),
    }
}

fn run_pathbase(args: PathbaseExportArgs) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = args;
        anyhow::bail!("'path export pathbase' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        use crate::cmd_pathbase::preflight_auth;

        let file = cache_ref(&args.input)?;
        let body = std::fs::read_to_string(&file)
            .with_context(|| format!("Failed to read {}", file.display()))?;
        let upload = PathbaseUploadArgs {
            url: args.url,
            anon: args.anon,
            repo: args.repo,
            name: args.name,
            public: args.public,
        };
        let base_url = resolve_upload_base_url(&upload);
        let needs_auth = upload.repo.is_some() || upload.public || upload.name.is_some();
        let auth = preflight_auth(&base_url, upload.anon, needs_auth)?;
        let summary_source = file.display().to_string();
        run_pathbase_inner(auth, base_url, upload, &body, &summary_source)
    }
}

/// Resolve the upload target URL from the CLI flag, the stored session,
/// or the default. Mirrors the order used inside `run_pathbase_inner` so
/// `cmd_share`'s pre-flight resolution agrees with the eventual upload.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn resolve_upload_base_url(args: &PathbaseUploadArgs) -> String {
    use crate::cmd_pathbase::{credentials_path, load_session, resolve_url};

    if let Some(u) = &args.url {
        return resolve_url(Some(u.clone()));
    }
    if let Ok(path) = credentials_path()
        && let Ok(Some(s)) = load_session(&path)
    {
        return s.url;
    }
    resolve_url(None)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn run_pathbase_inner(
    auth: crate::cmd_pathbase::AuthMode,
    base_url: String,
    args: PathbaseUploadArgs,
    body: &str,
    summary_source: &str,
) -> Result<()> {
    use crate::cmd_pathbase::{AuthMode, anon_graphs_post, graphs_post, repos_post};
    use pathbase_client::types::Visibility;

    // Validate locally so we give a clean error rather than relying on
    // the server to reject malformed payloads.
    let doc = toolpath::v1::Graph::from_json(body)
        .map_err(|e| anyhow::anyhow!("Invalid toolpath document: {}", e))?;

    let (token, username) = match auth {
        AuthMode::Anon => {
            let resp = anon_graphs_post(&base_url, body)?;
            let printable = if resp.url.starts_with("http://") || resp.url.starts_with("https://") {
                resp.url.clone()
            } else if resp.url.starts_with('/') {
                format!("{base_url}{}", resp.url)
            } else {
                format!("{base_url}/{}", resp.url)
            };
            // Summary first on stderr, then the URL on stdout — the
            // share URL is the primary product, so it's the last line
            // the user (or a script piping the output) sees.
            eprintln!(
                "Uploaded {} → anon graph {} ({} bytes)",
                summary_source,
                resp.id,
                body.len()
            );
            println!("{printable}");
            return Ok(());
        }
        AuthMode::Authed { token, username } => (token, username),
    };

    let (owner, repo) = match args.repo {
        Some(spec) => (spec.owner, spec.name),
        None => {
            // Pathstash default: own the repo "pathstash" under the username
            // we resolved during preflight. Create it on demand.
            repos_post(&base_url, &token, &username, "pathstash")?;
            (username, "pathstash".to_string())
        }
    };

    let name = args.name.or_else(|| Some(derive_name(&doc)));
    let created = graphs_post(
        &base_url,
        &token,
        &owner,
        &repo,
        name.as_deref(),
        body,
        args.public,
    )?;

    // The visibility we surface is what the server actually applied,
    // not what we requested. If server-side policy ever clamps the
    // request, we render the form the graph can actually be reached at.
    let requested = if args.public {
        Visibility::Public
    } else {
        Visibility::Unlisted
    };
    if created.visibility != requested {
        eprintln!(
            "note: requested visibility={requested} but server applied visibility={}",
            created.visibility
        );
    }
    // Summary first on stderr, URL last on stdout — same ordering as
    // the anon path so the share URL is consistently the final line.
    eprintln!(
        "Uploaded {} → {}/{}/graphs/{} ({}, {} bytes)",
        summary_source,
        owner,
        repo,
        created.id,
        created.visibility,
        body.len()
    );
    println!("{}", created.url);
    Ok(())
}

/// Default display label for a graph uploaded via `export pathbase`.
///
/// Sanitize the inner id (Path id / Graph id) into `[a-z0-9_-]`-only
/// form, lower-cased — close to a URL slug, even though the server
/// addresses graphs by UUID and never reads this back from the wire.
/// Fallback (id sanitizes to empty — non-ascii, all punctuation, …):
/// hash the canonical JSON and use a short hex prefix so re-uploads
/// of the same content produce the same display label.
#[cfg(not(target_os = "emscripten"))]
fn derive_name(doc: &toolpath::v1::Graph) -> String {
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
    if !trimmed.is_empty() {
        return trimmed;
    }

    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(doc).unwrap_or_default();
    let hex = format!("{:x}", Sha256::digest(&bytes));
    format!("path-{}", &hex[..12])
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::*;
    use crate::projection::test_support::make_convo_path;
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

        run_claude(ClaudeExportArgs {
            input: input_path.to_string_lossy().to_string(),
            output: Some(output_path.clone()),
            ..Default::default()
        })
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

        let err = run_claude(ClaudeExportArgs {
            input: input_path.to_string_lossy().to_string(),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("single-path graph"));
    }

    #[test]
    fn claude_invalid_json_errors() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        std::fs::write(&input_path, "not json").unwrap();
        let err = run_claude(ClaudeExportArgs {
            input: input_path.to_string_lossy().to_string(),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("parse") || err.to_string().contains("Failed"));
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

    /// Runs `p export claude --output` with `args` on `doc` and parses
    /// the lines.
    fn export_claude_lines(
        doc: &toolpath::v1::Graph,
        args: ClaudeExportArgs,
    ) -> Vec<serde_json::Value> {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let output_path = temp.path().join("out.jsonl");
        std::fs::write(&input_path, serde_json::to_string(doc).unwrap()).unwrap();
        run_claude(ClaudeExportArgs {
            input: input_path.to_string_lossy().to_string(),
            output: Some(output_path.clone()),
            ..args
        })
        .unwrap();
        std::fs::read_to_string(&output_path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn values_of<'a>(lines: &'a [serde_json::Value], key: &str) -> Vec<&'a str> {
        lines.iter().filter_map(|v| v.get(key)?.as_str()).collect()
    }

    #[test]
    fn session_id_flag_stamps_the_given_id() {
        let doc = make_path_doc();
        let plain = export_claude_lines(&doc, ClaudeExportArgs::default());
        let source_ids = values_of(&plain, "sessionId");
        assert_eq!(
            source_ids.len(),
            plain.len(),
            "every line carries a sessionId"
        );

        let given = "402a3ca5-2530-407e-9029-f96879a0b1c2";
        assert!(!source_ids.contains(&given));
        let renamed = export_claude_lines(
            &doc,
            ClaudeExportArgs {
                session_id: Some(given.to_string()),
                ..Default::default()
            },
        );
        assert_eq!(renamed.len(), plain.len());
        let ids = values_of(&renamed, "sessionId");
        assert_eq!(ids.len(), source_ids.len());
        assert!(ids.iter().all(|s| *s == given));
    }

    #[test]
    fn new_session_id_flag_mints_a_distinct_id_per_export() {
        let doc = make_path_doc();
        let plain = export_claude_lines(&doc, ClaudeExportArgs::default());
        let source_ids = values_of(&plain, "sessionId");
        let args = || ClaudeExportArgs {
            new_session_id: true,
            ..Default::default()
        };
        let first = export_claude_lines(&doc, args());
        let second = export_claude_lines(&doc, args());

        let id_of = |lines: &[serde_json::Value]| {
            let ids = values_of(lines, "sessionId");
            assert_eq!(ids.len(), source_ids.len());
            let id = ids[0].to_string();
            assert!(ids.iter().all(|s| *s == id), "one ID on every line");
            let parsed = uuid::Uuid::parse_str(&id).unwrap();
            assert_eq!(parsed.get_version_num(), 4);
            assert!(!source_ids.contains(&id.as_str()));
            id
        };
        assert_ne!(id_of(&first), id_of(&second));
    }

    /// Parses `p export claude --input x <extra>` the way the binary
    /// does, so the test sees clap's value parsers and conflicts.
    fn parse_export_claude(extra: &[&str]) -> Result<(), clap::Error> {
        use clap::Parser;
        #[derive(Parser, Debug)]
        struct Cli {
            #[command(subcommand)]
            cmd: ExportTarget,
        }
        Cli::try_parse_from(
            ["test", "claude", "--input", "x"]
                .into_iter()
                .chain(extra.iter().copied()),
        )
        .map(|_| ())
    }

    #[test]
    fn session_id_flag_takes_only_a_uuid() {
        let given = "402a3ca5-2530-407e-9029-f96879a0b1c2";
        assert!(parse_export_claude(&["--session-id", given]).is_ok());
        assert!(
            parse_export_claude(&["--session-id", "my-template"]).is_err(),
            "clap must reject a session ID that is not a UUID"
        );
    }

    #[test]
    fn session_id_and_new_session_id_are_mutually_exclusive() {
        let given = "402a3ca5-2530-407e-9029-f96879a0b1c2";
        assert!(parse_export_claude(&["--new-session-id"]).is_ok());
        assert!(
            parse_export_claude(&["--session-id", given, "--new-session-id"]).is_err(),
            "clap must reject simultaneous --session-id and --new-session-id"
        );
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
    fn pi_writes_resume_ready_layout() {
        // Build a Path doc with a single user turn, run `path export
        // pi --project DIR`, and confirm the JSONL lands at the
        // expected `~/.pi/agent/sessions/--<encoded>--/<id>.jsonl`
        // location and re-parses through Pi's own reader.
        use toolpath_pi::{PathResolver, PiConvo};

        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let project_dir = temp.path().join("myproj");
        std::fs::create_dir_all(&project_dir).unwrap();

        let session_uuid = "pi-session-test-1";
        let artifact = format!("pi://{}", session_uuid);
        let mut extra = HashMap::new();
        extra.insert("role".into(), serde_json::json!("user"));
        extra.insert("text".into(), serde_json::json!("Hello pi"));
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
        let graph = toolpath::v1::Graph::from_path(toolpath::v1::Path {
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
        std::fs::write(&input_path, graph.to_json().unwrap()).unwrap();

        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &fake_home);
        }
        let result = run_pi(
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
        result.expect("export pi");

        let canon_project = std::fs::canonicalize(&project_dir).unwrap();
        let resolver = PathResolver::new().with_home(&fake_home);
        let project_dir_path = resolver.project_dir(canon_project.to_str().unwrap());
        let expected = project_dir_path.join(format!("{}.jsonl", session_uuid));
        assert!(expected.exists(), "expected JSONL at {:?}", expected);

        let convo = PiConvo::with_resolver(resolver);
        let session = convo
            .read_session(canon_project.to_str().unwrap(), session_uuid)
            .expect("Pi reader accepts our output");
        assert_eq!(session.header.id, session_uuid);
        assert_eq!(session.header.cwd, canon_project.to_string_lossy());

        let user_texts: Vec<String> = session
            .entries
            .iter()
            .filter_map(|e| match e {
                toolpath_pi::Entry::Message {
                    message: toolpath_pi::AgentMessage::User { content, .. },
                    ..
                } => match content {
                    toolpath_pi::types::MessageContent::Text(s) => Some(s.clone()),
                    toolpath_pi::types::MessageContent::Blocks(blocks) => Some(
                        blocks
                            .iter()
                            .filter_map(|b| match b {
                                toolpath_pi::ContentBlock::Text { text, .. } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                },
                _ => None,
            })
            .collect();
        assert_eq!(user_texts, vec!["Hello pi".to_string()]);
    }

    #[test]
    fn pi_rejects_non_single_path_graph() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let empty_graph = serde_json::json!({
            "graph": { "id": "g1" },
            "paths": [],
        });
        std::fs::write(&input_path, empty_graph.to_string()).unwrap();

        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let err = run_pi(
            input_path.to_string_lossy().to_string(),
            Some(project),
            None,
        )
        .expect_err("should reject empty graph");
        assert!(err.to_string().contains("single-path"));
    }

    #[test]
    fn pi_output_to_file_writes_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let out_path = temp.path().join("out").join("pi.jsonl");

        let session_uuid = "pi-out-test";
        let artifact = format!("pi://{}", session_uuid);
        let mut extra = HashMap::new();
        extra.insert("role".into(), serde_json::json!("user"));
        extra.insert("text".into(), serde_json::json!("hi"));
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
        let graph = toolpath::v1::Graph::from_path(toolpath::v1::Path {
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
        std::fs::write(&input_path, graph.to_json().unwrap()).unwrap();

        run_pi(
            input_path.to_string_lossy().to_string(),
            None,
            Some(out_path.clone()),
        )
        .expect("export pi --output");

        assert!(out_path.exists(), "output file missing");
        let body = std::fs::read_to_string(&out_path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert!(lines.len() >= 2);
        assert!(lines[0].contains("\"type\":\"session\""));
        assert!(lines[1].contains("\"role\":\"user\""));

        let session = toolpath_pi::reader::read_session_from_file(&out_path)
            .expect("Pi reader accepts the JSONL");
        assert_eq!(session.header.id, session_uuid);
    }

    #[test]
    fn codex_output_to_file_writes_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let out_path = temp.path().join("out").join("codex.jsonl");

        let session_uuid = "019dabc6-8fef-7681-a054-b5bb75fcb97d";
        let artifact = format!("codex://{}", session_uuid);
        let mut extra = HashMap::new();
        extra.insert("role".into(), serde_json::json!("user"));
        extra.insert("text".into(), serde_json::json!("hello codex"));
        let step = Step {
            step: StepIdentity {
                id: "step-001".into(),
                parents: vec![],
                actor: "human:alex".into(),
                timestamp: "2026-04-20T16:00:00Z".into(),
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
        let graph = toolpath::v1::Graph::from_path(toolpath::v1::Path {
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
        std::fs::write(&input_path, graph.to_json().unwrap()).unwrap();

        run_codex(
            input_path.to_string_lossy().to_string(),
            None,
            Some(out_path.clone()),
        )
        .expect("export codex --output");

        assert!(out_path.exists(), "output file missing");
        let body = std::fs::read_to_string(&out_path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 4, "got {} lines: {:?}", lines.len(), lines);
        assert!(lines[0].contains("\"type\":\"session_meta\""));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[0]).unwrap()["payload"]["id"].as_str(),
            Some(session_uuid)
        );
        assert!(lines[1].contains("\"type\":\"turn_context\""));
        assert!(lines[2].contains("\"type\":\"response_item\""));
        assert!(lines[3].contains("\"type\":\"event_msg\""));
        assert!(lines[3].contains("\"type\":\"user_message\""));

        let reread = toolpath_codex::RolloutReader::read_session(&out_path)
            .expect("Codex reader accepts the JSONL");
        assert_eq!(reread.id, session_uuid);
        assert_eq!(reread.lines.len(), 4);
    }

    #[test]
    fn codex_writes_into_dated_sessions_dir_with_project() {
        // `--project DIR` mode writes to
        // `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. The
        // resolver's HOME-based default is overridden via $HOME.
        use toolpath_codex::PathResolver;

        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let project_dir = temp.path().join("myproj");
        std::fs::create_dir_all(&project_dir).unwrap();

        let session_uuid = "019dabc6-aaaa-bbbb-cccc-ddddeeeefff0";
        let artifact = format!("codex://{}", session_uuid);
        let mut extra = HashMap::new();
        extra.insert("role".into(), serde_json::json!("user"));
        extra.insert("text".into(), serde_json::json!("hi codex via project"));
        let step = Step {
            step: StepIdentity {
                id: "step-001".into(),
                parents: vec![],
                actor: "human:alex".into(),
                timestamp: "2026-05-15T10:30:00.000Z".into(),
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
        let graph = toolpath::v1::Graph::from_path(toolpath::v1::Path {
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
        std::fs::write(&input_path, graph.to_json().unwrap()).unwrap();

        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &fake_home);
        }
        let result = run_codex(
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
        result.expect("export codex --project");

        let resolver = PathResolver::new().with_home(&fake_home);
        let dated_dir = resolver
            .sessions_root()
            .unwrap()
            .join("2026")
            .join("05")
            .join("15");
        assert!(
            dated_dir.exists(),
            "expected dated sessions dir at {}",
            dated_dir.display()
        );
        let rollout_files: Vec<PathBuf> = std::fs::read_dir(&dated_dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|s| s.starts_with("rollout-") && s.ends_with(".jsonl"))
            })
            .collect();
        assert_eq!(rollout_files.len(), 1, "expected one rollout-*.jsonl");

        let name = rollout_files[0]
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap();
        assert!(name.contains("2026-05-15T10-30-00"), "got name: {}", name);
        assert!(
            name.contains("019dabc6"),
            "filename should embed the uuid prefix; got {}",
            name
        );

        let reread =
            toolpath_codex::RolloutReader::read_session(&rollout_files[0]).expect("read back");
        assert_eq!(reread.id, session_uuid);
    }

    #[test]
    fn codex_rejects_non_single_path_graph() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let empty_graph = serde_json::json!({
            "graph": { "id": "g1" },
            "paths": [],
        });
        std::fs::write(&input_path, empty_graph.to_string()).unwrap();

        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let err = run_codex(
            input_path.to_string_lossy().to_string(),
            Some(project),
            None,
        )
        .expect_err("should reject empty graph");
        assert!(err.to_string().contains("single-path"));
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
            name: None,
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
    fn derive_name_uses_path_id() {
        let doc = make_path_doc();
        assert_eq!(derive_name(&doc), "test-path");
    }

    #[test]
    fn derive_name_sanitizes_non_url_safe_chars() {
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
        assert_eq!(derive_name(&doc), "claude-path-42");
    }

    #[test]
    fn derive_name_falls_back_to_content_hash_when_id_empties_out() {
        // An id consisting entirely of non-url-safe characters sanitizes to
        // empty; we fall back to a deterministic content-hash label.
        use toolpath::v1::{Graph, Path, PathIdentity};
        let doc = Graph::from_path(Path {
            path: PathIdentity {
                id: "✨🚀🦀".into(),
                base: None,
                head: "h".into(),
                graph_ref: None,
            },
            steps: vec![],
            meta: None,
        });
        let s1 = derive_name(&doc);
        let s2 = derive_name(&doc);
        assert_eq!(s1, s2, "fallback name must be deterministic across calls");
        assert!(s1.starts_with("path-"), "got {s1}");
        assert_eq!(s1.len(), "path-".len() + 12, "got {s1}");
        assert!(
            s1.chars().skip(5).all(|c| c.is_ascii_hexdigit()),
            "got {s1}"
        );
    }

    #[test]
    fn derive_name_fallback_differs_across_documents() {
        use toolpath::v1::{Graph, Path, PathIdentity};
        let mk = |head: &str| {
            Graph::from_path(Path {
                path: PathIdentity {
                    id: "—".into(), // sanitizes to empty for both
                    base: None,
                    head: head.into(),
                    graph_ref: None,
                },
                steps: vec![],
                meta: None,
            })
        };
        assert_ne!(derive_name(&mk("a")), derive_name(&mk("b")));
    }
    #[test]
    fn opencode_output_to_file_writes_session_json() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let out_path = temp.path().join("session.json");

        std::fs::write(
            &input_path,
            serde_json::to_string(&make_path_doc()).unwrap(),
        )
        .unwrap();

        run_opencode(
            input_path.to_string_lossy().to_string(),
            None,
            Some(out_path.clone()),
        )
        .unwrap();

        assert!(out_path.exists());
        let body = std::fs::read_to_string(&out_path).unwrap();
        let session: toolpath_opencode::Session = serde_json::from_str(&body).unwrap();
        assert!(session.id.starts_with("ses_"));
        assert_eq!(session.messages.len(), 1);
        assert!(matches!(
            session.messages[0].data,
            toolpath_opencode::MessageData::User(_)
        ));
    }

    #[test]
    fn opencode_writes_into_db_with_project() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let project_dir = temp.path().join("myproj");
        std::fs::create_dir_all(&project_dir).unwrap();
        let data_dir = fake_home.join(".local/share/opencode");
        std::fs::create_dir_all(&data_dir).unwrap();

        let conn = rusqlite::Connection::open(data_dir.join("opencode.db")).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE project (
              id text PRIMARY KEY, worktree text NOT NULL, vcs text, name text,
              icon_url text, icon_color text,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              time_initialized integer, sandboxes text NOT NULL, commands text
            );
            CREATE TABLE session (
              id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
              slug text NOT NULL, directory text NOT NULL, title text NOT NULL,
              version text NOT NULL, share_url text,
              summary_additions integer, summary_deletions integer,
              summary_files integer, summary_diffs text, revert text, permission text,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              time_compacting integer, time_archived integer, workspace_id text
            );
            CREATE TABLE message (
              id text PRIMARY KEY, session_id text NOT NULL,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              data text NOT NULL
            );
            CREATE TABLE part (
              id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              data text NOT NULL
            );
            "#,
        )
        .unwrap();
        drop(conn);

        let input_path = temp.path().join("input.json");
        std::fs::write(
            &input_path,
            serde_json::to_string(&make_path_doc()).unwrap(),
        )
        .unwrap();

        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let prev_xdg = std::env::var_os("XDG_DATA_HOME");
        unsafe {
            std::env::set_var("HOME", &fake_home);
            std::env::remove_var("XDG_DATA_HOME");
        }
        let result = run_opencode(
            input_path.to_string_lossy().to_string(),
            Some(project_dir.clone()),
            None,
        );
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
        result.expect("export opencode --project");

        let conn = rusqlite::Connection::open(data_dir.join("opencode.db")).unwrap();
        let session_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM session", [], |r| r.get(0))
            .unwrap();
        assert_eq!(session_count, 1);
        let message_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM message", [], |r| r.get(0))
            .unwrap();
        assert!(message_count >= 1);
        let part_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM part", [], |r| r.get(0))
            .unwrap();
        assert!(part_count >= 1);
    }

    #[test]
    fn opencode_rejects_non_single_path_graph() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let empty_graph = serde_json::json!({
            "graph": { "id": "g1" },
            "paths": [],
        });
        std::fs::write(&input_path, empty_graph.to_string()).unwrap();

        let err = run_opencode(input_path.to_string_lossy().to_string(), None, None).unwrap_err();
        assert!(err.to_string().contains("single-path"));
    }

    #[test]
    fn export_claude_refuses_existing_session_without_force() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let session_id = "claude-force-test-session";
        let path = make_convo_path(&format!("claude-code://{}", session_id));
        let input_path = temp.path().join("input.json");
        let doc = toolpath::v1::Graph::from_path(path);
        std::fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();
        let input = input_path.to_string_lossy().to_string();

        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &fake_home);
        }
        let export = |input: String, force: bool| {
            run_claude(ClaudeExportArgs {
                input,
                project: Some(cwd.clone()),
                force,
                ..Default::default()
            })
        };
        let first = export(input.clone(), false);
        let second = export(input.clone(), false);
        let forced = export(input, true);
        unsafe {
            match prior_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }

        first.expect("first export should succeed");
        let err = second.expect_err("re-export without --force must fail");
        assert!(
            err.to_string().contains("--force"),
            "unhelpful error: {err}"
        );
        forced.expect("re-export with --force should succeed");
    }

    #[cfg(feature = "resume-remote")]
    mod resume_remote {
        use super::*;
        use crate::claude_session::generate_content_addressed_session_id;
        use crate::cmd_export::remote_session::RemoteSessionArgs;

        /// `make_path_doc` with `cwd` recorded on every step, plus one
        /// headerless line that carries a `cwd`.
        fn make_path_doc_with_cwd(cwd: &str) -> toolpath::v1::Graph {
            let mut path = make_path_doc().into_single_path().unwrap();
            for step in &mut path.steps {
                for change in step.change.values_mut() {
                    if let Some(structural) = change.structural.as_mut() {
                        structural
                            .extra
                            .insert("cwd".to_string(), serde_json::json!(cwd));
                    }
                }
            }
            let artifact_key = path.steps[0].change.keys().next().unwrap().clone();
            let mut extra = HashMap::new();
            extra.insert("entry_type".to_string(), serde_json::json!("custom-title"));
            extra.insert(
                "raw".to_string(),
                serde_json::json!({"type": "custom-title", "cwd": cwd, "customTitle": "x"}),
            );
            path.steps.push(Step {
                step: StepIdentity {
                    id: "step-003".to_string(),
                    parents: vec!["step-002".to_string()],
                    actor: "tool:claude-code".to_string(),
                    timestamp: "2024-01-01T00:00:02Z".to_string(),
                },
                change: HashMap::from([(
                    artifact_key,
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.event".to_string(),
                            extra,
                        }),
                    },
                )]),
                meta: None,
            });
            path.path.head = "step-003".to_string();
            toolpath::v1::Graph::from_path(path)
        }

        #[test]
        fn cwd_flag_rewrites_every_cwd() {
            let doc = make_path_doc_with_cwd("/old/project");
            let plain = export_claude_lines(&doc, ClaudeExportArgs::default());
            let old = values_of(&plain, "cwd");
            assert!(!old.is_empty());
            assert!(old.iter().all(|c| *c == "/old/project"));

            let rooted = export_claude_lines(
                &doc,
                ClaudeExportArgs {
                    remote: RemoteSessionArgs {
                        cwd: Some("/new/dir".to_string()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            assert_eq!(rooted.len(), plain.len());
            let new = values_of(&rooted, "cwd");
            assert_eq!(new.len(), old.len());
            assert!(new.iter().all(|c| *c == "/new/dir"));
            let preamble = rooted
                .iter()
                .find(|v| v["type"] == "custom-title")
                .expect("the headerless line survives export");
            assert_eq!(preamble["cwd"], "/new/dir");
        }

        #[test]
        fn cwd_flag_leaves_session_ids_alone() {
            let doc = make_path_doc_with_cwd("/old/project");
            let plain = export_claude_lines(&doc, ClaudeExportArgs::default());
            let rooted = export_claude_lines(
                &doc,
                ClaudeExportArgs {
                    remote: RemoteSessionArgs {
                        cwd: Some("/new/dir".to_string()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            assert_eq!(
                values_of(&plain, "sessionId"),
                values_of(&rooted, "sessionId")
            );
        }

        #[test]
        fn content_addressed_session_id_excludes_the_other_naming_flags() {
            let given = "402a3ca5-2530-407e-9029-f96879a0b1c2";
            assert!(parse_export_claude(&["--content-addressed-session-id"]).is_ok());
            for extra in [
                ["--content-addressed-session-id", "--new-session-id"].as_slice(),
                ["--content-addressed-session-id", "--session-id", given].as_slice(),
            ] {
                assert!(
                    parse_export_claude(extra).is_err(),
                    "clap must reject --content-addressed-session-id with {extra:?}"
                );
            }
        }

        #[test]
        fn content_addressed_session_id_flag_stamps_the_content_addressed_id() {
            let doc = make_path_doc();
            let plain = export_claude_lines(&doc, ClaudeExportArgs::default());
            let source_ids = values_of(&plain, "sessionId");
            assert_eq!(
                source_ids.len(),
                plain.len(),
                "every line carries a sessionId"
            );

            let expected =
                generate_content_addressed_session_id(&serde_json::to_string(&doc).unwrap())
                    .unwrap();
            assert!(!source_ids.contains(&expected.as_str()));
            let addressed = export_claude_lines(
                &doc,
                ClaudeExportArgs {
                    remote: RemoteSessionArgs {
                        content_addressed_session_id: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            assert_eq!(addressed.len(), plain.len());
            let ids = values_of(&addressed, "sessionId");
            assert_eq!(ids.len(), source_ids.len());
            assert!(ids.iter().all(|s| *s == expected));
        }

        #[test]
        fn cwd_flag_does_not_change_the_content_addressed_id() {
            let doc = make_path_doc_with_cwd("/old/project");
            let addressed = export_claude_lines(
                &doc,
                ClaudeExportArgs {
                    remote: RemoteSessionArgs {
                        content_addressed_session_id: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            let rerooted = export_claude_lines(
                &doc,
                ClaudeExportArgs {
                    remote: RemoteSessionArgs {
                        content_addressed_session_id: true,
                        cwd: Some("/new/dir".to_string()),
                    },
                    ..Default::default()
                },
            );
            assert_eq!(
                values_of(&addressed, "sessionId"),
                values_of(&rerooted, "sessionId")
            );
        }

        #[test]
        fn content_addressed_export_names_the_project_file() {
            let temp = tempfile::tempdir().unwrap();
            let fake_home = temp.path().join("home");
            std::fs::create_dir_all(&fake_home).unwrap();
            let cwd = temp.path().join("proj");
            std::fs::create_dir_all(&cwd).unwrap();

            let path = make_convo_path("claude-code://claude-addressed-file-test-session");
            let input_path = temp.path().join("input.json");
            let doc = toolpath::v1::Graph::from_path(path);
            std::fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();
            let args = ClaudeExportArgs {
                input: input_path.to_string_lossy().to_string(),
                project: Some(cwd.clone()),
                remote: RemoteSessionArgs {
                    content_addressed_session_id: true,
                    cwd: None,
                },
                ..Default::default()
            };

            let _g = crate::config::TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let prior_home = std::env::var_os("HOME");
            unsafe {
                std::env::set_var("HOME", &fake_home);
            }
            let result = run_claude(args);
            unsafe {
                match prior_home {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }

            result.expect("content-addressed export should succeed");
            let expected = generate_content_addressed_session_id(
                &std::fs::read_to_string(&input_path).unwrap(),
            )
            .unwrap();
            let canon = std::fs::canonicalize(&cwd).unwrap();
            let file = toolpath_claude::PathResolver::new()
                .with_home(&fake_home)
                .project_dir(canon.to_str().unwrap())
                .unwrap()
                .join(format!("{expected}.jsonl"));
            assert!(file.is_file(), "{}", file.display());
        }
    }
}
