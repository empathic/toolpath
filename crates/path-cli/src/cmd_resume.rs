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
//! ## Remote (`--remote ssh://[user@]host[:port][/path]`)
//!
//! v3 (host projects, remote just receives files): the **host** resolves
//! the document AND projects the session fully in memory, then ships the
//! finished harness file to the remote over SFTP and launches the
//! harness — so the remote needs only SSH and the harness installed.
//! **No `path` on the remote**, no Pathbase access, no temp files, and
//! no composed remote shell strings: the file operations are typed SFTP
//! calls via libssh2 (matching the repo's `git2`-over-shelling-out
//! ethos). Steps ([`run_remote`]):
//!
//! 1. **Resolve + project on the host** — same `resolve_input` /
//!    `ensure_path_with_agent` as a local resume, so a bad or non-agent
//!    document fails fast on the host, not deep inside SSH. The session
//!    id and JSONL come from the same in-memory projection
//!    ([`crate::cmd_export::claude_session_jsonl`]).
//! 2. **Preflight** — resolve the remote home over SFTP
//!    ([`ExecStrategy::remote_home`]). First remote touch, so
//!    reachability/auth failures abort here rather than dropping the
//!    user into a doomed session; the home anchors the Claude layout
//!    and the default launch dir.
//! 3. **Ship** — SFTP `mkdir -p` + write of
//!    `<home>/.claude/projects/<dir>/<id>.jsonl`, where `<dir>` is the
//!    launch cwd run through Claude Code's own project-dir sanitization
//!    so `claude -r` started there finds the session. A pinned `--cwd`
//!    is also created over SFTP.
//! 4. **Launch** — `execvp` an interactive `ssh -t host '[cd <cwd> && ]
//!    claude -r <id>'`. The `-t` gives the remote harness a real
//!    terminal; this is the one step that stays on the real `ssh`
//!    binary (it needs the user's TTY and ssh config). libssh2 uses
//!    agent auth and does not read `~/.ssh/config`/`known_hosts`; the
//!    launch does.
//!
//! `--harness` is required with `--remote` (and currently must be
//! `claude` — the projection and layout knowledge are Claude-specific).
//! The resolution-only flags (`--no-cache`/`--force`/`--url`) act on the
//! host and are never forwarded. `--cwd` does double duty as the
//! project-dir key for the shipped file and the launch's `cd` target;
//! absent, both default to the remote's ssh cwd (`$HOME`).
//!
//! See `docs/superpowers/specs/2026-05-08-path-resume-command-design.md`
//! for the full design.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;

/// Re-exported so external callers (integration tests, future consumers)
/// can construct [`ResumeArgs`] without depending on the `cmd_share`
/// module directly.
pub use crate::cmd_share::HarnessArg;

#[derive(Args, Debug)]
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
    pub harness: Option<HarnessArg>,

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

    /// Resume on a remote host over SSH instead of locally. Takes a
    /// full SSH URL (`ssh://[user@]host[:port][/path]`). When set, the
    /// resume is dispatched to the remote host rather than exec'ing a
    /// local harness.
    #[arg(long)]
    pub remote: Option<String>,
}

pub fn run(args: ResumeArgs) -> Result<()> {
    run_with_strategy(args, &RealExec)
}

/// Internal entry point that the integration tests call with a
/// `RecordingExec` strategy. Production callers use [`run`].
pub fn run_with_strategy(args: ResumeArgs, exec: &dyn ExecStrategy) -> Result<()> {
    // Remote resume: resolve + project here, ship the finished session
    // file to the remote (no `path` needed there), and launch the harness
    // over an interactive SSH. See the module docs' "Remote" section.
    if let Some(remote) = args.remote.as_deref() {
        return run_remote(&args, remote, exec);
    }

    let (graph, source_harness) = resolve_input(&args)?;
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

/// Read a path's source harness from `meta.source` (set by
/// `toolpath-convo::derive_path` to the provider id), falling back to
/// actor-string sniffing across the path's steps.
pub(crate) fn infer_source_harness(path: &TPath) -> Option<crate::cmd_share::Harness> {
    use crate::cmd_share::Harness;
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

/// Resolve the user-supplied `<input>` argument into a parsed `Graph`
/// plus the source harness inferred from its single inline path (if
/// any). See spec § "Input resolution" for the order.
pub(crate) fn resolve_input(
    args: &ResumeArgs,
) -> Result<(Graph, Option<crate::cmd_share::Harness>)> {
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

    let graph: Graph = match shape {
        Shape::PathbaseUrl(u) | Shape::PathbaseShorthand(u) => {
            // Probe the local cache before going to the network. The cache
            // id is purely a function of (owner, repo, slug), so we can
            // compute it without fetching. `--force` skips the probe and
            // re-fetches; `--no-cache` skips both the probe AND the post-
            // fetch write (still useful for ephemeral environments).
            let cache_id = crate::cmd_import::pathbase_cache_id_of(u, args.url.as_deref())?;
            if !args.force
                && !args.no_cache
                && let Ok(cache_path) = crate::cmd_cache::cache_path(&cache_id)
                && cache_path.exists()
            {
                let json = std::fs::read_to_string(&cache_path)
                    .with_context(|| format!("read {}", cache_path.display()))?;
                eprintln!("Resolved {} → {} (cached)", raw, cache_id);
                Graph::from_json(&json)
                    .map_err(|e| anyhow::anyhow!("cached toolpath document is invalid: {}", e))?
            } else {
                let derived = crate::cmd_import::pathbase_fetch_to_doc(u, args.url.as_deref())?;
                if !args.no_cache {
                    // force=true here: we either short-circuited above
                    // (cache miss) or the user explicitly passed --force,
                    // and either way we want the new bytes to land.
                    crate::cmd_cache::write_cached(&derived.cache_id, &derived.doc, true)?;
                    eprintln!("Resolved {} → {}", raw, derived.cache_id);
                }
                derived.doc
            }
        }
        Shape::FilePath(p) => {
            let json = std::fs::read_to_string(p).with_context(|| format!("read {}", p))?;
            Graph::from_json(&json)
                .map_err(|e| anyhow::anyhow!("not a valid toolpath document: {}", e))?
        }
        Shape::CacheId(id) => {
            let file = crate::cmd_cache::cache_ref(id).map_err(|e| {
                anyhow::anyhow!(
                    "couldn't resolve `{}` as a URL, file path, or cache id: {}",
                    raw,
                    e
                )
            })?;
            let json = std::fs::read_to_string(&file)
                .with_context(|| format!("read {}", file.display()))?;
            Graph::from_json(&json)
                .map_err(|e| anyhow::anyhow!("not a valid toolpath document: {}", e))?
        }
    };

    let harness = graph.single_path().and_then(infer_source_harness);
    Ok((graph, harness))
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
pub(crate) fn harness_available(
    harness: crate::cmd_share::Harness,
    path_override: Option<&std::path::Path>,
) -> bool {
    use crate::cmd_share::Harness;
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

const ALL_HARNESSES: &[crate::cmd_share::Harness] = &[
    crate::cmd_share::Harness::Claude,
    crate::cmd_share::Harness::Gemini,
    crate::cmd_share::Harness::Codex,
    crate::cmd_share::Harness::Copilot,
    crate::cmd_share::Harness::Opencode,
    crate::cmd_share::Harness::Cursor,
    crate::cmd_share::Harness::Pi,
];

/// Decide which harness to resume in.
///
/// - If `arg` is `Some`, validate the named harness is on PATH and return it.
/// - Otherwise, enumerate installed harnesses and launch the fzf picker.
///   `source` is used to label the source row in the picker UI.
///
/// `path_override` is `None` in production; tests pass `Some(dir)` to fake `$PATH`.
pub(crate) fn pick_harness(
    arg: Option<HarnessArg>,
    source: Option<crate::cmd_share::Harness>,
    path_override: Option<&std::path::Path>,
) -> Result<crate::cmd_share::Harness> {
    use crate::cmd_share::Harness;

    if let Some(a) = arg {
        let h = Harness::from_arg(a);
        if !harness_available(h, path_override) {
            anyhow::bail!(
                "harness `{}` isn't on PATH; install it or pick another with `--harness`",
                h.name()
            );
        }
        return Ok(h);
    }

    let installed: Vec<Harness> = ALL_HARNESSES
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

fn interactive_pick(
    installed: &[crate::cmd_share::Harness],
    source: Option<crate::cmd_share::Harness>,
) -> Result<crate::cmd_share::Harness> {
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
        lines.push(format!("{}{}", h.symbol(), suffix));
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

    for h in installed {
        if selected.starts_with(h.symbol()) {
            return Ok(*h);
        }
    }
    anyhow::bail!("picker returned an unrecognized row: {selected}")
}

/// Static map from harness to resume-argv shape. Lives here because
/// it's a per-harness CLI convention, not a projection concern.
pub(crate) fn argv_for(harness: crate::cmd_share::Harness, session_id: &str) -> Vec<String> {
    use crate::cmd_share::Harness;
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
    harness: crate::cmd_share::Harness,
    session_id: &str,
    cwd: &std::path::Path,
) -> (String, Vec<String>) {
    use crate::cmd_share::Harness;
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
    harness: crate::cmd_share::Harness,
    cwd: &std::path::Path,
) -> Result<String> {
    use crate::cmd_share::Harness;
    match harness {
        Harness::Claude => crate::cmd_export::project_claude(path, cwd),
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

/// A parsed `ssh://[user@]host[:port][/path]` remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    /// Login user; falls back to `$USER` at connect time when absent.
    pub user: Option<String>,
    pub host: String,
    /// Explicit port from the URL; SSH's default 22 when absent.
    pub port: Option<u16>,
}

/// Pluggable exec + remote-transport backend. Production uses
/// `RealExec` (`execvp` on Unix, spawn-and-wait on Windows; SFTP via
/// libssh2 for the remote file operations — typed calls, not composed
/// shell strings). Tests use `RecordingExec`.
pub trait ExecStrategy {
    fn exec(&self, binary: &str, args: &[String], cwd: &std::path::Path) -> Result<()>;

    /// Resolve the remote login home directory. Doubles as the
    /// reachability/auth preflight: it's the first remote touch, so a
    /// bad host, port, or key fails here with context.
    fn remote_home(&self, target: &SshTarget) -> Result<String>;

    /// Create `dir` (and any missing parents) on the remote —
    /// `mkdir -p` semantics, existing directories are fine.
    fn remote_mkdirs(&self, target: &SshTarget, dir: &str) -> Result<()>;

    /// Write `data` to the absolute remote `path`, truncating any
    /// existing file. Parent directories must already exist.
    fn remote_write(&self, target: &SshTarget, path: &str, data: &[u8]) -> Result<()>;
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

    fn remote_home(&self, target: &SshTarget) -> Result<String> {
        let sftp = sftp_channel(target)?;
        let home = sftp
            .realpath(std::path::Path::new("."))
            .context("resolve remote home directory")?;
        Ok(home.to_string_lossy().to_string())
    }

    fn remote_mkdirs(&self, target: &SshTarget, dir: &str) -> Result<()> {
        let sftp = sftp_channel(target)?;
        // Walk the components, creating as we go — `mkdir -p` semantics.
        let mut cur = std::path::PathBuf::new();
        for comp in std::path::Path::new(dir).components() {
            cur.push(comp);
            if cur.parent().is_none() {
                continue; // skip the root component
            }
            if sftp.stat(&cur).is_ok() {
                continue; // already exists
            }
            sftp.mkdir(&cur, 0o755)
                .with_context(|| format!("create remote dir {}", cur.display()))?;
        }
        Ok(())
    }

    fn remote_write(&self, target: &SshTarget, path: &str, data: &[u8]) -> Result<()> {
        use std::io::Write;
        let sftp = sftp_channel(target)?;
        let mut f = sftp
            .create(std::path::Path::new(path))
            .with_context(|| format!("create remote file {path}"))?;
        f.write_all(data)
            .with_context(|| format!("write remote file {path}"))?;
        Ok(())
    }
}

/// Open an authenticated SFTP channel to `target`: TCP connect,
/// handshake, then SSH-agent auth as the URL's user (or `$USER`). Each
/// call opens a fresh connection — the remote flow makes a handful of
/// calls, so pooling isn't worth the state.
///
/// Note: libssh2 does not consult `~/.ssh/known_hosts` or
/// `~/.ssh/config`; auth is agent-only. The interactive launch still
/// goes through the real `ssh` binary with the user's full config.
fn sftp_channel(target: &SshTarget) -> Result<ssh2::Sftp> {
    let port = target.port.unwrap_or(22);
    let addr = format!("{}:{}", target.host, port);
    let tcp = std::net::TcpStream::connect(&addr).with_context(|| format!("connect to {addr}"))?;
    let mut sess = ssh2::Session::new().context("create SSH session")?;
    sess.set_tcp_stream(tcp);
    sess.handshake()
        .with_context(|| format!("SSH handshake with {addr}"))?;
    let user = match target.user.clone() {
        Some(u) => u,
        None => {
            std::env::var("USER").context("no SSH user: put `user@` in the URL or set $USER")?
        }
    };
    sess.userauth_agent(&user).with_context(|| {
        format!("SSH agent auth as `{user}` on {addr} — is the key loaded (`ssh-add`)?")
    })?;
    sess.sftp().context("open SFTP channel")
}

/// Recording strategy for tests. `captured()` returns the most recent
/// invocation; the remote-transport calls are recorded as typed values
/// (targets, dirs, `(path, contents)` pairs) instead of shell strings.
#[derive(Default)]
pub struct RecordingExec {
    inner: std::sync::Mutex<CapturedExec>,
    homes: std::sync::Mutex<Vec<SshTarget>>,
    mkdirs: std::sync::Mutex<Vec<String>>,
    /// Written files: (remote path, contents as UTF-8 string).
    writes: std::sync::Mutex<Vec<(String, String)>>,
    /// When true, `remote_home` returns an error — simulates an
    /// unreachable or unauthenticated remote.
    home_fails: bool,
}

impl RecordingExec {
    /// A recorder whose remote preflight fails, for exercising the
    /// abort-before-dispatch path.
    pub fn failing_remote() -> Self {
        Self {
            home_fails: true,
            ..Default::default()
        }
    }

    pub fn captured(&self) -> CapturedExec {
        self.inner.lock().unwrap().clone()
    }

    /// Every `remote_home` (preflight) call, in call order.
    pub fn homes(&self) -> Vec<SshTarget> {
        self.homes.lock().unwrap().clone()
    }

    /// Every `remote_mkdirs` call, in call order.
    pub fn mkdirs(&self) -> Vec<String> {
        self.mkdirs.lock().unwrap().clone()
    }

    /// Every `remote_write` call as `(remote path, contents)`.
    pub fn writes(&self) -> Vec<(String, String)> {
        self.writes.lock().unwrap().clone()
    }
}

/// The fake remote home `RecordingExec` reports; tests key expected
/// paths off it.
pub const RECORDING_REMOTE_HOME: &str = "/home/recording";

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

    fn remote_home(&self, target: &SshTarget) -> Result<String> {
        self.homes.lock().unwrap().push(target.clone());
        if self.home_fails {
            anyhow::bail!("connection refused");
        }
        Ok(RECORDING_REMOTE_HOME.to_string())
    }

    fn remote_mkdirs(&self, _target: &SshTarget, dir: &str) -> Result<()> {
        self.mkdirs.lock().unwrap().push(dir.to_string());
        Ok(())
    }

    fn remote_write(&self, _target: &SshTarget, path: &str, data: &[u8]) -> Result<()> {
        self.writes
            .lock()
            .unwrap()
            .push((path.to_string(), String::from_utf8_lossy(data).to_string()));
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

/// v2 remote resume: the host resolves the document locally, pipes the
/// JSON into `ssh host 'path p incept claude …'` (which hydrates the
/// session into the remote's Claude layout), then hands off to an
/// interactive `ssh -t host 'claude -r <id>'`. The remote needs `path`
/// (for incept) + the harness installed — but no Pathbase access, since
/// the host already resolved the doc.
///
/// The host knows `<id>` without asking the remote: the Claude session
/// id is a pure function of the document (the projector takes it
/// verbatim from the conversation view), so projecting the same bytes on
/// both sides yields the same id — see [`crate::cmd_export::claude_session_id`].
///
/// Steps:
/// 1. resolve + validate the doc on the host (fail fast on bad input),
///    and compute the session id locally;
/// 2. version preflight (`ssh host 'path --version'`), echoing both
///    versions and aborting if `path`/SSH is unreachable;
/// 3. hydrate: pipe the JSON into `path p incept claude [--project …]`;
/// 4. `execvp` the interactive `ssh -t … '[cd … && ]claude -r <id>'`.
///
/// `--cwd` does double duty: it is incept's `--project` dir AND the `cd`
/// target for the launch (they must match for `claude -r` to find the
/// session). Absent, both default to the remote's ssh cwd (`$HOME`).
fn run_remote(args: &ResumeArgs, remote: &str, exec: &dyn ExecStrategy) -> Result<()> {
    // The remote's interactive picker can't run from here, so pin the
    // harness explicitly. Only Claude is wired up so far: the host-side
    // projection and layout knowledge are Claude-specific.
    match args.harness {
        None => anyhow::bail!(
            "--remote requires --harness <X>: the host can't run the remote's \
             harness picker, so the target must be pinned"
        ),
        Some(HarnessArg::Claude) => {}
        Some(_) => anyhow::bail!("remote resume currently supports only --harness claude"),
    }

    // 1. Resolve + validate locally so a bad document fails on the host,
    //    not deep inside an SSH session — and project the session fully
    //    in memory: the remote never sees the toolpath document, only the
    //    finished harness file.
    let (graph, _source) = resolve_input(args)?;
    let path = ensure_path_with_agent(&graph)?;
    let (session_id, jsonl) = crate::cmd_export::claude_session_jsonl(path)?;
    let target = parse_ssh_url(remote)?;

    // 2. Preflight: resolve the remote home over SFTP. First remote
    //    touch, so reachability/auth failures surface here; the home also
    //    anchors the Claude layout and the default launch dir.
    let home = exec.remote_home(&target).with_context(|| {
        format!("probing remote over {remote} — is the host reachable over SSH?")
    })?;
    if home.is_empty() {
        anyhow::bail!("remote probe over {remote} returned no home directory");
    }
    eprintln!("remote {remote}: reachable (home {home})");

    // 3. Ship: create the Claude project dir and write the projected
    //    JSONL over SFTP — typed file operations, no remote shell. The
    //    project dir is keyed on the launch cwd (--cwd, else the remote
    //    home) with Claude Code's own sanitization, so `claude -r`
    //    started there finds the session.
    let project_path = match args.cwd.as_ref() {
        Some(dir) => dir.display().to_string(),
        None => home.clone(),
    };
    let dir_name = claude_project_dir_name(&project_path);
    let projects_dir = format!("{home}/.claude/projects/{dir_name}");
    exec.remote_mkdirs(&target, &projects_dir)
        .with_context(|| format!("creating {projects_dir} on {remote}"))?;
    let dest = format!("{projects_dir}/{session_id}.jsonl");
    exec.remote_write(&target, &dest, jsonl.as_bytes())
        .with_context(|| format!("shipping session file to {remote}:{dest}"))?;
    eprintln!("Shipped session {session_id} → {remote}:{dest}");

    // The launch cwd must exist before the interactive `cd` — create it
    // over SFTP too, since resuming into a fresh directory is the normal
    // case.
    if let Some(dir) = args.cwd.as_ref() {
        let dir = dir.display().to_string();
        exec.remote_mkdirs(&target, &dir)
            .with_context(|| format!("creating launch dir {dir} on {remote}"))?;
    }

    // 4. Interactive launch of the harness against the shipped session,
    //    with a real TTY — the one step that stays on the real `ssh`
    //    binary (it needs the user's terminal and ssh config).
    let launch_cmd = remote_launch_command(&session_id, args.cwd.as_deref());
    let (binary, argv) = ssh_invocation_tty(remote, &launch_cmd, true)?;
    let cwd = std::env::current_dir()?;
    exec_harness(&binary, &argv, &cwd, exec)
}

/// The name Claude Code gives a project's directory under
/// `~/.claude/projects/`. Host-side mirror of `toolpath-claude`'s
/// (private) `sanitize_project_path` — kept in sync by a unit test that
/// compares against `PathResolver::project_dir`.
fn claude_project_dir_name(project_path: &str) -> String {
    project_path.replace(['/', '_', '.'], "-")
}

/// The far-side launch command: `claude -r <id>`, prefixed with a
/// `cd <cwd> &&` when a cwd was pinned so the harness starts where the
/// shipped session is keyed. The directory itself is created over SFTP
/// before launch — this is the only remote shell string left, and it's
/// minimal because a `cd` can't happen anywhere else.
fn remote_launch_command(session_id: &str, cwd: Option<&std::path::Path>) -> String {
    let launch = format!("claude -r {}", shell_single_quote(session_id));
    match cwd {
        Some(dir) => format!(
            "cd {} && {launch}",
            shell_single_quote(&dir.display().to_string())
        ),
        None => launch,
    }
}

/// Parse a full SSH URL (`ssh://[user@]host[:port][/path]`) into a
/// typed [`SshTarget`]. The optional `/path` component is ignored.
fn parse_ssh_url(remote: &str) -> Result<SshTarget> {
    let rest = remote
        .strip_prefix("ssh://")
        .with_context(|| format!("remote must be a full SSH URL (ssh://…), got `{remote}`"))?;

    // Strip an optional `/path` component; the authority is everything
    // before the first slash.
    let authority = match rest.find('/') {
        Some(i) => &rest[..i],
        None => rest,
    };
    if authority.is_empty() {
        anyhow::bail!("SSH URL `{remote}` is missing a host");
    }

    // Split a trailing `:port` (all-digit) off the `[user@]host` part.
    let (userhost, port) =
        match authority.rsplit_once(':') {
            Some((uh, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (
                uh,
                Some(p.parse::<u16>().with_context(|| {
                    format!("SSH URL `{remote}` has an out-of-range port `{p}`")
                })?),
            ),
            _ => (authority, None),
        };
    if userhost.is_empty() {
        anyhow::bail!("SSH URL `{remote}` is missing a host");
    }

    let (user, host) = match userhost.split_once('@') {
        Some((u, h)) if !u.is_empty() && !h.is_empty() => (Some(u.to_string()), h.to_string()),
        Some(_) => anyhow::bail!("SSH URL `{remote}` has an empty user or host"),
        None => (None, userhost.to_string()),
    };

    Ok(SshTarget { user, host, port })
}

/// Build the `ssh` invocation for the interactive launch from a full
/// SSH URL and an already-built remote command. Returns `("ssh", argv)`
/// where argv is `[-t]? [-p <port>]? <[user@]host> <remote command>`.
/// Pass `tty = true` so the remote harness gets a real terminal.
fn ssh_invocation_tty(remote: &str, remote_cmd: &str, tty: bool) -> Result<(String, Vec<String>)> {
    let target = parse_ssh_url(remote)?;
    let mut argv = Vec::new();
    if tty {
        argv.push("-t".to_string());
    }
    if let Some(port) = target.port {
        argv.push("-p".to_string());
        argv.push(port.to_string());
    }
    argv.push(match &target.user {
        Some(user) => format!("{user}@{}", target.host),
        None => target.host.clone(),
    });
    argv.push(remote_cmd.to_string());
    Ok(("ssh".to_string(), argv))
}

/// Single-quote a string for safe interpolation into the remote shell
/// command, escaping any embedded single quotes.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
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

    #[test]
    fn ssh_invocation_parses_user_host_port_and_path() {
        let (binary, argv) = ssh_invocation_tty(
            "ssh://dev@example.com:2222/home/dev/project",
            "path resume 'owner/repo/slug'",
            false,
        )
        .unwrap();
        assert_eq!(binary, "ssh");
        assert_eq!(
            argv,
            vec![
                "-p".to_string(),
                "2222".to_string(),
                "dev@example.com".to_string(),
                "path resume 'owner/repo/slug'".to_string(),
            ]
        );
    }

    #[test]
    fn ssh_invocation_without_port_omits_p_flag() {
        let (_binary, argv) =
            ssh_invocation_tty("ssh://example.com", "path resume 'abc'", false).unwrap();
        assert_eq!(
            argv,
            vec!["example.com".to_string(), "path resume 'abc'".to_string()]
        );
    }

    #[test]
    fn ssh_invocation_rejects_non_ssh_url() {
        let err = parse_ssh_url("https://example.com/x").unwrap_err();
        assert!(err.to_string().contains("full SSH URL"), "actual: {err}");
    }

    #[test]
    fn ssh_invocation_tty_prepends_dash_t() {
        let (_binary, argv) = ssh_invocation_tty(
            "ssh://dev@example.com:2222",
            "path resume '/tmp/x.json'",
            true,
        )
        .unwrap();
        assert_eq!(
            argv,
            vec![
                "-t".to_string(),
                "-p".to_string(),
                "2222".to_string(),
                "dev@example.com".to_string(),
                "path resume '/tmp/x.json'".to_string(),
            ]
        );
    }

    #[test]
    fn parse_ssh_url_extracts_user_host_port_and_ignores_path() {
        assert_eq!(
            parse_ssh_url("ssh://dev@example.com:2222/home/dev/project").unwrap(),
            SshTarget {
                user: Some("dev".to_string()),
                host: "example.com".to_string(),
                port: Some(2222),
            }
        );
        assert_eq!(
            parse_ssh_url("ssh://example.com").unwrap(),
            SshTarget {
                user: None,
                host: "example.com".to_string(),
                port: None,
            }
        );
    }

    #[test]
    fn parse_ssh_url_rejects_out_of_range_port() {
        let err = parse_ssh_url("ssh://example.com:99999").unwrap_err();
        assert!(err.to_string().contains("out-of-range"), "actual: {err}");
    }

    #[test]
    fn claude_project_dir_name_matches_projector_sanitization() {
        // The host-side mirror of Claude Code's project-dir sanitization
        // must agree with toolpath-claude's (private) implementation, or
        // the shipped file lands where `claude -r` won't look. Compare
        // against the resolver's own dir name.
        let resolver = toolpath_claude::PathResolver::new();
        let expected = resolver
            .project_dir("/srv/my_app/v1.2")
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_eq!(claude_project_dir_name("/srv/my_app/v1.2"), expected);
    }

    #[test]
    fn remote_launch_command_quotes_id_and_cds() {
        assert_eq!(remote_launch_command("sess-1", None), "claude -r 'sess-1'");
        // Directory creation happens over SFTP before launch — the shell
        // string stays minimal: just the cd and the harness.
        assert_eq!(
            remote_launch_command("sess-1", Some(std::path::Path::new("/srv/work"))),
            "cd '/srv/work' && claude -r 'sess-1'"
        );
    }

    /// Write a minimal agent-bearing Path to a temp file and return
    /// `--remote` args pointing at it (harness pinned to Claude).
    fn remote_args_with_doc(dir: &std::path::Path) -> ResumeArgs {
        let mut path = make_convo_path_for_resume("claude-code://remote-v1-test");
        path.steps[0].step.actor = "agent:claude-code".to_string();
        let graph = toolpath::v1::Graph::from_path(path);
        let doc = dir.join("doc.json");
        std::fs::write(&doc, graph.to_json().unwrap()).unwrap();
        ResumeArgs {
            input: doc.to_string_lossy().to_string(),
            cwd: None,
            harness: Some(HarnessArg::Claude),
            no_cache: false,
            force: false,
            url: None,
            remote: Some("ssh://dev@example.com:2222".to_string()),
        }
    }

    #[test]
    fn remote_resume_probes_ships_then_launches() {
        // v3 over SFTP: host resolves AND projects the session locally,
        // preflights by resolving the remote home (typed transport call,
        // no shell), writes the projected JSONL into the remote's Claude
        // layout, then launches an interactive `ssh -t … claude -r <id>`.
        let td = tempfile::tempdir().unwrap();
        let rec = RecordingExec::default();
        run_with_strategy(remote_args_with_doc(td.path()), &rec).unwrap();

        // 1. preflight: one home lookup against the parsed target.
        let homes = rec.homes();
        assert_eq!(homes.len(), 1, "exactly one preflight");
        assert_eq!(
            homes[0],
            SshTarget {
                user: Some("dev".to_string()),
                host: "example.com".to_string(),
                port: Some(2222),
            }
        );

        // 2. ship: mkdir + write into the remote Claude layout, keyed on
        //    the remote home (no --cwd pinned). The written bytes are
        //    Claude JSONL, not the raw toolpath doc.
        let projects_dir = format!(
            "{RECORDING_REMOTE_HOME}/.claude/projects/{}",
            claude_project_dir_name(RECORDING_REMOTE_HOME)
        );
        assert_eq!(rec.mkdirs(), vec![projects_dir.clone()]);
        let writes = rec.writes();
        assert_eq!(writes.len(), 1, "exactly one file written");
        let (dest, body) = &writes[0];
        assert_eq!(dest, &format!("{projects_dir}/remote-v1-test.jsonl"));
        assert!(
            body.contains("\"sessionId\":\"remote-v1-test\""),
            "written bytes should be projected JSONL: {body}"
        );

        // 3. launch: interactive `ssh -t … claude -r '<id>'` with the id
        // derived from the doc's `claude-code://remote-v1-test` key.
        let cap = rec.captured();
        assert_eq!(cap.binary, "ssh");
        assert!(
            cap.args.iter().any(|a| a == "-t"),
            "launch needs -t: {:?}",
            cap.args
        );
        assert!(
            cap.args.iter().any(|a| a == "claude -r 'remote-v1-test'"),
            "launch cmd: {:?}",
            cap.args
        );
    }

    #[test]
    fn remote_resume_with_cwd_creates_launch_dir_over_sftp() {
        // A pinned --cwd is created via a typed mkdir call — not a shell
        // `mkdir -p` baked into the launch string.
        let td = tempfile::tempdir().unwrap();
        let mut args = remote_args_with_doc(td.path());
        args.cwd = Some(std::path::PathBuf::from("/srv/fresh"));
        let rec = RecordingExec::default();
        run_with_strategy(args, &rec).unwrap();

        assert!(
            rec.mkdirs().contains(&"/srv/fresh".to_string()),
            "launch dir should be created over SFTP: {:?}",
            rec.mkdirs()
        );
        let cap = rec.captured();
        assert!(
            cap.args
                .iter()
                .any(|a| a == "cd '/srv/fresh' && claude -r 'remote-v1-test'"),
            "launch should cd only (no shell mkdir): {:?}",
            cap.args
        );
    }

    #[test]
    fn remote_resume_rejects_non_claude_harness() {
        let td = tempfile::tempdir().unwrap();
        let mut args = remote_args_with_doc(td.path());
        args.harness = Some(HarnessArg::Codex);
        let rec = RecordingExec::default();
        let err = run_with_strategy(args, &rec).unwrap_err();
        assert!(
            err.to_string().contains("claude"),
            "error should name the supported harness: {err}"
        );
        assert!(rec.homes().is_empty(), "must fail before any remote touch");
    }

    #[test]
    fn remote_resume_aborts_when_preflight_fails() {
        // If the remote preflight fails (unreachable, bad auth), abort
        // before writing anything or dispatching.
        let td = tempfile::tempdir().unwrap();
        let rec = RecordingExec::failing_remote();
        let err = run_with_strategy(remote_args_with_doc(td.path()), &rec).unwrap_err();
        assert!(
            err.to_string().contains("remote"),
            "error should explain the preflight failure: {err}"
        );
        assert!(
            rec.writes().is_empty() && rec.mkdirs().is_empty(),
            "must not touch the remote after a failed preflight"
        );
        assert!(rec.captured().binary.is_empty(), "must not dispatch");
    }

    #[test]
    fn remote_resume_without_harness_errors() {
        let td = tempfile::tempdir().unwrap();
        let mut args = remote_args_with_doc(td.path());
        args.harness = None;
        let rec = RecordingExec::default();
        let err = run_with_strategy(args, &rec).unwrap_err();
        assert!(err.to_string().contains("--harness"), "actual: {err}");
        assert!(rec.homes().is_empty(), "must fail before any remote touch");
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
            harness: Some(HarnessArg::Claude),
            no_cache: false,
            force: false,
            url: None,
            remote: None,
        };

        let recorder = RecordingExec::default();
        run_with_strategy(args, &recorder).unwrap();

        let cap = recorder.captured();
        assert_eq!(cap.binary, "claude");
        assert_eq!(cap.args[0], "-r");
        assert_eq!(cap.cwd, std::fs::canonicalize(cwd.path()).unwrap());
    }

    use crate::cmd_share::Harness;
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
            no_cache: false,
            force: false,
            url: None,
            remote: None,
        };
        let (g, harness) = resolve_input(&args).unwrap();
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
            force: false,
            url: None,
            remote: None,
        };
        let (g, harness) = resolve_input(&args).unwrap();
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
            no_cache: false,
            force: false,
            url: None,
            remote: None,
        };
        let result = resolve_input(&args);

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
            no_cache: false,
            force: false,
            url: None,
            remote: None,
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
        let result = pick_harness(Some(HarnessArg::Claude), None, Some(td.path()));
        assert_eq!(result.unwrap(), Harness::Claude);

        let err = pick_harness(Some(HarnessArg::Gemini), None, Some(td.path())).unwrap_err();
        assert!(err.to_string().contains("`gemini` isn't on PATH"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cursor_available_via_open_fallback_on_macos() {
        let td = fake_path_with(&["open"]);
        assert!(harness_available(Harness::Cursor, Some(td.path())));
        let picked = pick_harness(Some(HarnessArg::Cursor), None, Some(td.path()));
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
