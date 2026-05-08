//! `path resume` — fetch / load a Toolpath document and exec a coding
//! agent's resume command after projecting the session into the
//! harness's on-disk layout.
//!
//! See `docs/superpowers/specs/2026-05-08-path-resume-command-design.md`.

#![cfg(not(target_os = "emscripten"))]

#[allow(unused_imports)]
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
}
