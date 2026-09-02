//! `path resume <input>` — fetch / load a Toolpath document, pick an
//! installed coding-agent harness, project the session into that
//! harness's on-disk layout, and exec the harness's resume command.
//!
//! ## Inputs
//!
//! `<input>` is resolved in this order:
//! 1. `https://` / `http://` URL → fetched via `pathbase-client`,
//!    cached unless `--no-cache`.
//! 2. `owner/repo/slug` shorthand → same Pathbase fetch flow.
//! 3. Existing file path → read directly.
//! 4. Otherwise treated as a cache id under `~/.toolpath/documents/`.
//!
//! ## Harness selection
//!
//! With `--harness X`, `X` is validated against `$PATH` and used.
//! Without `--harness`, an `fzf` picker shows installed harnesses
//! with the source harness pre-selected. Source comes from
//! `path.meta.source` (`claude-code`, `gemini-cli`, `codex`,
//! `opencode`, `pi`) with actor-string fallback.
//!
//! ## Project directory
//!
//! `-C / --cwd P` overrides the shell cwd. The harness is exec'd
//! with cwd set to P and the on-disk projection is keyed on P.
//!
//! ## Launch
//!
//! On Unix the harness binary is `execvp`'d, replacing the current
//! process. On Windows it's spawned and waited on with the exit
//! code propagated. If `exec` itself fails (e.g. the binary disappears
//! between PATH check and exec), the recipe is printed to stderr.
//!
//! Exec is mockable via [`ExecStrategy`]: production uses [`RealExec`],
//! integration tests use [`RecordingExec`] to capture
//! `(binary, args, cwd)` without launching anything.
//!
//! See `docs/superpowers/specs/2026-05-08-path-resume-command-design.md`
//! for the full design.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;

#[cfg(feature = "resume-remote")]
mod remote;

use crate::harness::Harness;

#[derive(Args, Debug, Default)]
pub struct ResumeArgs {
    /// Toolpath document to resume from. Accepted shapes: a Pathbase
    /// URL (`https://host/owner/repo/slug`), a bare Pathbase shorthand
    /// (`owner/repo/slug`), a path to a local toolpath JSON file, or a
    /// cache id (e.g. `claude-abc`, `pathbase-foo-bar-baz`).
    pub input: String,

    /// Working directory to run the resumed harness from. Defaults to
    /// the current shell cwd. The on-disk projection is keyed on this
    /// directory and the harness will be exec'd with cwd set to it.
    #[arg(short = 'C', long)]
    pub cwd: Option<PathBuf>,

    /// Pin the resume target. Skips the interactive picker.
    #[arg(long, value_enum)]
    pub harness: Option<Harness>,

    /// Skip the cache entirely when fetching from Pathbase: don't read
    /// an existing entry, don't write the fetched body. Useful for
    /// ephemeral environments where you don't want the cache to grow.
    #[arg(long)]
    pub no_cache: bool,

    /// Force a re-fetch from Pathbase even if a cache entry exists,
    /// overwriting it with the new bytes. Default behavior is to use
    /// the cached doc on hit and never round-trip.
    #[arg(long)]
    pub force: bool,

    /// Pathbase server URL. Falls back to the stored session's URL,
    /// then `$PATHBASE_URL`, then `https://pathbase.dev`.
    #[arg(long)]
    pub url: Option<String>,

    #[cfg(feature = "resume-remote")]
    /// Plan the resume on this ssh destination instead of this
    /// machine (`user@host`; Claude only). With `--remote`,
    /// `-C` names the remote project directory; default: the local cwd
    /// with the local home swapped for the remote home. Read-only for
    /// now: the command stops after the plan.
    #[arg(long, value_parser = crate::ssh::Destination::parse)]
    pub remote: Option<crate::ssh::Destination>,

    #[cfg(feature = "resume-remote")]
    /// Stop after printing the plan. Only with --remote.
    #[arg(long, requires = "remote")]
    pub dry_run: bool,
}

pub fn run(args: ResumeArgs) -> Result<()> {
    run_with_strategy(args, &RealExec)
}

/// Internal entry point that the integration tests call with a
/// `RecordingExec` strategy. Production callers use [`run`].
pub fn run_with_strategy(args: ResumeArgs, exec: &dyn ExecStrategy) -> Result<()> {
    let resolved = resolve_input(&args)?;
    #[cfg(feature = "resume-remote")]
    if let Some(dest) = &args.remote {
        let home = crate::config::home_dir();
        let transport = crate::ssh::Russh::new(
            std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
            home.as_deref()
                .context("cannot determine the home directory")?
                .join(".ssh"),
        )?;
        return run_remote_with_transport(
            &args,
            dest,
            &resolved,
            &transport,
            home.as_deref(),
            &std::env::current_dir()?,
        );
    }
    let ResolvedInput {
        graph,
        source_harness,
        ..
    } = resolved;
    let path = ensure_path_with_agent(&graph)?;

    let cwd = match args.cwd.as_ref() {
        Some(p) => {
            std::fs::canonicalize(p).with_context(|| format!("resolve cwd path {}", p.display()))?
        }
        None => std::env::current_dir()?,
    };

    let target = pick_harness(args.harness, source_harness, None)?;
    eprintln!(
        "Picked harness: {}{}",
        target.name(),
        if Some(target) == source_harness {
            " (source)"
        } else {
            ""
        }
    );

    let session_id = project_into_harness(path, target, &cwd)?;
    let (binary, argv) = invocation_for(target, &session_id, &cwd);
    exec_harness(&binary, &argv, &cwd, exec)
}

use toolpath::v1::{Graph, Path as TPath, PathOrRef};

#[cfg(feature = "resume-remote")]
/// `--remote`: validate the input the same way a local resume does,
/// then hand the document text to the remote planner. Claude only.
fn run_remote_with_transport(
    args: &ResumeArgs,
    dest: &crate::ssh::Destination,
    resolved: &ResolvedInput,
    transport: &dyn crate::ssh::Transport,
    local_home: Option<&std::path::Path>,
    local_cwd: &std::path::Path,
) -> Result<()> {
    ensure_path_with_agent(&resolved.graph)?;
    match (args.harness, resolved.source_harness) {
        (Some(Harness::Claude), _) | (None, Some(Harness::Claude)) => {}
        (Some(h), _) => anyhow::bail!(
            "remote resume supports claude only (got --harness {})",
            h.name()
        ),
        (None, source) => anyhow::bail!(
            "remote resume supports claude only; the document's source is {}. \
             Pass `--harness claude` to force a Claude projection.",
            source.map_or("unknown", |h| h.name())
        ),
    }
    let remote_dir_flag = args
        .cwd
        .as_deref()
        .map(|p| p.to_str().context("-C must be valid UTF-8"))
        .transpose()?;
    remote::run_remote(
        &resolved.json,
        dest,
        remote_dir_flag,
        args.dry_run,
        transport,
        local_home,
        local_cwd,
    )
}

/// Read a path's source harness from `meta.source` (set by
/// `toolpath-convo::derive_path` to the provider id), falling back to
/// actor-string sniffing across the path's steps.
pub(crate) fn infer_source_harness(path: &TPath) -> Option<Harness> {
    let meta_source = path.meta.as_ref().and_then(|m| m.source.as_deref());
    if let Some(source) = meta_source {
        match source {
            "claude-code" => return Some(Harness::Claude),
            "gemini-cli" => return Some(Harness::Gemini),
            "codex" => return Some(Harness::Codex),
            "copilot" => return Some(Harness::Copilot),
            "opencode" => return Some(Harness::Opencode),
            "cursor" => return Some(Harness::Cursor),
            "pi" => return Some(Harness::Pi),
            _ => {} // fall through to actor sniffing
        }
    }
    for step in &path.steps {
        let actor = &step.step.actor;
        if actor.starts_with("agent:claude-code") {
            return Some(Harness::Claude);
        }
        if actor.starts_with("agent:gemini-cli") || actor.starts_with("agent:gemini") {
            return Some(Harness::Gemini);
        }
        if actor.starts_with("agent:codex") {
            return Some(Harness::Codex);
        }
        if actor.starts_with("agent:copilot") {
            return Some(Harness::Copilot);
        }
        if actor.starts_with("agent:opencode") {
            return Some(Harness::Opencode);
        }
        if actor.starts_with("agent:cursor") {
            return Some(Harness::Cursor);
        }
        if actor.starts_with("agent:pi") {
            return Some(Harness::Pi);
        }
    }
    None
}

/// Validate that a parsed Toolpath document is a single inline Path
/// carrying at least one `agent:*` actor. Returns the inner Path borrow
/// on success.
pub(crate) fn ensure_path_with_agent(g: &Graph) -> Result<&TPath> {
    if g.paths.is_empty() {
        anyhow::bail!("resume needs a `Path`; expected one path, got an empty graph");
    }
    if g.paths.len() > 1 {
        anyhow::bail!(
            "resume needs a single `Path`; input is a graph with {} paths. \
             Pick one with `path query …` or split first.",
            g.paths.len()
        );
    }
    let path = match &g.paths[0] {
        PathOrRef::Path(p) => p.as_ref(),
        PathOrRef::Ref(_) => anyhow::bail!(
            "resume needs an inline `Path`; got a $ref. Resolve it first with `path import` or fetch the document."
        ),
    };
    let has_agent = path
        .steps
        .iter()
        .any(|s| s.step.actor.starts_with("agent:"));
    if !has_agent {
        anyhow::bail!(
            "no agent session in input — `path resume` only works on harness-derived paths"
        );
    }
    Ok(path)
}

/// A resolved input: the parsed document, its source harness, and
/// the JSON text it was parsed from. The text is kept because the
/// remote session ID hashes the document text, not a type
/// round-trip.
#[derive(Debug)]
pub(crate) struct ResolvedInput {
    pub(crate) graph: Graph,
    pub(crate) source_harness: Option<Harness>,
    #[cfg(feature = "resume-remote")]
    pub(crate) json: String,
}

/// Resolve the user-supplied `<input>` argument into a
/// [`ResolvedInput`]: the parsed `Graph` plus the source harness
/// inferred from its single inline path (if any). See spec § "Input
/// resolution" for the order.
pub(crate) fn resolve_input(args: &ResumeArgs) -> Result<ResolvedInput> {
    let raw = args.input.as_str();

    enum Shape<'a> {
        PathbaseUrl(&'a str),
        PathbaseShorthand(&'a str),
        FilePath(&'a str),
        CacheId(&'a str),
    }

    let shape = if raw.starts_with("http://") || raw.starts_with("https://") {
        Shape::PathbaseUrl(raw)
    } else if looks_like_pathbase_shorthand(raw) {
        Shape::PathbaseShorthand(raw)
    } else if std::path::Path::new(raw).is_file() {
        Shape::FilePath(raw)
    } else {
        Shape::CacheId(raw)
    };

    let (json, source) = match shape {
        Shape::PathbaseUrl(u) | Shape::PathbaseShorthand(u) => {
            // Probe the local cache before going to the network. The cache
            // id is purely a function of the parsed (owner, repo, id), so
            // we can compute it without fetching. `--force` skips the probe
            // and re-fetches; `--no-cache` skips both the probe AND the
            // post-fetch write (still useful for ephemeral environments).
            let (_, ref_) = crate::derive::parse_pathbase_ref(u, args.url.as_deref())?;
            let cache_id = crate::cache::pathbase_cache_id(&ref_.owner, &ref_.repo, &ref_.id);
            if !args.force
                && !args.no_cache
                && let Ok(cache_path) = crate::cache::cache_path(&cache_id)
                && cache_path.exists()
            {
                let json = std::fs::read_to_string(&cache_path)
                    .with_context(|| format!("read {}", cache_path.display()))?;
                eprintln!("Resolved {} → {} (cached)", raw, cache_id);
                (json, format!("cache entry {}", cache_path.display()))
            } else {
                let derived = crate::derive::pathbase_fetch_to_doc(u, args.url.as_deref())?;
                if !args.no_cache {
                    // force=true here: we either short-circuited above
                    // (cache miss) or the user explicitly passed --force,
                    // and either way we want the new bytes to land.
                    crate::cache::write_cached(&derived.cache_id, &derived.doc, true)?;
                    eprintln!("Resolved {} → {}", raw, derived.cache_id);
                }
                let json = derived
                    .doc
                    .to_json()
                    .context("serialize the fetched document")?;
                (json, "fetched from Pathbase".to_string())
            }
        }
        Shape::FilePath(p) => (
            std::fs::read_to_string(p).with_context(|| format!("read {}", p))?,
            format!("file {p}"),
        ),
        Shape::CacheId(id) => {
            let file = crate::cache::cache_ref(id).map_err(|e| {
                anyhow::anyhow!(
                    "couldn't resolve `{}` as a URL, file path, or cache id: {}",
                    raw,
                    e
                )
            })?;
            let json = std::fs::read_to_string(&file)
                .with_context(|| format!("read {}", file.display()))?;
            (json, format!("cache entry {}", file.display()))
        }
    };

    let graph = Graph::from_json(&json)
        .map_err(|e| anyhow::anyhow!("not a valid toolpath document ({source}): {e}"))?;
    let source_harness = graph.single_path().and_then(infer_source_harness);
    Ok(ResolvedInput {
        graph,
        source_harness,
        #[cfg(feature = "resume-remote")]
        json,
    })
}

/// Probe `$PATH` (or `path_override`, for tests) for a given binary name.
/// Cross-platform: on Windows, also tries `<name>.exe`.
pub(crate) fn binary_on_path(name: &str, path_override: Option<&std::path::Path>) -> bool {
    let dirs: Vec<std::path::PathBuf> = match path_override {
        Some(p) => vec![p.to_path_buf()],
        None => std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default(),
    };
    for d in dirs {
        let candidate = d.join(name);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            let exe = d.join(format!("{name}.exe"));
            if exe.is_file() {
                return true;
            }
        }
    }
    false
}

/// Cursor is special: the `cursor` CLI shim must be installed
/// explicitly from the IDE's command palette, but `open -a Cursor`
/// (macOS) / `xdg-open` (Linux) always work. Treat cursor as available
/// when either path is open.
pub(crate) fn harness_available(harness: Harness, path_override: Option<&std::path::Path>) -> bool {
    if binary_on_path(harness.name(), path_override) {
        return true;
    }
    if harness == Harness::Cursor {
        #[cfg(target_os = "macos")]
        {
            return binary_on_path("open", path_override);
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            return binary_on_path("xdg-open", path_override);
        }
    }
    false
}

/// Decide which harness to resume in.
///
/// - If `arg` is `Some`, validate the named harness is on PATH and return it.
/// - Otherwise, enumerate installed harnesses and launch the fzf picker.
///   `source` is used to label the source row in the picker UI.
///
/// `path_override` is `None` in production; tests pass `Some(dir)` to fake `$PATH`.
pub(crate) fn pick_harness(
    arg: Option<Harness>,
    source: Option<Harness>,
    path_override: Option<&std::path::Path>,
) -> Result<Harness> {
    if let Some(h) = arg {
        if !harness_available(h, path_override) {
            anyhow::bail!(
                "harness `{}` isn't on PATH; install it or pick another with `--harness`",
                h.name()
            );
        }
        return Ok(h);
    }

    let installed: Vec<Harness> = Harness::ALL
        .iter()
        .copied()
        .filter(|h| harness_available(*h, path_override))
        .collect();

    if installed.is_empty() {
        anyhow::bail!(
            "no installed harnesses found on PATH; install one of: claude, gemini, codex, opencode, cursor, pi"
        );
    }

    interactive_pick(&installed, source)
}

fn interactive_pick(installed: &[Harness], source: Option<Harness>) -> Result<Harness> {
    if !crate::fuzzy::available() {
        let hint = if crate::fuzzy::embedded_picker_available() {
            "rerun in a terminal"
        } else {
            "install `fzf` (or build with the default `embedded-picker` feature) and rerun in a terminal"
        };
        anyhow::bail!("interactive picker requires a TTY; pass `--harness <X>` or {hint}");
    }
    let mut lines: Vec<String> = Vec::with_capacity(installed.len());
    for h in installed {
        let suffix = if Some(*h) == source { "  (source)" } else { "" };
        lines.push(format!("{}{}", h.padded_name(), suffix));
    }

    let header = match source {
        Some(s) => format!("pick a harness to resume in (source: {})", s.name()),
        None => "pick a harness to resume in".to_string(),
    };

    let opts = crate::fuzzy::PickOptions {
        with_nth: "1..",
        header: Some(&header),
        ..Default::default()
    };
    let selected = match crate::fuzzy::pick(&lines, &opts)
        .map_err(|e| anyhow::anyhow!("fzf failed: {}", e))?
    {
        crate::fuzzy::PickResult::Selected(rows) => rows.into_iter().next().unwrap_or_default(),
        crate::fuzzy::PickResult::Cancelled => std::process::exit(130),
        crate::fuzzy::PickResult::NoMatch => {
            anyhow::bail!("fzf returned no match — picker UI was empty?");
        }
    };

    let picked_name = selected.split_whitespace().next().unwrap_or_default();
    for h in installed {
        if picked_name == h.name() {
            return Ok(*h);
        }
    }
    anyhow::bail!("picker returned an unrecognized row: {selected}")
}

/// Static map from harness to resume-argv shape. Lives here because
/// it's a per-harness CLI convention, not a projection concern.
pub(crate) fn argv_for(harness: Harness, session_id: &str) -> Vec<String> {
    match harness {
        Harness::Claude => vec!["-r".into(), session_id.into()],
        Harness::Gemini => vec!["--resume".into(), session_id.into()],
        Harness::Codex => vec!["resume".into(), session_id.into()],
        Harness::Copilot => vec!["--resume".into(), session_id.into()],
        Harness::Opencode => vec!["--session".into(), session_id.into()],
        // Cursor.app has no "open composer by id" flag — we exec the
        // workspace path so Cursor opens on that folder; the projected
        // composer appears at the top of the chat list.
        Harness::Cursor => {
            let _ = session_id;
            vec![".".into()]
        }
        Harness::Pi => vec!["--session".into(), session_id.into()],
    }
}

pub(crate) fn invocation_for(
    harness: Harness,
    session_id: &str,
    cwd: &std::path::Path,
) -> (String, Vec<String>) {
    if harness == Harness::Cursor {
        return cursor_invocation(cwd);
    }
    (harness.name().to_string(), argv_for(harness, session_id))
}

fn cursor_invocation(cwd: &std::path::Path) -> (String, Vec<String>) {
    let workspace = cwd.to_string_lossy().into_owned();
    if binary_on_path("cursor", None) {
        ("cursor".to_string(), vec![workspace])
    } else {
        #[cfg(target_os = "macos")]
        {
            (
                "open".to_string(),
                vec!["-a".into(), "Cursor".into(), workspace],
            )
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            ("xdg-open".to_string(), vec![workspace])
        }
        #[cfg(not(unix))]
        {
            ("cursor".to_string(), vec![workspace])
        }
    }
}

/// Project a Path into the chosen harness's on-disk layout under `cwd`,
/// returning the projected session id.
pub(crate) fn project_into_harness(
    path: &TPath,
    harness: Harness,
    cwd: &std::path::Path,
) -> Result<String> {
    match harness {
        Harness::Claude => match crate::cmd_export::project_claude(path, cwd)? {
            crate::cmd_export::ClaudeProjection::Written { session_id } => Ok(session_id),
            crate::cmd_export::ClaudeProjection::AlreadyLocal { session_id } => {
                eprintln!(
                    "Session {session_id} already exists in this project; resuming the local copy."
                );
                Ok(session_id)
            }
        },
        Harness::Gemini => crate::cmd_export::project_gemini(path, cwd),
        Harness::Codex => crate::cmd_export::project_codex(path, cwd),
        Harness::Copilot => crate::cmd_export::project_copilot(path, cwd),
        Harness::Opencode => crate::cmd_export::project_opencode(path, cwd),
        Harness::Cursor => crate::cmd_export::project_cursor(path, cwd),
        Harness::Pi => crate::cmd_export::project_pi(path, cwd),
    }
}

/// What `exec_harness` saw (for tests).
#[derive(Debug, Clone, Default)]
pub struct CapturedExec {
    pub binary: String,
    pub args: Vec<String>,
    pub cwd: std::path::PathBuf,
}

/// Pluggable exec backend. Production uses `RealExec` (`execvp` on
/// Unix, spawn-and-wait on Windows). Tests use `RecordingExec`.
pub trait ExecStrategy {
    fn exec(&self, binary: &str, args: &[String], cwd: &std::path::Path) -> Result<()>;
}

/// Production implementation. On Unix this never returns on success
/// (the current process is replaced); on Windows it spawns the child,
/// waits, and propagates the exit code.
pub struct RealExec;

impl ExecStrategy for RealExec {
    fn exec(&self, binary: &str, args: &[String], cwd: &std::path::Path) -> Result<()> {
        let mut cmd = std::process::Command::new(binary);
        cmd.args(args);
        cmd.current_dir(cwd);

        eprintln!(
            "Resuming: {} {} (cwd: {})",
            binary,
            args.join(" "),
            cwd.display()
        );

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // exec only returns if it fails.
            let err = cmd.exec();
            anyhow::bail!(
                "couldn't exec `{}`: {}. Recipe: {} {} (run from {})",
                binary,
                err,
                binary,
                args.join(" "),
                cwd.display()
            );
        }
        #[cfg(not(unix))]
        {
            let status = cmd
                .spawn()
                .with_context(|| format!("spawn {}", binary))?
                .wait()
                .with_context(|| format!("wait for {}", binary))?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }
}

/// Recording strategy for tests. `captured()` returns the most recent
/// invocation.
#[derive(Default)]
pub struct RecordingExec {
    inner: std::sync::Mutex<CapturedExec>,
}

impl RecordingExec {
    pub fn captured(&self) -> CapturedExec {
        self.inner.lock().unwrap().clone()
    }
}

impl ExecStrategy for RecordingExec {
    fn exec(&self, binary: &str, args: &[String], cwd: &std::path::Path) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        *g = CapturedExec {
            binary: binary.to_string(),
            args: args.to_vec(),
            cwd: cwd.to_path_buf(),
        };
        Ok(())
    }
}

pub(crate) fn exec_harness(
    binary: &str,
    args: &[String],
    cwd: &std::path::Path,
    strategy: &dyn ExecStrategy,
) -> Result<()> {
    strategy.exec(binary, args, cwd)
}

fn looks_like_pathbase_shorthand(s: &str) -> bool {
    // Three non-empty slash-separated segments, none containing whitespace
    // or starting with a dot/slash (which would indicate a relative or
    // absolute path).
    if s.starts_with('.') || s.starts_with('/') {
        return false;
    }
    let segs: Vec<&str> = s.split('/').collect();
    segs.len() == 3
        && segs
            .iter()
            .all(|s| !s.is_empty() && !s.contains(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "resume-remote")]
    #[test]
    fn remote_rejects_a_non_claude_harness_before_any_remote_call() {
        let mut path = make_convo_path_for_resume("codex://remote-test-session");
        path.steps[0].step.actor = "agent:codex".to_string();
        let graph = toolpath::v1::Graph::from_path(path);
        let resolved = ResolvedInput {
            json: graph.to_json().unwrap(),
            graph,
            source_harness: Some(Harness::Codex),
        };
        let args = |harness: Option<Harness>| ResumeArgs {
            input: "unused".to_string(),
            harness,
            dry_run: true,
            ..Default::default()
        };
        let dest = crate::ssh::Destination::parse("user@host").unwrap();
        let fake = crate::ssh::fake::FakeSsh::new();
        let run = |harness| {
            run_remote_with_transport(
                &args(harness),
                &dest,
                &resolved,
                &fake,
                None,
                std::path::Path::new("/"),
            )
        };

        let err = run(None).unwrap_err();
        assert!(err.to_string().contains("supports claude only"), "{err:#}");
        assert!(err.to_string().contains("--harness claude"), "{err:#}");

        let err = run(Some(Harness::Codex)).unwrap_err();
        assert!(err.to_string().contains("got --harness codex"), "{err:#}");
        assert!(
            fake.calls().is_empty(),
            "no remote call before the harness check"
        );
    }

    #[test]
    fn run_with_strategy_records_invocation_for_file_input_with_explicit_harness() {
        let _env = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _home = scoped_home_for_resume();
        let _path_guard = ScopedPathForResume::with_binaries(&["claude"]);
        let cwd = tempfile::tempdir().unwrap();
        let doc_file = cwd.path().join("doc.json");

        // Build a minimal path with a conversation.append step that
        // project_claude can consume, reusing the existing helper.
        let mut path = make_convo_path_for_resume("claude-code://resume-test-session");
        // Overwrite the actor to agent:claude-code so run_with_strategy can
        // pass the ensure_path_with_agent check.
        path.steps[0].step.actor = "agent:claude-code".to_string();

        let graph = toolpath::v1::Graph::from_path(path);
        std::fs::write(&doc_file, graph.to_json().unwrap()).unwrap();

        let args = ResumeArgs {
            input: doc_file.to_string_lossy().to_string(),
            cwd: Some(cwd.path().to_path_buf()),
            harness: Some(Harness::Claude),
            ..Default::default()
        };

        let recorder = RecordingExec::default();
        run_with_strategy(args, &recorder).unwrap();

        let cap = recorder.captured();
        assert_eq!(cap.binary, "claude");
        assert_eq!(cap.args[0], "-r");
        assert_eq!(cap.cwd, std::fs::canonicalize(cwd.path()).unwrap());
    }

    use toolpath::v1::{Graph, PathMeta, PathOrRef};

    fn make_step_with_actor(id: &str, actor: &str) -> toolpath::v1::Step {
        toolpath::v1::Step::new(id, actor, "2026-01-01T00:00:00Z")
            .with_raw_change("src/main.rs", "@@ -1 +1 @@\n-old\n+new")
    }

    fn make_path_with_actor(actor: &str) -> toolpath::v1::Path {
        use toolpath::v1::{Path, PathIdentity};
        let step = make_step_with_actor("s1", actor);
        Path {
            path: PathIdentity {
                id: "p1".to_string(),
                base: None,
                head: "s1".to_string(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        }
    }

    #[test]
    fn infer_source_harness_meta_source_wins() {
        let mut path = make_path_with_actor("agent:codex");
        path.meta = Some(PathMeta {
            source: Some("claude-code".to_string()),
            ..Default::default()
        });
        assert_eq!(infer_source_harness(&path), Some(Harness::Claude));
    }

    #[test]
    fn infer_source_harness_meta_source_unknown_falls_through_to_actor() {
        let mut path = make_path_with_actor("agent:gemini-cli");
        path.meta = Some(PathMeta {
            source: Some("something-bespoke".to_string()),
            ..Default::default()
        });
        assert_eq!(infer_source_harness(&path), Some(Harness::Gemini));
    }

    #[test]
    fn infer_source_harness_actor_sniff_codex() {
        let path = make_path_with_actor("agent:codex");
        assert_eq!(infer_source_harness(&path), Some(Harness::Codex));
    }

    #[test]
    fn infer_source_harness_actor_sniff_opencode() {
        let path = make_path_with_actor("agent:opencode");
        assert_eq!(infer_source_harness(&path), Some(Harness::Opencode));
    }

    #[test]
    fn infer_source_harness_actor_sniff_pi() {
        let path = make_path_with_actor("agent:pi");
        assert_eq!(infer_source_harness(&path), Some(Harness::Pi));
    }

    #[test]
    fn infer_source_harness_returns_none_when_no_signal() {
        let path = make_path_with_actor("human:alex");
        assert_eq!(infer_source_harness(&path), None);
    }

    #[test]
    fn ensure_path_with_agent_accepts_single_path_with_agent_actor() {
        let g = Graph::from_path(make_path_with_actor("agent:claude-code"));
        assert!(ensure_path_with_agent(&g).is_ok());
    }

    #[test]
    fn ensure_path_with_agent_rejects_empty_graph() {
        let mut g = Graph::from_path(make_path_with_actor("agent:claude-code"));
        g.paths.clear();
        let err = ensure_path_with_agent(&g).unwrap_err();
        assert!(err.to_string().contains("expected"));
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn ensure_path_with_agent_rejects_multi_path_graph() {
        let mut g = Graph::from_path(make_path_with_actor("agent:claude-code"));
        g.paths.push(PathOrRef::Path(Box::new(make_path_with_actor(
            "agent:claude-code",
        ))));
        let err = ensure_path_with_agent(&g).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("single `Path`"), "actual: {s}");
        assert!(s.contains("2 paths"), "actual: {s}");
    }

    #[test]
    fn ensure_path_with_agent_rejects_agentless_path() {
        let g = Graph::from_path(make_path_with_actor("human:alex"));
        let err = ensure_path_with_agent(&g).unwrap_err();
        assert!(err.to_string().contains("no agent session"));
    }

    #[test]
    fn ensure_path_with_agent_rejects_path_ref_only_graph() {
        use toolpath::v1::PathRef;
        let mut g = Graph::from_path(make_path_with_actor("agent:claude-code"));
        g.paths = vec![PathOrRef::Ref(PathRef {
            ref_url: "$ref://something".into(),
        })];
        let err = ensure_path_with_agent(&g).unwrap_err();
        assert!(err.to_string().contains("inline `Path`"), "actual: {}", err);
    }

    #[test]
    fn resolve_input_file_path() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("doc.json");
        let graph = toolpath::v1::Graph::from_path(make_path_with_actor("agent:claude-code"));
        std::fs::write(&p, graph.to_json().unwrap()).unwrap();

        let args = ResumeArgs {
            input: p.to_string_lossy().to_string(),
            cwd: None,
            harness: None,
            ..Default::default()
        };
        let ResolvedInput {
            graph: g,
            source_harness: harness,
            ..
        } = resolve_input(&args).unwrap();
        let _path = ensure_path_with_agent(&g).unwrap();
        assert_eq!(harness, Some(Harness::Claude));
    }

    #[test]
    fn resolve_input_url_dispatches_to_pathbase_fetch() {
        let _env = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        use crate::cmd_pathbase::tests::MockServer;
        let body = {
            let mut path = make_path_with_actor("agent:codex");
            path.meta = Some(toolpath::v1::PathMeta {
                source: Some("codex".to_string()),
                ..Default::default()
            });
            toolpath::v1::Graph::from_path(path).to_json().unwrap()
        };
        // MockServer::start requires &'static str — leak the body to satisfy this.
        let body_static: &'static str = Box::leak(body.into_boxed_str());
        let server = MockServer::start("HTTP/1.1 200 OK", body_static);

        let args = ResumeArgs {
            input: format!(
                "{}/u/alex/repos/pathstash/graphs/fe94b6f9-b0af-4cdd-b9ca-3c9a2a697537",
                server.base()
            ),
            cwd: None,
            harness: None,
            no_cache: true, // skip cache write in tests
            ..Default::default()
        };
        let ResolvedInput {
            graph: g,
            source_harness: harness,
            ..
        } = resolve_input(&args).unwrap();
        let _ = ensure_path_with_agent(&g).unwrap();
        assert_eq!(harness, Some(Harness::Codex));
    }

    #[test]
    fn resolve_input_url_uses_cache_on_hit_without_refetching() {
        // Regression for the second-invocation cache-hit error: re-running
        // `path resume <url>` should silently reuse the cached doc instead
        // of erroring. We seed the cache with a known-good doc, point the
        // input at a 500-erroring mock server (so any network round-trip
        // would surface as an error), and confirm resolve_input still
        // returns the cached graph.
        let _env = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Pin TOOLPATH_CONFIG_DIR to a tempdir so we don't pollute the
        // user's real cache.
        let cfg_dir = tempfile::tempdir().unwrap();
        let prev_cfg = std::env::var_os("TOOLPATH_CONFIG_DIR");
        unsafe {
            std::env::set_var("TOOLPATH_CONFIG_DIR", cfg_dir.path());
        }

        // Seed the cache with a codex-source graph. Cache id keys on the
        // graph UUID since Pathbase 1.1+ addresses graphs by UUID.
        const FIXTURE_UUID: &str = "fe94b6f9-b0af-4cdd-b9ca-3c9a2a697537";
        let cache_id = format!("pathbase-alex-pathstash-{FIXTURE_UUID}");
        let cache_id = cache_id.as_str();
        let documents = cfg_dir.path().join("documents");
        std::fs::create_dir_all(&documents).unwrap();
        let cached_graph = {
            let mut path = make_path_with_actor("agent:codex");
            path.meta = Some(toolpath::v1::PathMeta {
                source: Some("codex".to_string()),
                ..Default::default()
            });
            toolpath::v1::Graph::from_path(path)
        };
        std::fs::write(
            documents.join(format!("{cache_id}.json")),
            cached_graph.to_json().unwrap(),
        )
        .unwrap();

        // Mock server that 500s any request — proves we never call out.
        use crate::cmd_pathbase::tests::MockServer;
        let server = MockServer::start("HTTP/1.1 500 Internal Server Error", "boom");

        let args = ResumeArgs {
            input: format!(
                "{}/u/alex/repos/pathstash/graphs/{FIXTURE_UUID}",
                server.base()
            ),
            cwd: None,
            harness: None,
            ..Default::default()
        };
        let result = resolve_input(&args).map(|r| (r.graph, r.source_harness));

        // Restore env before asserting so a panic doesn't poison sibling tests.
        unsafe {
            match prev_cfg {
                Some(v) => std::env::set_var("TOOLPATH_CONFIG_DIR", v),
                None => std::env::remove_var("TOOLPATH_CONFIG_DIR"),
            }
        }

        let (g, harness) = result.expect("resolve_input should reuse cache without refetching");
        let _ = ensure_path_with_agent(&g).unwrap();
        assert_eq!(harness, Some(Harness::Codex));
    }

    #[test]
    fn resolve_input_unresolvable_errors_clearly() {
        let _env = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let args = ResumeArgs {
            input: "definitely/not/a/real/cache/id".to_string(),
            cwd: None,
            harness: None,
            ..Default::default()
        };
        let err = resolve_input(&args).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("couldn't resolve"), "actual: {s}");
    }

    fn fake_path_with(binaries: &[&str]) -> tempfile::TempDir {
        let td = tempfile::tempdir().unwrap();
        for b in binaries {
            let p = td.path().join(b);
            std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = std::fs::metadata(&p).unwrap().permissions();
                perm.set_mode(0o755);
                std::fs::set_permissions(&p, perm).unwrap();
            }
        }
        td
    }

    #[test]
    fn binary_on_path_finds_present_binary() {
        let td = fake_path_with(&["claude"]);
        assert!(binary_on_path("claude", Some(td.path())));
        assert!(!binary_on_path("gemini", Some(td.path())));
    }

    #[test]
    fn pick_harness_explicit_arg_validates_path() {
        let td = fake_path_with(&["claude"]);
        let result = pick_harness(Some(Harness::Claude), None, Some(td.path()));
        assert_eq!(result.unwrap(), Harness::Claude);

        let err = pick_harness(Some(Harness::Gemini), None, Some(td.path())).unwrap_err();
        assert!(err.to_string().contains("`gemini` isn't on PATH"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cursor_available_via_open_fallback_on_macos() {
        let td = fake_path_with(&["open"]);
        assert!(harness_available(Harness::Cursor, Some(td.path())));
        let picked = pick_harness(Some(Harness::Cursor), None, Some(td.path()));
        assert_eq!(picked.unwrap(), Harness::Cursor);
    }

    #[test]
    fn cursor_unavailable_when_no_launcher_at_all() {
        let td = fake_path_with(&["claude"]);
        assert!(!harness_available(Harness::Cursor, Some(td.path())));
    }

    #[test]
    fn cursor_invocation_includes_workspace_path() {
        let cwd = std::path::PathBuf::from("/tmp/some-workspace");
        let (binary, argv) = invocation_for(Harness::Cursor, "ignored-session-id", &cwd);
        assert!(
            argv.iter().any(|a| a == "/tmp/some-workspace"),
            "workspace path must appear in argv; got {argv:?}",
        );
        assert!(
            matches!(binary.as_str(), "cursor" | "open" | "xdg-open"),
            "expected cursor/open/xdg-open, got {binary:?}",
        );
    }

    #[test]
    fn pick_harness_zero_installed_errors() {
        let td = fake_path_with(&[]);
        let err = pick_harness(None, Some(Harness::Claude), Some(td.path())).unwrap_err();
        assert!(
            err.to_string().contains("no installed harnesses")
                || err.to_string().contains("no harnesses on PATH"),
            "actual: {}",
            err
        );
    }

    #[test]
    fn argv_for_returns_harness_specific_shape() {
        assert_eq!(
            argv_for(Harness::Claude, "abc"),
            vec!["-r".to_string(), "abc".to_string()]
        );
        assert_eq!(
            argv_for(Harness::Gemini, "abc"),
            vec!["--resume".to_string(), "abc".to_string()]
        );
        assert_eq!(
            argv_for(Harness::Codex, "abc"),
            vec!["resume".to_string(), "abc".to_string()]
        );
        assert_eq!(
            argv_for(Harness::Opencode, "abc"),
            vec!["--session".to_string(), "abc".to_string()]
        );
        assert_eq!(
            argv_for(Harness::Pi, "abc"),
            vec!["--session".to_string(), "abc".to_string()]
        );
    }

    #[test]
    fn project_into_harness_claude_round_trip() {
        let _env = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _home = scoped_home_for_resume();
        let cwd = tempfile::tempdir().unwrap();
        let path = make_convo_path_for_resume("claude-code://resume-test-session");

        let session_id = project_into_harness(&path, Harness::Claude, cwd.path()).unwrap();
        assert!(!session_id.is_empty());
    }

    /// Build a minimal `toolpath::v1::Path` with a single `conversation.append`
    /// step using the given `artifact_key` (e.g. `"claude-code://my-session"`).
    /// Required for projectors that extract the session id from the artifact key.
    fn make_convo_path_for_resume(artifact_key: &str) -> toolpath::v1::Path {
        use std::collections::HashMap;
        let mut extra = HashMap::new();
        extra.insert("role".to_string(), serde_json::json!("user"));
        extra.insert("text".to_string(), serde_json::json!("hello"));
        let step = toolpath::v1::Step {
            step: toolpath::v1::StepIdentity {
                id: "s1".to_string(),
                parents: vec![],
                actor: "human:test".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact_key.to_string(),
                    toolpath::v1::ArtifactChange {
                        raw: None,
                        structural: Some(toolpath::v1::StructuralChange {
                            change_type: "conversation.append".to_string(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };
        toolpath::v1::Path {
            path: toolpath::v1::PathIdentity {
                id: "test-path".to_string(),
                base: None,
                head: "s1".to_string(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        }
    }

    fn scoped_home_for_resume() -> ScopedHomeForResume {
        ScopedHomeForResume::new()
    }

    struct ScopedPathForResume {
        _bin_dir: tempfile::TempDir,
        prev: Option<std::ffi::OsString>,
    }

    impl ScopedPathForResume {
        /// Prepends a tempdir containing the named binaries to `PATH` for
        /// the guard's lifetime.
        fn with_binaries(binaries: &[&str]) -> Self {
            let bin_dir = fake_path_with(binaries);
            let prev = std::env::var_os("PATH");
            let new_path = std::env::join_paths(
                std::iter::once(bin_dir.path().to_path_buf())
                    .chain(std::env::split_paths(&prev.clone().unwrap_or_default())),
            )
            .unwrap();
            unsafe {
                std::env::set_var("PATH", new_path);
            }
            Self {
                _bin_dir: bin_dir,
                prev,
            }
        }
    }

    impl Drop for ScopedPathForResume {
        fn drop(&mut self) {
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var("PATH", v),
                    None => std::env::remove_var("PATH"),
                }
            }
        }
    }

    struct ScopedHomeForResume {
        _td: tempfile::TempDir,
        prev: Option<std::ffi::OsString>,
    }

    impl ScopedHomeForResume {
        fn new() -> Self {
            let td = tempfile::tempdir().unwrap();
            let prev = std::env::var_os("HOME");
            unsafe {
                std::env::set_var("HOME", td.path());
            }
            Self { _td: td, prev }
        }
    }

    impl Drop for ScopedHomeForResume {
        fn drop(&mut self) {
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    #[test]
    fn exec_strategy_recording_captures_invocation() {
        let recorder = RecordingExec::default();
        let strategy: &dyn ExecStrategy = &recorder;
        exec_harness(
            "claude",
            &["-r".into(), "abc123".into()],
            std::path::Path::new("/tmp/x"),
            strategy,
        )
        .unwrap();

        let captured = recorder.captured();
        assert_eq!(captured.binary, "claude");
        assert_eq!(captured.args, vec!["-r".to_string(), "abc123".to_string()]);
        assert_eq!(captured.cwd, std::path::PathBuf::from("/tmp/x"));
    }
}
