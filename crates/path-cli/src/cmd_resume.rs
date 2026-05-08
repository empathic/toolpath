//! `path resume` — fetch / load a Toolpath document and exec a coding
//! agent's resume command after projecting the session into the
//! harness's on-disk layout.
//!
//! See `docs/superpowers/specs/2026-05-08-path-resume-command-design.md`.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;

use crate::cmd_share::HarnessArg;

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

    /// Skip writing the cache when fetching from Pathbase.
    #[arg(long)]
    pub no_cache: bool,

    /// Overwrite an existing cache entry when fetching from Pathbase.
    #[arg(long)]
    pub force: bool,

    /// Pathbase server URL. Falls back to the stored session's URL,
    /// then `$PATHBASE_URL`, then `https://pathbase.dev`.
    #[arg(long)]
    pub url: Option<String>,
}

pub fn run(_args: ResumeArgs) -> Result<()> {
    anyhow::bail!("path resume: not yet implemented")
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
            "opencode" => return Some(Harness::Opencode),
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
            let derived = crate::cmd_import::pathbase_fetch_to_doc(u, args.url.as_deref())?;
            if !args.no_cache {
                crate::cmd_cache::write_cached(&derived.cache_id, &derived.doc, args.force)?;
                eprintln!("Resolved {} → {}", raw, derived.cache_id);
            }
            derived.doc
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

const ALL_HARNESSES: &[crate::cmd_share::Harness] = &[
    crate::cmd_share::Harness::Claude,
    crate::cmd_share::Harness::Gemini,
    crate::cmd_share::Harness::Codex,
    crate::cmd_share::Harness::Opencode,
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
        if !binary_on_path(h.name(), path_override) {
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
        .filter(|h| binary_on_path(h.name(), path_override))
        .collect();

    if installed.is_empty() {
        anyhow::bail!(
            "no installed harnesses found on PATH; install one of: claude, gemini, codex, opencode, pi"
        );
    }

    interactive_pick(&installed, source)
}

fn interactive_pick(
    installed: &[crate::cmd_share::Harness],
    source: Option<crate::cmd_share::Harness>,
) -> Result<crate::cmd_share::Harness> {
    if !crate::fzf::available() {
        anyhow::bail!(
            "interactive picker requires `fzf` on PATH and a TTY; pass `--harness <X>` or rerun in a terminal"
        );
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

    let opts = crate::fzf::PickOptions {
        with_nth: "1..",
        header: Some(&header),
        ..Default::default()
    };
    let selected = match crate::fzf::pick(&lines, &opts)
        .map_err(|e| anyhow::anyhow!("fzf failed: {}", e))?
    {
        crate::fzf::PickResult::Selected(rows) => rows.into_iter().next().unwrap_or_default(),
        crate::fzf::PickResult::Cancelled => std::process::exit(130),
        crate::fzf::PickResult::NoMatch => {
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
        Harness::Claude   => vec!["-r".into(), session_id.into()],
        Harness::Gemini   => vec!["--resume".into(), session_id.into()],
        Harness::Codex    => vec!["resume".into(), session_id.into()],
        Harness::Opencode => vec!["--session".into(), session_id.into()],
        Harness::Pi       => vec!["--session".into(), session_id.into()],
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
        Harness::Claude   => crate::cmd_export::project_claude(path, cwd),
        Harness::Gemini   => crate::cmd_export::project_gemini(path, cwd),
        Harness::Codex    => crate::cmd_export::project_codex(path, cwd),
        Harness::Opencode => crate::cmd_export::project_opencode(path, cwd),
        Harness::Pi       => crate::cmd_export::project_pi(path, cwd),
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
            let status = cmd.spawn()
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
    segs.len() == 3 && segs.iter().all(|s| !s.is_empty() && !s.contains(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_returns_not_implemented_until_wired() {
        let args = ResumeArgs {
            input: "irrelevant".to_string(),
            cwd: None,
            harness: None,
            no_cache: false,
            force: false,
            url: None,
        };
        let err = run(args).unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
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
        g.paths
            .push(PathOrRef::Path(Box::new(make_path_with_actor("agent:claude-code"))));
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
        };
        let (g, harness) = resolve_input(&args).unwrap();
        let _path = ensure_path_with_agent(&g).unwrap();
        assert_eq!(harness, Some(Harness::Claude));
    }

    #[test]
    fn resolve_input_url_dispatches_to_pathbase_fetch() {
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
            input: format!("{}/alex/pathstash/p", server.base()),
            cwd: None,
            harness: None,
            no_cache: true, // skip cache write in tests
            force: false,
            url: None,
        };
        let (g, harness) = resolve_input(&args).unwrap();
        let _ = ensure_path_with_agent(&g).unwrap();
        assert_eq!(harness, Some(Harness::Codex));
    }

    #[test]
    fn resolve_input_unresolvable_errors_clearly() {
        let args = ResumeArgs {
            input: "definitely/not/a/real/cache/id".to_string(),
            cwd: None,
            harness: None,
            no_cache: false,
            force: false,
            url: None,
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
        assert_eq!(argv_for(Harness::Claude, "abc"),   vec!["-r".to_string(), "abc".to_string()]);
        assert_eq!(argv_for(Harness::Gemini, "abc"),   vec!["--resume".to_string(), "abc".to_string()]);
        assert_eq!(argv_for(Harness::Codex, "abc"),    vec!["resume".to_string(), "abc".to_string()]);
        assert_eq!(argv_for(Harness::Opencode, "abc"), vec!["--session".to_string(), "abc".to_string()]);
        assert_eq!(argv_for(Harness::Pi, "abc"),       vec!["--session".to_string(), "abc".to_string()]);
    }

    #[test]
    fn project_into_harness_claude_round_trip() {
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

    struct ScopedHomeForResume { _td: tempfile::TempDir, prev: Option<std::ffi::OsString> }

    impl ScopedHomeForResume {
        fn new() -> Self {
            let td = tempfile::tempdir().unwrap();
            let prev = std::env::var_os("HOME");
            unsafe { std::env::set_var("HOME", td.path()); }
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
        exec_harness("claude", &["-r".into(), "abc123".into()], std::path::Path::new("/tmp/x"), strategy)
            .unwrap();

        let captured = recorder.captured();
        assert_eq!(captured.binary, "claude");
        assert_eq!(captured.args, vec!["-r".to_string(), "abc123".to_string()]);
        assert_eq!(captured.cwd, std::path::PathBuf::from("/tmp/x"));
    }
}
