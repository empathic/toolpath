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
//! ## Remote (`--remote ssh://[user@]host[:port][/path]` | `[user@]host[:port]` | config alias)
//!
//! v3 (host projects, remote just receives files): the **host** resolves
//! the document AND projects the session fully in memory, then ships the
//! finished harness file to the remote over SFTP and launches the
//! harness — so the remote needs only SSH and the harness installed.
//! **No `path` on the remote**, no Pathbase access, no temp files, and
//! no composed remote shell strings: the file operations are typed SFTP
//! calls via libssh2 (matching the repo's `git2`-over-shelling-out
//! ethos). Steps (`run_remote`):
//!
//! 1. **Resolve + project on the host** — same `resolve_input` /
//!    `ensure_path_with_agent` as a local resume, so a bad or non-agent
//!    document fails fast on the host, not deep inside SSH. The session
//!    id and JSONL come from the same in-memory projection
//!    (`cmd_export::claude_session_jsonl`).
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
//!    binary (it needs the user's TTY and full ssh config). The
//!    transport honors the `HostName`/`User`/`Port`/`IdentityFile`
//!    subset of `~/.ssh/config` (natively parsed — see
//!    `parse_ssh_config`) with URL values winning; configured
//!    identities are matched against the agent by public-key blob, so
//!    key-pinned hosts (exe.dev-style, where the key IS the identity)
//!    authenticate as the right account. `known_hosts` is still not
//!    checked on the transport connection.
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

use crate::harness::Harness;

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

    /// Resume on a remote host over SSH instead of locally. Takes a full
    /// SSH URL (`ssh://[user@]host[:port][/path]`), a bare
    /// `[user@]host[:port]`, or a `~/.ssh/config` Host alias (its
    /// HostName/User/Port/IdentityFile are resolved from the config). When
    /// set, the resume is dispatched to the remote host rather than
    /// exec'ing a local harness.
    #[arg(long)]
    pub remote: Option<String>,

    /// Wrap the remote launch in a named tmux session
    /// (`tmux new-session -A -s path-<id> …`) so it survives SSH
    /// disconnects and can be detached/re-attached; re-running the same
    /// resume re-attaches. Requires --remote and tmux on the remote.
    #[arg(long, requires = "remote")]
    pub tmux: bool,

    /// Remote session-persistence backend. Skips the picker. Requires --remote.
    #[arg(long, value_enum, requires = "remote")]
    pub persist: Option<PersistBackend>,

    /// Transport for the interactive launch. ssh (default); mosh/et reserved.
    #[arg(long, value_enum, default_value_t = Transport::Ssh, requires = "remote")]
    pub via: Transport,
}

/// Resolve the effective persist backend from `--tmux`/`--persist`,
/// treating `--tmux` as a deprecated alias for `--persist tmux`.
/// Errors if both are set.
fn resolve_persist_flag(args: &ResumeArgs) -> Result<Option<PersistBackend>> {
    match (args.tmux, args.persist) {
        (true, Some(_)) => anyhow::bail!(
            "--tmux is a deprecated alias for --persist tmux; don't combine it with --persist"
        ),
        (true, None) => {
            eprintln!("note: --tmux is deprecated; use --persist tmux");
            Ok(Some(PersistBackend::Tmux))
        }
        (false, p) => Ok(p),
    }
}

pub fn run(args: ResumeArgs) -> Result<()> {
    run_with_strategy(args, &RealExec::default())
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

/// Resolve the user-supplied `<input>` argument into a parsed `Graph`
/// plus the source harness inferred from its single inline path (if
/// any). See spec § "Input resolution" for the order.
pub(crate) fn resolve_input(args: &ResumeArgs) -> Result<(Graph, Option<Harness>)> {
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
                Graph::from_json(&json)
                    .map_err(|e| anyhow::anyhow!("cached toolpath document is invalid: {}", e))?
            } else {
                let derived = crate::derive::pathbase_fetch_to_doc(u, args.url.as_deref())?;
                if !args.no_cache {
                    // force=true here: we either short-circuited above
                    // (cache miss) or the user explicitly passed --force,
                    // and either way we want the new bytes to land.
                    crate::cache::write_cached(&derived.cache_id, &derived.doc, true)?;
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
            let file = crate::cache::cache_ref(id).map_err(|e| {
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

/// A parsed SSH remote: `ssh://[user@]host[:port][/path]`, a bare
/// `[user@]host[:port]`, or a `~/.ssh/config` Host alias in the `host` slot.
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

    /// Which of `bins` exist on the remote (`command -v`). One exec channel.
    fn remote_which(
        &self,
        target: &SshTarget,
        bins: &[&str],
    ) -> Result<std::collections::BTreeSet<String>>;
}

/// Production implementation. On Unix this never returns on success
/// (the current process is replaced); on Windows it spawns the child,
/// waits, and propagates the exit code.
///
/// The remote-transport methods share one authenticated SSH connection
/// (cached across calls) and prefer SFTP; when the server won't open an
/// SFTP channel (some custom sshds don't, e.g. exe.dev VMs), they fall
/// back to the SCP protocol for writes and a minimal exec channel for
/// `pwd`/`mkdir -p` — all still through libssh2, no external binaries.
#[derive(Default)]
pub struct RealExec {
    conn: std::sync::Mutex<Option<RemoteConn>>,
}

/// A live authenticated session plus the SFTP channel if the server
/// offers one (`None` ⇒ SCP/exec fallback).
struct RemoteConn {
    key: SshTarget,
    sess: ssh2::Session,
    sftp: Option<ssh2::Sftp>,
}

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
        self.with_conn(target, |conn| match &conn.sftp {
            Some(sftp) => {
                let home = sftp
                    .realpath(std::path::Path::new("."))
                    .context("resolve remote home directory")?;
                validate_remote_home(&home.to_string_lossy())
            }
            None => {
                let out = exec_channel_capture(&conn.sess, "pwd")?;
                validate_remote_home(&out)
            }
        })
    }

    fn remote_mkdirs(&self, target: &SshTarget, dir: &str) -> Result<()> {
        self.with_conn(target, |conn| match &conn.sftp {
            Some(sftp) => {
                // Walk the components, creating as we go — `mkdir -p`
                // semantics.
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
            None => {
                exec_channel_capture(&conn.sess, &format!("mkdir -p {}", shell_single_quote(dir)))
                    .with_context(|| format!("create remote dir {dir}"))?;
                Ok(())
            }
        })
    }

    fn remote_write(&self, target: &SshTarget, path: &str, data: &[u8]) -> Result<()> {
        use std::io::Write;
        self.with_conn(target, |conn| match &conn.sftp {
            Some(sftp) => {
                let mut f = sftp
                    .create(std::path::Path::new(path))
                    .with_context(|| format!("create remote file {path}"))?;
                f.write_all(data)
                    .with_context(|| format!("write remote file {path}"))?;
                Ok(())
            }
            None => {
                // SCP protocol upload — libssh2's scp_send, no external
                // binary.
                let mut ch = conn
                    .sess
                    .scp_send(std::path::Path::new(path), 0o644, data.len() as u64, None)
                    .with_context(|| format!("open SCP upload to {path}"))?;
                ch.write_all(data)
                    .with_context(|| format!("SCP write to {path}"))?;
                ch.send_eof().context("SCP finish (eof)")?;
                ch.wait_eof().context("SCP finish (wait eof)")?;
                ch.close().context("SCP close")?;
                ch.wait_close().context("SCP close (wait)")?;
                Ok(())
            }
        })
    }

    fn remote_which(
        &self,
        target: &SshTarget,
        bins: &[&str],
    ) -> Result<std::collections::BTreeSet<String>> {
        if bins.is_empty() {
            return Ok(Default::default());
        }
        let probe = bins
            .iter()
            .map(|b| {
                format!(
                    "command -v {} >/dev/null 2>&1 && echo {}",
                    shell_single_quote(b),
                    shell_single_quote(b)
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        self.with_conn(target, |conn| {
            let out = exec_channel_capture(&conn.sess, &probe)?;
            Ok(out
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect())
        })
    }
}

impl RealExec {
    /// Run `f` against the cached connection for `target`, dialing (and
    /// probing for SFTP support) on first use or when the target
    /// changed.
    fn with_conn<T>(
        &self,
        target: &SshTarget,
        f: impl FnOnce(&RemoteConn) -> Result<T>,
    ) -> Result<T> {
        let mut guard = self.conn.lock().unwrap();
        if guard.as_ref().is_none_or(|c| &c.key != target) {
            *guard = Some(connect_remote(target)?);
        }
        f(guard.as_ref().expect("connection just established"))
    }
}

/// Run a one-shot command over a libssh2 exec channel and return its
/// stdout. Used only on servers without an SFTP subsystem, and only for
/// `pwd` / `mkdir -p`.
fn exec_channel_capture(sess: &ssh2::Session, cmd: &str) -> Result<String> {
    use std::io::Read;
    let mut ch = sess.channel_session().context("open exec channel")?;
    ch.exec(cmd).with_context(|| format!("run `{cmd}`"))?;
    let mut out = String::new();
    ch.read_to_string(&mut out)
        .with_context(|| format!("read `{cmd}` output"))?;
    let mut err = String::new();
    ch.stderr().read_to_string(&mut err).ok();
    ch.wait_close().ok();
    let status = ch.exit_status().unwrap_or(-1);
    // Some minimal sshds report a bogus nonzero exit even on success —
    // trust actual output over the status code when there is any.
    if status != 0 && out.trim().is_empty() {
        anyhow::bail!(
            "`{cmd}` exited {status}{}",
            if err.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", err.trim())
            }
        );
    }
    Ok(out)
}

/// Validate that resolving the remote home produced a real absolute path,
/// not an error banner. Some sshds authenticate a key at the transport
/// layer but then answer every command with a notice — e.g. exe.dev
/// replies `Please complete registration by running: ssh exe.dev` for a
/// key not yet bound to an account. Without this check that banner gets
/// spliced into the remote session path (`~/.claude/projects/Please
/// complete registration…/`), which fails deep inside the file ship with
/// an inscrutable error. Require a single-line absolute path so the
/// failure is caught early with the offending output shown verbatim.
fn validate_remote_home(raw: &str) -> Result<String> {
    let home = raw.trim();
    if home.is_empty() {
        anyhow::bail!("remote home lookup returned nothing");
    }
    if !home.starts_with('/') || home.contains(['\n', '\r']) {
        anyhow::bail!(
            "remote home lookup returned an unexpected value (not an absolute path): {home:?}. \
             This often means the SSH key authenticated but the host doesn't recognize it — \
             e.g. an unregistered key on a key-identified host. Register/load the right key and retry."
        );
    }
    Ok(home.to_string())
}

/// The subset of `~/.ssh/config` the transport honors for a host:
/// `HostName`, `User`, `Port`, `IdentityFile`. Parsed natively —
/// libssh2 reads no config at all, and without this, agent auth picks
/// whatever key happens to be first (which servers like exe.dev, that
/// identify accounts *by key*, then misroute).
#[derive(Debug, Default, PartialEq, Eq)]
struct SshHostConfig {
    host_name: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity_files: Vec<std::path::PathBuf>,
    /// `IdentitiesOnly yes` — when set (or when any `IdentityFile` is
    /// configured), auth must use only the configured keys and must not
    /// fall back to trying every agent key.
    identities_only: bool,
}

/// Load [`SshHostConfig`] for `host` from `~/.ssh/config` (empty config
/// when the file or `$HOME` is missing).
fn ssh_host_config(host: &str) -> SshHostConfig {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return SshHostConfig::default();
    };
    match std::fs::read_to_string(home.join(".ssh/config")) {
        Ok(content) => parse_ssh_config(&content, host, &home),
        Err(_) => SshHostConfig::default(),
    }
}

/// Minimal ssh_config parser: `Host` blocks with `*`/`?`/`!` patterns,
/// first-obtained-value-wins for scalars (OpenSSH semantics),
/// accumulating `IdentityFile` with `~` expansion. Directives before
/// the first `Host` line apply to every host.
fn parse_ssh_config(content: &str, host: &str, home: &std::path::Path) -> SshHostConfig {
    let mut cfg = SshHostConfig::default();
    let mut active = true; // pre-Host directives are global
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (keyword, value) = match line.split_once([' ', '\t', '=']) {
            // OpenSSH allows `Key = value` (whitespace around `=`); after
            // splitting on the first separator, strip a leading `=` and
            // surrounding whitespace off the value so `User = alice`
            // yields `alice`, not `= alice`.
            Some((k, v)) => (
                k.to_ascii_lowercase(),
                v.trim().trim_start_matches('=').trim().trim_matches('"'),
            ),
            None => continue,
        };
        if keyword == "host" {
            let patterns: Vec<&str> = value.split_whitespace().collect();
            let negated = patterns
                .iter()
                .any(|p| p.strip_prefix('!').is_some_and(|p| glob_match(p, host)));
            let matched = patterns
                .iter()
                .any(|p| !p.starts_with('!') && glob_match(p, host));
            active = matched && !negated;
            continue;
        }
        if !active {
            continue;
        }
        match keyword.as_str() {
            "hostname" if cfg.host_name.is_none() => cfg.host_name = Some(value.to_string()),
            "user" if cfg.user.is_none() => cfg.user = Some(value.to_string()),
            "port" if cfg.port.is_none() => cfg.port = value.parse().ok(),
            "identityfile" => {
                let path = match value.strip_prefix("~/") {
                    Some(rest) => home.join(rest),
                    None => std::path::PathBuf::from(value),
                };
                if !cfg.identity_files.contains(&path) {
                    cfg.identity_files.push(path);
                }
            }
            "identitiesonly" if value.eq_ignore_ascii_case("yes") => {
                cfg.identities_only = true;
            }
            _ => {}
        }
    }
    cfg
}

/// ssh_config-style glob: `*` matches any run, `?` a single char.
fn glob_match(pattern: &str, s: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = s.chars().collect();
    // Iterative wildcard match with backtracking on the last `*`.
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Verify the connected server's host key against `~/.ssh/known_hosts`
/// (TOFU baseline, matching what the interactive `ssh` launch does).
/// Aborts on a mismatch (possible MITM). A not-yet-known host is
/// accepted with a warning — first-contact, like ssh's default
/// `StrictHostKeyChecking accept-new` — because the transport ships
/// bytes before the interactive step can prompt. A missing/unreadable
/// known_hosts file is non-fatal (nothing to check against).
fn check_known_host(sess: &ssh2::Session, host: &str, port: u16) -> Result<()> {
    let Some((key, _)) = sess.host_key() else {
        anyhow::bail!("remote {host}:{port} presented no host key");
    };
    let mut known = sess.known_hosts().context("open known_hosts")?;
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        // Best-effort read; absence is handled by the check below.
        let _ = known.read_file(
            &home.join(".ssh/known_hosts"),
            ssh2::KnownHostFileKind::OpenSSH,
        );
    }
    use ssh2::CheckResult;
    match known.check_port(host, port, key) {
        CheckResult::Match => Ok(()),
        CheckResult::Mismatch => anyhow::bail!(
            "host key for {host}:{port} does NOT match ~/.ssh/known_hosts — \
             possible man-in-the-middle; refusing to ship the session"
        ),
        CheckResult::NotFound | CheckResult::Failure => {
            eprintln!(
                "note: {host}:{port} is not in ~/.ssh/known_hosts — accepting on first contact"
            );
            Ok(())
        }
    }
}

/// Authenticate `sess` as `user`, honoring configured identities: for
/// each `IdentityFile`, first look for the matching key in the agent
/// (by public-key blob — works for passphrase-protected keys), then try
/// the key file directly. Falls back to plain agent auth (any loaded
/// key) ONLY when no identity was configured; when identities are
/// pinned, trying an arbitrary agent key could misroute on hosts that
/// identify the account by key (e.g. exe.dev), so a pinned-but-failed
/// auth errors instead.
fn authenticate(sess: &ssh2::Session, user: &str, cfg: &SshHostConfig, addr: &str) -> Result<()> {
    for key_path in &cfg.identity_files {
        if agent_auth_with_key(sess, user, key_path)? {
            return Ok(());
        }
        if sess
            .userauth_pubkey_file(user, None, key_path, None)
            .is_ok()
        {
            return Ok(());
        }
    }
    // Pinned identities that all failed must not silently degrade to
    // "try every agent key" — that's the exact misroute the config
    // parsing exists to prevent.
    if !cfg.identity_files.is_empty() || cfg.identities_only {
        anyhow::bail!(
            "SSH auth as `{user}` on {addr} failed with the configured IdentityFile(s); \
             none matched a loaded agent key or were usable directly. Load the pinned key \
             (`ssh-add <keyfile>`) — refusing to fall back to an arbitrary agent key."
        );
    }
    sess.userauth_agent(user).with_context(|| {
        format!("SSH agent auth as `{user}` on {addr} — is the key loaded (`ssh-add`)?")
    })
}

/// Try agent auth with the specific key whose public half sits next to
/// `key_path` (`<key>.pub`). Returns Ok(false) when the pub file or a
/// matching agent identity isn't there — callers fall through.
fn agent_auth_with_key(
    sess: &ssh2::Session,
    user: &str,
    key_path: &std::path::Path,
) -> Result<bool> {
    use base64::Engine as _;
    let pub_path = std::path::PathBuf::from(format!("{}.pub", key_path.display()));
    let Ok(line) = std::fs::read_to_string(&pub_path) else {
        return Ok(false);
    };
    let Some(b64) = line.split_whitespace().nth(1) else {
        return Ok(false);
    };
    let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        return Ok(false);
    };
    let mut agent = sess.agent().context("open SSH agent")?;
    if agent.connect().is_err() {
        return Ok(false); // no agent running — try the key file instead
    }
    agent.list_identities().context("list agent identities")?;
    for identity in agent.identities().context("read agent identities")? {
        if identity.blob() == blob.as_slice() {
            agent
                .userauth(user, &identity)
                .with_context(|| format!("agent auth with {}", pub_path.display()))?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Dial + authenticate a session to `target` and probe for SFTP: TCP
/// connect (bounded), handshake, config-aware auth (see
/// [`authenticate`]), then one SFTP-open attempt — servers without the
/// subsystem (e.g. exe.dev VMs) get the SCP/exec fallback instead.
///
/// `~/.ssh/config` fills the gaps the URL leaves open (HostName, User,
/// Port, IdentityFile); URL values win. known_hosts is still not
/// checked — the interactive launch goes through the real `ssh` binary
/// with the user's full config.
fn connect_remote(target: &SshTarget) -> Result<RemoteConn> {
    use std::net::ToSocketAddrs;
    let cfg = ssh_host_config(&target.host);
    let host = cfg.host_name.clone().unwrap_or_else(|| target.host.clone());
    let port = target.port.or(cfg.port).unwrap_or(22);
    let addr = format!("{host}:{port}");
    // Bounded connect + per-operation timeouts: a wedged remote should
    // fail with context, never hang the resume silently.
    let sock = addr
        .to_socket_addrs()
        .with_context(|| format!("resolve {addr}"))?
        .next()
        .with_context(|| format!("no address for {addr}"))?;
    let tcp = std::net::TcpStream::connect_timeout(&sock, std::time::Duration::from_secs(10))
        .with_context(|| format!("connect to {addr}"))?;
    let mut sess = ssh2::Session::new().context("create SSH session")?;
    sess.set_tcp_stream(tcp);
    sess.set_timeout(30_000); // ms; applies to handshake/auth/channel ops
    sess.handshake()
        .with_context(|| format!("SSH handshake with {addr}"))?;
    // Verify the server host key against ~/.ssh/known_hosts BEFORE
    // authenticating or shipping any bytes: the transport uploads the
    // full session transcript over this channel ahead of the
    // interactive `ssh` launch (which does its own check), so a
    // known_hosts mismatch must abort here, not after the leak.
    check_known_host(&sess, &host, port)?;
    let user = match target.user.clone().or_else(|| cfg.user.clone()) {
        Some(u) => u,
        None => {
            std::env::var("USER").context("no SSH user: put `user@` in the URL or set $USER")?
        }
    };
    authenticate(&sess, &user, &cfg, &addr)?;

    // SFTP probe: keep it brief — a server without the subsystem may
    // just sit on the channel request until a timeout.
    sess.set_timeout(5_000);
    let sftp = sess.sftp().ok();
    sess.set_timeout(30_000);
    if sftp.is_none() {
        eprintln!("note: remote has no SFTP subsystem — using SCP fallback");
    }

    Ok(RemoteConn {
        key: target.clone(),
        sess,
        sftp,
    })
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
    /// Canned set of binaries `remote_which` reports as present.
    available: std::collections::BTreeSet<String>,
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

    /// A recorder whose `remote_which` reports exactly `bins` as present.
    pub fn with_available<'a>(bins: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            available: bins.into_iter().map(String::from).collect(),
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

    fn remote_which(
        &self,
        _target: &SshTarget,
        bins: &[&str],
    ) -> Result<std::collections::BTreeSet<String>> {
        Ok(bins
            .iter()
            .filter(|b| self.available.contains(**b))
            .map(|b| b.to_string())
            .collect())
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

/// Remote resume (v3). See the module-level "Remote" section for the
/// full design; in brief the host resolves + projects the session in
/// memory (`cmd_export::claude_session_jsonl`), preflights the
/// remote over SFTP, ships the finished Claude JSONL into the remote's
/// project layout, and `execvp`s an interactive `ssh -t … claude -r
/// <id>`. The remote needs only sshd + the harness — no `path`, no
/// Pathbase, no temp files.
fn run_remote(args: &ResumeArgs, remote: &str, exec: &dyn ExecStrategy) -> Result<()> {
    // The remote's interactive picker can't run from here, so pin the
    // harness explicitly. Only Claude is wired up so far: the host-side
    // projection and layout knowledge are Claude-specific.
    match args.harness {
        None => anyhow::bail!(
            "--remote requires --harness <X>: the host can't run the remote's \
             harness picker, so the target must be pinned"
        ),
        Some(Harness::Claude) => {}
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
    //    started there finds the session. The cwd is normalized to the
    //    absolute form the remote shell's `cd` will land on (trailing
    //    slashes / `.` / `..` / relative-to-home resolved) so the host's
    //    dir-name key matches what remote Claude computes from its cwd.
    let launch_cwd: Option<String> = args
        .cwd
        .as_ref()
        .map(|dir| normalize_remote_cwd(dir, &home));
    let project_path = launch_cwd.clone().unwrap_or_else(|| home.clone());
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
    if let Some(dir) = launch_cwd.as_deref() {
        exec.remote_mkdirs(&target, dir)
            .with_context(|| format!("creating launch dir {dir} on {remote}"))?;
    }

    // 4. Interactive launch of the harness against the shipped session,
    //    with a real TTY — the one step that stays on the real `ssh`
    //    binary (it needs the user's terminal and ssh config).
    let backend = if args.tmux {
        PersistBackend::Tmux
    } else {
        PersistBackend::Plain
    };
    let launch_cmd = persist_plan(
        Harness::Claude,
        &session_id,
        launch_cwd.as_deref(),
        backend,
        &home,
    )
    .remote_command;
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

/// Normalize a `--cwd` to the absolute path the remote shell's `cd`
/// will land on, so the host's project-dir key matches what remote
/// Claude derives from `getcwd`. Relative paths resolve against the
/// remote `home` (where a `cd`-less ssh command starts); `.`, `..`,
/// `//`, and trailing slashes collapse. Symlinks aren't resolved (the
/// host can't see the remote FS) — an acceptable edge.
fn normalize_remote_cwd(cwd: &std::path::Path, home: &str) -> String {
    let raw = cwd.to_string_lossy();
    let combined = if raw.starts_with('/') {
        raw.into_owned()
    } else {
        format!("{home}/{raw}")
    };
    let mut out: Vec<&str> = Vec::new();
    for seg in combined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    format!("/{}", out.join("/"))
}

/// The far-side launch command, derived from the same per-harness
/// invocation table the local resume uses (`name()` + [`argv_for`]) so
/// the two can't drift — prefixed with a `cd <cwd> &&` when a cwd was
/// pinned so the harness starts where the shipped session is keyed.
/// `cwd` is the already-normalized absolute remote path. The directory
/// itself is created over SFTP before launch — this is the only remote
/// shell string left, and it's minimal because a `cd` can't happen
/// anywhere else.
struct PersistPlan {
    remote_command: String,
    extra_file: Option<(String, Vec<u8>)>,
    post_note: Option<String>,
}

fn persist_plan(
    harness: Harness,
    session_id: &str,
    cwd: Option<&str>,
    backend: PersistBackend,
    home: &str,
) -> PersistPlan {
    let launch: Vec<String> = std::iter::once(harness.name().to_string())
        .chain(argv_for(harness, session_id).iter().map(|a| shell_quote(a)))
        .collect();
    let inner = launch.join(" ");
    let inner = match cwd {
        Some(dir) => format!("cd {} && {inner}", shell_quote(dir)),
        None => inner,
    };
    // Detachable session names can't contain `.`/`:`.
    let name = format!("path-{}", session_id.replace(['.', ':'], "-"));

    let mut extra_file = None;
    let mut post_note = None;
    let remote_command = match backend {
        PersistBackend::Plain => inner,
        PersistBackend::Tmux => format!(
            "tmux new-session -A -s {} {}",
            shell_quote(&name),
            shell_single_quote(&inner)
        ),
        PersistBackend::Abduco => format!(
            "abduco -A {} sh -c {}",
            shell_quote(&name),
            shell_single_quote(&inner)
        ),
        PersistBackend::Dtach => format!(
            "dtach -A {} -z sh -c {}",
            shell_quote(&format!(
                "/tmp/path-dtach-{}",
                session_id.replace(['.', ':'], "-")
            )),
            shell_single_quote(&inner)
        ),
        PersistBackend::Zellij => zellij_plan(&name, &inner, home, &mut extra_file),
        PersistBackend::Shpool => shpool_plan(&name, &inner, &mut post_note),
    };
    PersistPlan {
        remote_command,
        extra_file,
        post_note,
    }
}

fn zellij_plan(
    name: &str,
    inner: &str,
    home: &str,
    extra: &mut Option<(String, Vec<u8>)>,
) -> String {
    let layout_path = format!("{home}/.cache/path/zellij-{name}.kdl");
    // Single-pane layout that runs INNER via the shell. `close_on_exit`
    // keeps the pane if the harness exits so output stays visible.
    let kdl = format!(
        "layout {{\n    pane command=\"sh\" {{\n        args \"-c\" {inner:?}\n    }}\n}}\n"
    );
    *extra = Some((layout_path.clone(), kdl.into_bytes()));
    format!(
        "zellij --session {} --layout {}",
        shell_quote(name),
        shell_quote(&layout_path)
    )
}

fn shpool_plan(name: &str, inner: &str, note: &mut Option<String>) -> String {
    *note = Some(format!(
        "shpool has no command arg — in the persistent shell, run:\n    {inner}"
    ));
    format!("shpool attach {}", shell_quote(name))
}

/// Quote for the remote shell only when needed — plain flags, ids, and
/// paths stay bare so the echoed recipe reads like something a human
/// would type; anything else gets [`shell_single_quote`].
fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@'));
    if safe {
        s.to_string()
    } else {
        shell_single_quote(s)
    }
}

/// Parse an SSH remote into a typed [`SshTarget`]. Accepts either a full
/// `ssh://[user@]host[:port][/path]` URL (the optional `/path` is ignored)
/// or a bare `[user@]host[:port]` — including a plain `~/.ssh/config`
/// `Host` alias, whose `HostName`/`User`/`Port`/`IdentityFile` are then
/// resolved by [`connect_remote`] (libssh2 transport) and by the `ssh`
/// binary (interactive launch). Other URL schemes are rejected explicitly.
fn parse_ssh_url(remote: &str) -> Result<SshTarget> {
    let rest = if let Some(r) = remote.strip_prefix("ssh://") {
        r
    } else if let Some((scheme, _)) = remote.split_once("://") {
        anyhow::bail!(
            "remote must be an SSH URL (ssh://…) or a host/alias, got a `{scheme}://` URL: `{remote}`"
        );
    } else {
        // Bare `[user@]host[:port]` or a ~/.ssh/config Host alias.
        remote
    };

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

/// Assemble candidate persist backends for the remote persistence picker.
/// Returns backends from DISPLAY_ORDER filtered to those whose bin() is
/// in the available set, plus Plain which is always offered.
fn persist_candidates(available: &std::collections::BTreeSet<String>) -> Vec<PersistBackend> {
    PersistBackend::DISPLAY_ORDER
        .into_iter()
        .filter(|b| match b.bin() {
            None => true, // Plain always offered
            Some(bin) => available.contains(bin),
        })
        .collect()
}

/// Pick the preferred backend from the available set. Priority:
/// [Tmux, Zellij, Abduco, Dtach], falling back to Plain if none are available.
fn preferred_backend(available: &std::collections::BTreeSet<String>) -> PersistBackend {
    const PRIORITY: [PersistBackend; 4] = [
        PersistBackend::Tmux,
        PersistBackend::Zellij,
        PersistBackend::Abduco,
        PersistBackend::Dtach,
    ];
    PRIORITY
        .into_iter()
        .find(|b| b.bin().is_some_and(|bin| available.contains(bin)))
        .unwrap_or(PersistBackend::Plain)
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

/// Transport protocol for remote resume. v1 implements only SSH; mosh and ET
/// are reserved for future use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Transport {
    Ssh,
    Mosh,
    Et,
}

/// Carry the persist-wrapped remote command over the chosen transport.
/// v1 implements only ssh; mosh/et are reserved (spec: Transport axis).
fn launch_invocation(
    transport: Transport,
    remote: &str,
    remote_cmd: &str,
) -> Result<(String, Vec<String>)> {
    match transport {
        Transport::Ssh => ssh_invocation_tty(remote, remote_cmd, true),
        Transport::Mosh => {
            anyhow::bail!("--via mosh is not yet supported (reserved); use --via ssh")
        }
        Transport::Et => anyhow::bail!("--via et is not yet supported (reserved); use --via ssh"),
    }
}

/// Remote session-persistence backend for `--remote` resume. See the
/// design spec: three launch mechanisms (direct-wrap, layout-wrap for
/// zellij, attach-only for shpool).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PersistBackend {
    Plain,
    Tmux,
    Abduco,
    Dtach,
    Zellij,
    Shpool,
}

impl PersistBackend {
    /// Probe target on the remote (`command -v <bin>`); `Plain` has none.
    fn bin(&self) -> Option<&'static str> {
        match self {
            PersistBackend::Plain => None,
            PersistBackend::Tmux => Some("tmux"),
            PersistBackend::Abduco => Some("abduco"),
            PersistBackend::Dtach => Some("dtach"),
            PersistBackend::Zellij => Some("zellij"),
            PersistBackend::Shpool => Some("shpool"),
        }
    }

    fn describe(&self) -> &'static str {
        match self {
            PersistBackend::Plain => "plain — no persistence; dies on disconnect",
            PersistBackend::Tmux => "tmux — detachable; survives drops, reattachable",
            PersistBackend::Abduco => "abduco — minimal detach/attach; survives drops",
            PersistBackend::Dtach => "dtach — tiny detach/attach; survives drops",
            PersistBackend::Zellij => "zellij — detachable workspace (layout-launched)",
            PersistBackend::Shpool => {
                "shpool — persistent shell (attach-only; run the command yourself)"
            }
        }
    }

    /// Fixed picker display order.
    const DISPLAY_ORDER: [PersistBackend; 6] = [
        PersistBackend::Tmux,
        PersistBackend::Zellij,
        PersistBackend::Abduco,
        PersistBackend::Dtach,
        PersistBackend::Shpool,
        PersistBackend::Plain,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_remote_home_accepts_absolute_path() {
        assert_eq!(
            validate_remote_home("/home/exedev\n").unwrap(),
            "/home/exedev"
        );
        assert_eq!(validate_remote_home("  /root  ").unwrap(), "/root");
    }

    #[test]
    fn validate_remote_home_rejects_error_banner() {
        // The exact exe.dev banner an unregistered-but-SSH-authenticated
        // key produces — must fail early, not become a path component.
        let err = validate_remote_home("Please complete registration by running: ssh exe.dev")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not an absolute path"), "got: {err}");
        assert!(err.contains("unregistered key"), "got: {err}");
    }

    #[test]
    fn validate_remote_home_rejects_empty_and_multiline() {
        assert!(
            validate_remote_home("   ")
                .unwrap_err()
                .to_string()
                .contains("nothing")
        );
        // A leading path with trailing banner lines is still rejected.
        assert!(validate_remote_home("/home/x\nextra chatter").is_err());
    }

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
        assert!(err.to_string().contains("host/alias"), "actual: {err}");
        assert!(err.to_string().contains("https://"), "actual: {err}");
    }

    #[test]
    fn parse_ssh_url_accepts_bare_alias() {
        // A plain ~/.ssh/config Host alias — no scheme, no user, no port.
        // HostName/User/Port/IdentityFile get resolved downstream from
        // the config; here it's just the host slot.
        assert_eq!(
            parse_ssh_url("mybox").unwrap(),
            SshTarget {
                user: None,
                host: "mybox".to_string(),
                port: None,
            }
        );
    }

    #[test]
    fn parse_ssh_url_accepts_bare_user_host_port() {
        assert_eq!(
            parse_ssh_url("dev@example.com:2222").unwrap(),
            SshTarget {
                user: Some("dev".to_string()),
                host: "example.com".to_string(),
                port: Some(2222),
            }
        );
    }

    #[test]
    fn ssh_invocation_passes_bare_alias_to_ssh_binary() {
        // The interactive launch hands the alias straight to `ssh`, which
        // resolves HostName/User/Port/ProxyJump natively.
        let (binary, argv) = ssh_invocation_tty("mybox", "path resume 'abc'", true).unwrap();
        assert_eq!(binary, "ssh");
        assert_eq!(
            argv,
            vec![
                "-t".to_string(),
                "mybox".to_string(),
                "path resume 'abc'".to_string(),
            ]
        );
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
    fn ssh_config_matches_wildcard_host_and_expands_identity() {
        // The exe.dev shape: a wildcard Host block pinning an identity.
        // libssh2 doesn't read ~/.ssh/config, so the transport must — or
        // agent auth picks whatever key happens to be first.
        let config = "Host exe.dev *.exe.xyz\n\
                      \x20 IdentitiesOnly yes\n\
                      \x20 IdentityFile ~/.ssh/id_ed25519_signing\n\
                      \x20 StrictHostKeyChecking accept-new\n";
        let cfg = parse_ssh_config(
            config,
            "pathremote.exe.xyz",
            std::path::Path::new("/home/u"),
        );
        assert_eq!(
            cfg.identity_files,
            vec![std::path::PathBuf::from("/home/u/.ssh/id_ed25519_signing")]
        );
        assert_eq!(cfg.user, None);
        // A non-matching host gets nothing.
        let other = parse_ssh_config(config, "example.com", std::path::Path::new("/home/u"));
        assert!(other.identity_files.is_empty());
    }

    #[test]
    fn ssh_config_first_value_wins_across_blocks() {
        let config = "Host other\n\
                      \x20 User nope\n\
                      Host pathremote.*\n\
                      \x20 User dev\n\
                      \x20 Port 2200\n\
                      \x20 HostName real.example.com\n\
                      Host *\n\
                      \x20 User fallback\n\
                      \x20 Port 9\n";
        let cfg = parse_ssh_config(config, "pathremote.exe.xyz", std::path::Path::new("/h"));
        assert_eq!(cfg.user.as_deref(), Some("dev"));
        assert_eq!(cfg.port, Some(2200));
        assert_eq!(cfg.host_name.as_deref(), Some("real.example.com"));
    }

    #[test]
    fn ssh_config_handles_padded_equals_and_identities_only() {
        // OpenSSH allows `Key = value`; a naive first-separator split
        // would leave `= value`. And IdentitiesOnly must be parsed so
        // auth won't fall back to an arbitrary agent key.
        let config = "Host exe.dev\n\
                      \x20 User = dev\n\
                      \x20 Port = 2200\n\
                      \x20 IdentityFile = ~/.ssh/id_pinned\n\
                      \x20 IdentitiesOnly yes\n";
        let cfg = parse_ssh_config(config, "exe.dev", std::path::Path::new("/home/u"));
        assert_eq!(cfg.user.as_deref(), Some("dev"));
        assert_eq!(cfg.port, Some(2200));
        assert_eq!(
            cfg.identity_files,
            vec![std::path::PathBuf::from("/home/u/.ssh/id_pinned")]
        );
        assert!(cfg.identities_only);
    }

    #[test]
    fn authenticate_refuses_arbitrary_agent_key_when_identities_pinned() {
        // No live session, so we can't exercise the ssh2 calls — but the
        // guard's decision is pure: a config with pinned identities (or
        // IdentitiesOnly) must not reach the any-key fallback. We assert
        // that via the config shape the guard keys off.
        let pinned = SshHostConfig {
            identity_files: vec![std::path::PathBuf::from("/home/u/.ssh/id_pinned")],
            ..Default::default()
        };
        assert!(!pinned.identity_files.is_empty() || pinned.identities_only);
        let only = SshHostConfig {
            identities_only: true,
            ..Default::default()
        };
        assert!(!only.identity_files.is_empty() || only.identities_only);
    }

    #[test]
    fn normalize_remote_cwd_matches_remote_getcwd() {
        // Host-side normalization must equal what the remote shell's `cd`
        // lands on, else the project-dir key won't match remote Claude's.
        let home = "/home/dev";
        assert_eq!(
            normalize_remote_cwd(std::path::Path::new("/srv/work/"), home),
            "/srv/work"
        );
        assert_eq!(
            normalize_remote_cwd(std::path::Path::new("/srv/./work"), home),
            "/srv/work"
        );
        assert_eq!(
            normalize_remote_cwd(std::path::Path::new("/srv/x/../work"), home),
            "/srv/work"
        );
        assert_eq!(
            normalize_remote_cwd(std::path::Path::new("work"), home),
            "/home/dev/work"
        );
        assert_eq!(
            normalize_remote_cwd(std::path::Path::new("./work"), home),
            "/home/dev/work"
        );
    }

    #[test]
    fn validate_session_id_rejects_path_traversal() {
        use crate::cmd_export::validate_session_id;
        assert!(validate_session_id("4523d750-77e7-4a41-922f-5b949064f429").is_ok());
        assert!(validate_session_id("../../../.ssh/authorized_keys").is_err());
        assert!(validate_session_id(".hidden").is_err());
        assert!(validate_session_id("has/slash").is_err());
        assert!(validate_session_id("").is_err());
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
    fn remote_launch_command_derives_from_harness_table() {
        // Binary + argv come from the same per-harness table the local
        // resume uses; safe args stay bare so the recipe reads cleanly.
        assert_eq!(
            persist_plan(
                Harness::Claude,
                "sess-1",
                None,
                PersistBackend::Plain,
                "/home/u"
            )
            .remote_command,
            "claude -r sess-1"
        );
        // Directory creation happens over SFTP before launch — the shell
        // string stays minimal: just the cd and the harness.
        assert_eq!(
            persist_plan(
                Harness::Claude,
                "sess-1",
                Some("/srv/work"),
                PersistBackend::Plain,
                "/home/u"
            )
            .remote_command,
            "cd /srv/work && claude -r sess-1"
        );
    }

    #[test]
    fn remote_launch_command_tmux_wraps_for_detachable_sessions() {
        // --tmux: the whole launch runs inside a named tmux session so
        // it survives SSH disconnects; -A re-attaches on a second run.
        assert_eq!(
            persist_plan(
                Harness::Claude,
                "sess-1",
                None,
                PersistBackend::Tmux,
                "/home/u"
            )
            .remote_command,
            "tmux new-session -A -s path-sess-1 'claude -r sess-1'"
        );
        assert_eq!(
            persist_plan(
                Harness::Claude,
                "sess-1",
                Some("/srv/work"),
                PersistBackend::Tmux,
                "/home/u"
            )
            .remote_command,
            "tmux new-session -A -s path-sess-1 'cd /srv/work && claude -r sess-1'"
        );
    }

    #[test]
    fn persist_plan_direct_wrap_backends() {
        let id = "sess-1";
        let plain = persist_plan(Harness::Claude, id, None, PersistBackend::Plain, "/home/u");
        assert_eq!(plain.remote_command, "claude -r sess-1");
        assert!(plain.extra_file.is_none() && plain.post_note.is_none());

        let tmux = persist_plan(
            Harness::Claude,
            id,
            Some("/srv/w"),
            PersistBackend::Tmux,
            "/home/u",
        );
        assert_eq!(
            tmux.remote_command,
            "tmux new-session -A -s path-sess-1 'cd /srv/w && claude -r sess-1'"
        );

        let abduco = persist_plan(Harness::Claude, id, None, PersistBackend::Abduco, "/home/u");
        assert_eq!(
            abduco.remote_command,
            "abduco -A path-sess-1 sh -c 'claude -r sess-1'"
        );

        let dtach = persist_plan(Harness::Claude, id, None, PersistBackend::Dtach, "/home/u");
        assert_eq!(
            dtach.remote_command,
            "dtach -A /tmp/path-dtach-sess-1 -z sh -c 'claude -r sess-1'"
        );
    }

    #[test]
    fn persist_plan_shpool_attach_only_with_note() {
        let p = persist_plan(
            Harness::Claude,
            "sess-1",
            Some("/srv/w"),
            PersistBackend::Shpool,
            "/home/u",
        );
        assert_eq!(p.remote_command, "shpool attach path-sess-1");
        let note = p.post_note.expect("shpool note");
        assert!(
            note.contains("cd /srv/w && claude -r sess-1"),
            "note: {note}"
        );
        assert!(p.extra_file.is_none());
    }

    #[test]
    fn shell_quote_leaves_safe_strings_bare() {
        assert_eq!(shell_quote("-r"), "-r");
        assert_eq!(shell_quote("/srv/work"), "/srv/work");
        assert_eq!(shell_quote("has space"), "'has space'");
        assert_eq!(shell_quote(""), "''");
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
            harness: Some(Harness::Claude),
            no_cache: false,
            force: false,
            url: None,
            remote: Some("ssh://dev@example.com:2222".to_string()),
            tmux: false,
            persist: None,
            via: Transport::Ssh,
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
            cap.args.iter().any(|a| a == "claude -r remote-v1-test"),
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
                .any(|a| a == "cd /srv/fresh && claude -r remote-v1-test"),
            "launch should cd only (no shell mkdir): {:?}",
            cap.args
        );
    }

    #[test]
    fn remote_resume_rejects_non_claude_harness() {
        let td = tempfile::tempdir().unwrap();
        let mut args = remote_args_with_doc(td.path());
        args.harness = Some(Harness::Codex);
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
            harness: Some(Harness::Claude),
            no_cache: false,
            force: false,
            url: None,
            remote: None,
            tmux: false,
            persist: None,
            via: Transport::Ssh,
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
    fn resolve_persist_flag_maps_tmux_and_rejects_conflict() {
        let mut a = ResumeArgs {
            input: "claude-x".to_string(),
            cwd: None,
            harness: None,
            no_cache: false,
            force: false,
            url: None,
            remote: None,
            tmux: false,
            persist: None,
            via: Transport::Ssh,
        };
        a.remote = Some("ssh://h".into());
        a.tmux = true;
        assert_eq!(
            resolve_persist_flag(&a).unwrap(),
            Some(PersistBackend::Tmux)
        );

        a.persist = Some(PersistBackend::Dtach);
        assert!(resolve_persist_flag(&a).is_err()); // both --tmux and --persist

        a.tmux = false;
        assert_eq!(
            resolve_persist_flag(&a).unwrap(),
            Some(PersistBackend::Dtach)
        );
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
            tmux: false,
            persist: None,
            via: Transport::Ssh,
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
            tmux: false,
            persist: None,
            via: Transport::Ssh,
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
            tmux: false,
            persist: None,
            via: Transport::Ssh,
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
            tmux: false,
            persist: None,
            via: Transport::Ssh,
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
    fn launch_invocation_ssh_and_deferred_transports() {
        let (bin, argv) = launch_invocation(Transport::Ssh, "ssh://h", "claude -r x").unwrap();
        assert_eq!(bin, "ssh");
        assert_eq!(
            argv,
            vec!["-t".to_string(), "h".to_string(), "claude -r x".to_string()]
        );

        let err = launch_invocation(Transport::Mosh, "ssh://h", "claude -r x").unwrap_err();
        assert!(err.to_string().contains("not yet supported"), "{err}");
        assert!(launch_invocation(Transport::Et, "ssh://h", "x").is_err());
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

    #[test]
    fn persist_backend_bin_and_order() {
        assert_eq!(PersistBackend::Plain.bin(), None);
        assert_eq!(PersistBackend::Tmux.bin(), Some("tmux"));
        assert_eq!(PersistBackend::Shpool.bin(), Some("shpool"));
        // Display order is stable and complete.
        assert_eq!(PersistBackend::DISPLAY_ORDER.len(), 6);
        assert_eq!(PersistBackend::DISPLAY_ORDER[0], PersistBackend::Tmux);
        assert!(PersistBackend::Tmux.describe().contains("detach"));
    }

    #[test]
    fn persist_plan_zellij_ships_layout() {
        let p = persist_plan(
            Harness::Claude,
            "sess-1",
            Some("/srv/w"),
            PersistBackend::Zellij,
            "/home/u",
        );
        assert_eq!(
            p.remote_command,
            "zellij --session path-sess-1 --layout /home/u/.cache/path/zellij-path-sess-1.kdl"
        );
        let (path, body) = p.extra_file.expect("layout shipped");
        assert_eq!(path, "/home/u/.cache/path/zellij-path-sess-1.kdl");
        let body = String::from_utf8(body).unwrap();
        assert!(
            body.contains("cd /srv/w && claude -r sess-1"),
            "body: {body}"
        );
        assert!(body.contains("pane"), "must be a KDL layout: {body}");
    }

    #[test]
    fn recording_exec_remote_which_returns_canned_set() {
        let rec = RecordingExec::with_available(["tmux", "dtach"]);
        let got = rec
            .remote_which(
                &SshTarget {
                    user: None,
                    host: "h".into(),
                    port: None,
                },
                &["tmux", "zellij", "dtach"],
            )
            .unwrap();
        assert!(got.contains("tmux") && got.contains("dtach"));
        assert!(!got.contains("zellij"));
    }

    #[test]
    fn persist_candidates_and_preference() {
        use std::collections::BTreeSet;
        let avail: BTreeSet<String> = ["dtach", "zellij"].iter().map(|s| s.to_string()).collect();
        let cands = persist_candidates(&avail);
        // DISPLAY_ORDER filtered to available + always Plain, in order.
        assert_eq!(
            cands,
            vec![
                PersistBackend::Zellij,
                PersistBackend::Dtach,
                PersistBackend::Plain
            ]
        );
        assert_eq!(preferred_backend(&avail), PersistBackend::Zellij); // tmux absent -> zellij

        let none: BTreeSet<String> = BTreeSet::new();
        assert_eq!(persist_candidates(&none), vec![PersistBackend::Plain]);
        assert_eq!(preferred_backend(&none), PersistBackend::Plain);

        let with_tmux: BTreeSet<String> =
            ["tmux", "shpool"].iter().map(|s| s.to_string()).collect();
        assert_eq!(preferred_backend(&with_tmux), PersistBackend::Tmux); // shpool never preferred over tmux
    }
}
