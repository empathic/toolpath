# `path share` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `path share` command that aggregates agent sessions across installed harnesses, ranks current-project sessions first in a single fzf picker, and uploads the picked session to Pathbase in one shot.

**Architecture:** New `cmd_share.rs` module in `crates/path-cli/src/`. Reuses derive helpers from `cmd_import.rs` (lifted to `pub(crate)` as single-pair functions) and the upload helper from `cmd_export.rs` (refactored into a body-taking `run_pathbase_inner`). Aggregation, picker, and CLI dispatch live in the new module.

**Tech Stack:** Rust 2024, clap (CLI), reqwest+tokio (HTTP via shared `cmd_pathbase` helpers), `fzf` (interactive picker), the existing `toolpath-{claude,gemini,codex,opencode,pi}` provider crates.

**Spec:** `docs/superpowers/specs/2026-05-07-path-share-command-design.md` (commit `b3ee214`).

---

## File map

- **Modify** `crates/path-cli/src/cmd_import.rs` — lift `DerivedDoc` to `pub(crate)`; extract single-pair derive helpers as `pub(crate) fn`s.
- **Modify** `crates/path-cli/src/cmd_export.rs` — split `run_pathbase` into `run_pathbase_inner(args, body)` + thin wrapper; add `pub(crate) struct PathbaseUploadArgs`.
- **Create** `crates/path-cli/src/cmd_share.rs` — module: types (`Harness`, `SessionRow`, `HarnessBundle`), aggregation (`gather_sessions`), picker, dispatch (`run`).
- **Modify** `crates/path-cli/src/lib.rs` — add `mod cmd_share;` and `Commands::Share { args }` enum arm.
- **Modify** `crates/path-cli/tests/integration.rs` — add `share_*` integration tests.
- **Modify** `CLAUDE.md` — add a `path share` line to the CLI usage block, and one item to "Things to know" describing the unified picker.

---

## Task 1: Refactor `cmd_import.rs` — lift visibility, extract single-pair derive helpers

Mechanical refactor; no behavior change. The new `pub(crate)` helpers each derive a `DerivedDoc` for one explicit `(project, session)` or `session` pair, so `cmd_share` can call them after its own picker resolves a row.

**Files:**
- Modify: `crates/path-cli/src/cmd_import.rs`

- [ ] **Step 1.1: Lift `DerivedDoc` to `pub(crate)`**

In `crates/path-cli/src/cmd_import.rs` around line 174, change:

```rust
struct DerivedDoc {
    cache_id: String,
    doc: Graph,
}
```

to:

```rust
pub(crate) struct DerivedDoc {
    pub(crate) cache_id: String,
    pub(crate) doc: Graph,
}
```

- [ ] **Step 1.2: Add `derive_claude_pair`**

Add this function next to `derive_claude_with_manager` (around line 369):

```rust
/// Derive a single Claude conversation given an explicit project + session.
/// Used by `cmd_share` after its picker has resolved the pair; mirrors the
/// `(Some(p), Some(s), _)` arm in [`derive_claude_with_manager`].
pub(crate) fn derive_claude_pair(project: &str, session: &str) -> Result<DerivedDoc> {
    let manager = toolpath_claude::ClaudeConvo::new();
    let cfg = toolpath_claude::derive::DeriveConfig {
        project_path: Some(project.to_string()),
        include_thinking: false,
    };
    let convo = manager
        .read_conversation(project, session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_claude::derive::derive_path(&convo, &cfg);
    let cache_id = make_id("claude", &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
    })
}
```

- [ ] **Step 1.3: Add `derive_gemini_pair`**

Add this function next to `derive_gemini_with_manager` (around line 562):

```rust
/// Derive a single Gemini conversation given an explicit project + session.
pub(crate) fn derive_gemini_pair(
    project: &str,
    session: &str,
    include_thinking: bool,
) -> Result<DerivedDoc> {
    let manager = toolpath_gemini::GeminiConvo::new();
    let cfg = toolpath_gemini::derive::DeriveConfig {
        project_path: Some(project.to_string()),
        include_thinking,
    };
    let convo = manager
        .read_conversation(project, session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_gemini::derive::derive_path(&convo, &cfg);
    let cache_id = make_id("gemini", &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
    })
}
```

- [ ] **Step 1.4: Add `derive_pi_pair`**

Add this function next to `derive_pi_with_manager` (around line 995):

```rust
/// Derive a single Pi session given an explicit project + session.
pub(crate) fn derive_pi_pair(
    project: &str,
    session: &str,
    base: Option<PathBuf>,
) -> Result<DerivedDoc> {
    let manager = if let Some(path) = base {
        let resolver = toolpath_pi::PathResolver::new().with_sessions_dir(&path);
        toolpath_pi::PiConvo::with_resolver(resolver)
    } else {
        toolpath_pi::PiConvo::new()
    };
    let config = toolpath_pi::DeriveConfig::default();
    let session = manager
        .read_session(project, session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let doc = Graph::from_path(toolpath_pi::derive::derive_path(&session, &config));
    let cache_id = make_id("pi", &doc_inner_id(&doc));
    Ok(DerivedDoc { cache_id, doc })
}
```

- [ ] **Step 1.5: Add `derive_codex_one`**

Add this function next to `derive_codex` (around line 738):

```rust
/// Derive a single Codex session given an explicit session id.
pub(crate) fn derive_codex_one(session: &str) -> Result<DerivedDoc> {
    let manager = toolpath_codex::CodexConvo::new();
    let config = toolpath_codex::derive::DeriveConfig { project_path: None };
    let s = manager
        .read_session(session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_codex::derive::derive_path(&s, &config);
    let cache_id = make_id("codex", &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
    })
}
```

- [ ] **Step 1.6: Add `derive_opencode_one`**

Add this function next to `derive_opencode` (around line 848). Wrap in the same `cfg(not(target_os = "emscripten"))` gate the rest of opencode uses:

```rust
/// Derive a single opencode session given an explicit session id.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn derive_opencode_one(
    session: &str,
    no_snapshot_diffs: bool,
) -> Result<DerivedDoc> {
    let manager = toolpath_opencode::OpencodeConvo::new();
    let config = toolpath_opencode::derive::DeriveConfig {
        no_snapshot_diffs,
        ..Default::default()
    };
    let s = manager
        .read_session(session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path =
        toolpath_opencode::derive::derive_path_with_resolver(&s, &config, manager.resolver());
    let cache_id = make_id("opencode", &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
    })
}
```

- [ ] **Step 1.7: Verify the workspace still builds and tests pass**

```bash
cargo build -p path-cli
cargo test -p path-cli --lib
```

Expected: build succeeds, all existing tests pass (this was a pure addition — no call sites rewritten).

- [ ] **Step 1.8: Commit**

```bash
git add crates/path-cli/src/cmd_import.rs
git commit -m "refactor(path-cli): extract single-pair derive helpers

Lifts DerivedDoc to pub(crate) and adds derive_{claude,gemini,pi}_pair
and derive_{codex,opencode}_one. These are the explicit-args paths
already exercised by the (Some(p), Some(s), _) arm of each existing
dispatch — extracted so cmd_share can reuse them without re-implementing
the per-harness wiring."
```

---

## Task 2: Refactor `cmd_export.rs` — split `run_pathbase` so the body can come from memory

Today `run_pathbase` reads from a cache file. `cmd_share` has the derived `Graph` in memory; we want to upload without writing-then-reading. Extract a `run_pathbase_inner(args, body)` and have the existing wrapper read the file then call the inner.

**Files:**
- Modify: `crates/path-cli/src/cmd_export.rs`

- [ ] **Step 2.1: Add `pub(crate) struct PathbaseUploadArgs`**

Add this near the existing `struct PathbaseExportArgs` (around line 219):

```rust
/// Pathbase upload knobs that don't depend on where the body came from.
/// Identical to [`PathbaseExportArgs`] minus the `input` field — the body
/// is supplied by the caller (read from cache, derived in memory, …).
#[derive(Debug)]
pub(crate) struct PathbaseUploadArgs {
    pub(crate) url: Option<String>,
    pub(crate) anon: bool,
    pub(crate) repo: Option<RepoSpec>,
    pub(crate) slug: Option<String>,
    pub(crate) public: bool,
}
```

- [ ] **Step 2.2: Lift `RepoSpec` and `parse_repo_spec` to `pub(crate)`**

In the same file, change:

```rust
#[derive(Debug, Clone)]
pub struct RepoSpec {
    pub owner: String,
    pub name: String,
}

fn parse_repo_spec(s: &str) -> std::result::Result<RepoSpec, String> {
```

so both `pub` items become `pub(crate)` (already `pub` for `RepoSpec`; convert for `parse_repo_spec`):

```rust
#[derive(Debug, Clone)]
pub(crate) struct RepoSpec {
    pub(crate) owner: String,
    pub(crate) name: String,
}

pub(crate) fn parse_repo_spec(s: &str) -> std::result::Result<RepoSpec, String> {
```

- [ ] **Step 2.3: Extract `run_pathbase_inner`**

Replace the body of `run_pathbase` (lines 1202–1329 — the `#[cfg(not(target_os = "emscripten"))]` arm) so that it reads the file then calls a new inner. The new shape:

```rust
fn run_pathbase(args: PathbaseExportArgs) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = args;
        anyhow::bail!("'path export pathbase' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let file = cache_ref(&args.input)?;
        let body = std::fs::read_to_string(&file)
            .with_context(|| format!("Failed to read {}", file.display()))?;
        let upload = PathbaseUploadArgs {
            url: args.url,
            anon: args.anon,
            repo: args.repo,
            slug: args.slug,
            public: args.public,
        };
        let summary_source = file.display().to_string();
        run_pathbase_inner(upload, &body, &summary_source)
    }
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn run_pathbase_inner(
    args: PathbaseUploadArgs,
    body: &str,
    summary_source: &str,
) -> Result<()> {
    use crate::cmd_pathbase::{
        anon_paths_post, api_me, credentials_path, load_session, paths_post, repos_post,
        resolve_url,
    };

    // Validate locally so we give a clean error rather than relying on
    // the server to reject malformed payloads.
    let doc = toolpath::v1::Graph::from_json(body)
        .map_err(|e| anyhow::anyhow!("Invalid toolpath document: {}", e))?;

    let stored = load_session(&credentials_path()?)?;
    let base_url = match (&args.url, &stored) {
        (Some(u), _) => resolve_url(Some(u.clone())),
        (None, Some(s)) => s.url.clone(),
        (None, None) => resolve_url(None),
    };

    let go_anon = args.anon || (stored.is_none() && args.repo.is_none() && args.slug.is_none());

    if go_anon {
        if !args.anon && stored.is_none() {
            eprintln!(
                "note: not logged in — uploading anonymously (not listable). Run `path auth login --url {base_url}` for a listable upload."
            );
        }
        let resp = anon_paths_post(&base_url, body)?;
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
            summary_source,
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
            let user = api_me(&base_url, &session.token)?;
            repos_post(&base_url, &session.token, "pathstash")?;
            (user.username, "pathstash".to_string())
        }
    };

    let slug = args.slug.unwrap_or_else(|| derive_slug(&doc));
    let created = paths_post(
        &base_url,
        &session.token,
        &owner,
        &repo,
        &slug,
        body,
        args.public,
    )?;

    if created.is_public != args.public {
        eprintln!(
            "note: requested is_public={} but server applied is_public={}",
            args.public, created.is_public
        );
    }
    let visibility = if created.is_public { "public" } else { "secret" };
    let url = pathbase_share_url(
        &base_url,
        &owner,
        &repo,
        &created.slug,
        &created.id,
        created.is_public,
    );
    println!("{url}");
    eprintln!(
        "Uploaded {} → {}/{}/{} ({} path, {} bytes)",
        summary_source,
        owner,
        repo,
        created.slug,
        visibility,
        body.len()
    );
    Ok(())
}
```

`summary_source` is the human-readable label used in the stderr "Uploaded …" line — `cache_ref` path for `export pathbase`, and a synthesized "<harness> session <id>" string for `cmd_share`. Keeps the inner free of cache-vs-memory branching.

- [ ] **Step 2.4: Verify the workspace still builds and tests pass**

```bash
cargo build -p path-cli
cargo test -p path-cli
```

Expected: existing `pathbase_*` tests in `cmd_pathbase.rs` and `export_pathbase_repo_flag_requires_login` integration test still pass — refactor preserved behavior.

- [ ] **Step 2.5: Commit**

```bash
git add crates/path-cli/src/cmd_export.rs
git commit -m "refactor(path-cli): split run_pathbase into wrapper + inner

run_pathbase_inner takes a body string and a summary_source label, so
callers with an in-memory toolpath document (cmd_share) can upload
without round-tripping through the cache."
```

---

## Task 3: Scaffold `cmd_share.rs` and wire into `lib.rs`

Empty module with the CLI surface and a `run()` stub that errors. This is the smallest change that lets `path share --help` print and `path share` produce a recognisable "not implemented yet" failure, so subsequent tasks can be tested incrementally.

**Files:**
- Create: `crates/path-cli/src/cmd_share.rs`
- Modify: `crates/path-cli/src/lib.rs`

- [ ] **Step 3.1: Write the failing test for the help output**

Append to `crates/path-cli/tests/integration.rs`:

```rust
#[test]
fn share_help_lists_unified_picker_flags() {
    cmd()
        .args(["share", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--harness"))
        .stdout(predicate::str::contains("--session"))
        .stdout(predicate::str::contains("--project"))
        .stdout(predicate::str::contains("--anon"));
}
```

- [ ] **Step 3.2: Run the test to confirm it fails**

```bash
cargo test -p path-cli --test integration share_help_lists_unified_picker_flags
```

Expected: FAIL — `path share` is not yet a recognised subcommand.

- [ ] **Step 3.3: Create `cmd_share.rs`**

```rust
//! `path share` — interactive Pathbase upload across installed agent
//! harnesses. See `docs/superpowers/specs/2026-05-07-path-share-command-design.md`.

#![cfg(not(target_os = "emscripten"))]

use anyhow::Result;
use clap::{Args, ValueEnum};
use std::path::PathBuf;

use crate::cmd_export::RepoSpec;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum HarnessArg {
    Claude,
    Gemini,
    Codex,
    Opencode,
    Pi,
}

#[derive(Args, Debug)]
pub struct ShareArgs {
    /// Pathbase server URL (defaults to the stored session's server)
    #[arg(long)]
    pub url: Option<String>,

    /// Force the anonymous endpoint, ignoring any stored credentials
    #[arg(long, conflicts_with_all = ["repo", "public"])]
    pub anon: bool,

    /// Target a specific repo as `owner/name` instead of `<you>/pathstash`
    #[arg(long, value_parser = crate::cmd_export::parse_repo_spec)]
    pub repo: Option<RepoSpec>,

    /// Override the auto-derived slug (defaults to the toolpath document id)
    #[arg(long)]
    pub slug: Option<String>,

    /// Make the uploaded path publicly listable (default: secret/unlisted)
    #[arg(long)]
    pub public: bool,

    /// Narrow the picker to one harness, or skip the picker entirely
    /// when used with --session.
    #[arg(long, value_enum)]
    pub harness: Option<HarnessArg>,

    /// Skip the picker. Requires --harness; requires --project for
    /// claude/gemini/pi.
    #[arg(long, requires = "harness")]
    pub session: Option<String>,

    /// Override cwd-as-project. Filters the picker to sessions tied to
    /// this project across all harnesses.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Overwrite the cache entry if it already exists
    #[arg(long)]
    pub force: bool,

    /// Skip writing the cache; derive in-memory only
    #[arg(long)]
    pub no_cache: bool,
}

pub fn run(args: ShareArgs) -> Result<()> {
    let _ = args;
    anyhow::bail!("`path share` is not yet implemented")
}
```

- [ ] **Step 3.4: Wire it into `lib.rs`**

In `crates/path-cli/src/lib.rs`, add the module declaration alongside the others:

```rust
#[cfg(not(target_os = "emscripten"))]
mod cmd_share;
```

Add to the `Commands` enum (anywhere among the existing arms; placing it next to `Auth` is natural):

```rust
    /// Share an agent session to Pathbase via an interactive picker
    #[cfg(not(target_os = "emscripten"))]
    Share {
        #[command(flatten)]
        args: cmd_share::ShareArgs,
    },
```

Add the dispatch arm in `pub fn run`:

```rust
        #[cfg(not(target_os = "emscripten"))]
        Commands::Share { args } => cmd_share::run(args),
```

- [ ] **Step 3.5: Run the help test to verify it passes**

```bash
cargo test -p path-cli --test integration share_help_lists_unified_picker_flags
```

Expected: PASS.

- [ ] **Step 3.6: Confirm `path share` runs and bails with the stub error**

```bash
cargo run -p path-cli -- share 2>&1 | head -3
```

Expected: stderr says `Error: \`path share\` is not yet implemented`.

- [ ] **Step 3.7: Commit**

```bash
git add crates/path-cli/src/cmd_share.rs crates/path-cli/src/lib.rs crates/path-cli/tests/integration.rs
git commit -m "feat(path-cli): scaffold \`path share\` command

Adds the cmd_share module with the full CLI surface (--url, --harness,
--session, --project, --anon, --repo, --slug, --public, --force,
--no-cache) and a stub run() that bails. Wires it into lib.rs as
Commands::Share. Subsequent tasks fill in the body."
```

---

## Task 4: Add `Harness`, `SessionRow`, and `HarnessBundle` types

Pure types with small helper methods. No aggregation logic yet — that comes in tasks 5 and 6. Splitting it out keeps the test fixtures focused.

**Files:**
- Modify: `crates/path-cli/src/cmd_share.rs`

- [ ] **Step 4.1: Write the failing tests for the type helpers**

Append to `crates/path-cli/src/cmd_share.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_name_and_symbol_are_distinct() {
        let all = [
            Harness::Claude,
            Harness::Gemini,
            Harness::Codex,
            Harness::Opencode,
            Harness::Pi,
        ];
        let names: Vec<&str> = all.iter().map(|h| h.name()).collect();
        let symbols: Vec<&str> = all.iter().map(|h| h.symbol()).collect();
        assert_eq!(names.len(), 5);
        assert_eq!(
            names.iter().collect::<std::collections::HashSet<_>>().len(),
            5,
            "names must be unique"
        );
        assert_eq!(
            symbols
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            5,
            "symbols must be unique"
        );
    }

    #[test]
    fn harness_project_keyed_matches_design() {
        assert!(Harness::Claude.project_keyed());
        assert!(Harness::Gemini.project_keyed());
        assert!(Harness::Pi.project_keyed());
        assert!(!Harness::Codex.project_keyed());
        assert!(!Harness::Opencode.project_keyed());
    }

    #[test]
    fn harness_from_arg_roundtrips() {
        for (arg, harness) in [
            (HarnessArg::Claude, Harness::Claude),
            (HarnessArg::Gemini, Harness::Gemini),
            (HarnessArg::Codex, Harness::Codex),
            (HarnessArg::Opencode, Harness::Opencode),
            (HarnessArg::Pi, Harness::Pi),
        ] {
            assert_eq!(Harness::from_arg(arg), harness);
        }
    }
}
```

- [ ] **Step 4.2: Run the tests to confirm they fail**

```bash
cargo test -p path-cli --lib cmd_share
```

Expected: FAIL — `Harness`, `SessionRow`, etc. don't exist yet.

- [ ] **Step 4.3: Add the types**

Insert above the `pub fn run` definition:

```rust
use chrono::{DateTime, Utc};

/// Which agent harness a session was produced by.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Harness {
    Claude,
    Gemini,
    Codex,
    Opencode,
    Pi,
}

impl Harness {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Gemini => "gemini",
            Harness::Codex => "codex",
            Harness::Opencode => "opencode",
            Harness::Pi => "pi",
        }
    }

    /// Padded so all five symbols line up in the fzf column.
    pub(crate) fn symbol(&self) -> &'static str {
        match self {
            Harness::Claude => "claude  ",
            Harness::Gemini => "gemini  ",
            Harness::Codex => "codex   ",
            Harness::Opencode => "opencode",
            Harness::Pi => "pi      ",
        }
    }

    /// True when the underlying provider keys sessions by project path.
    /// claude/gemini/pi: true. codex/opencode: false (sessions store cwd
    /// per-row, not as a directory key).
    pub(crate) fn project_keyed(&self) -> bool {
        matches!(self, Harness::Claude | Harness::Gemini | Harness::Pi)
    }

    pub(crate) fn from_arg(arg: HarnessArg) -> Self {
        match arg {
            HarnessArg::Claude => Harness::Claude,
            HarnessArg::Gemini => Harness::Gemini,
            HarnessArg::Codex => Harness::Codex,
            HarnessArg::Opencode => Harness::Opencode,
            HarnessArg::Pi => Harness::Pi,
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Harness::Claude),
            "gemini" => Some(Harness::Gemini),
            "codex" => Some(Harness::Codex),
            "opencode" => Some(Harness::Opencode),
            "pi" => Some(Harness::Pi),
            _ => None,
        }
    }
}

/// One row in the unified session picker.
#[derive(Debug, Clone)]
pub(crate) struct SessionRow {
    pub(crate) harness: Harness,
    /// Project path for keyed providers; `None` for codex/opencode.
    pub(crate) project: Option<String>,
    /// Recorded cwd from the session (codex/opencode only).
    pub(crate) cwd: Option<String>,
    pub(crate) session_id: String,
    pub(crate) title: String,
    pub(crate) last_activity: Option<DateTime<Utc>>,
    pub(crate) message_count: usize,
    pub(crate) matches_cwd: bool,
}

/// Bundle of provider managers used during aggregation. Production code
/// builds this from real `$HOME` via `from_environment`; tests construct
/// it directly with provider-specific resolvers.
#[derive(Default)]
pub(crate) struct HarnessBundle {
    pub(crate) claude: Option<toolpath_claude::ClaudeConvo>,
    pub(crate) gemini: Option<toolpath_gemini::GeminiConvo>,
    pub(crate) codex: Option<toolpath_codex::CodexConvo>,
    pub(crate) opencode: Option<toolpath_opencode::OpencodeConvo>,
    pub(crate) pi: Option<toolpath_pi::PiConvo>,
}

impl HarnessBundle {
    /// Build the production bundle. Each provider is included
    /// unconditionally (its `new()` doesn't fail on a missing home dir);
    /// `gather_sessions` skips the ones whose listing returns empty/NotFound.
    pub(crate) fn from_environment() -> Self {
        Self {
            claude: Some(toolpath_claude::ClaudeConvo::new()),
            gemini: Some(toolpath_gemini::GeminiConvo::new()),
            codex: Some(toolpath_codex::CodexConvo::new()),
            opencode: Some(toolpath_opencode::OpencodeConvo::new()),
            pi: Some(toolpath_pi::PiConvo::new()),
        }
    }
}
```

- [ ] **Step 4.4: Run the tests to verify they pass**

```bash
cargo test -p path-cli --lib cmd_share
```

Expected: PASS.

- [ ] **Step 4.5: Commit**

```bash
git add crates/path-cli/src/cmd_share.rs
git commit -m "feat(path-cli): add Harness, SessionRow, HarnessBundle types

Pure data types plus from_arg/parse helpers and a project_keyed
predicate. HarnessBundle::from_environment instantiates each provider
unconditionally; gather_sessions (next task) skips providers whose
listing returns empty or NotFound."
```

---

## Task 5: Implement `gather_sessions` for project-keyed harnesses (claude, gemini, pi)

Aggregator collects rows from claude/gemini/pi only in this task. Codex/opencode arrive in task 6. Each provider gets one unit test that uses an injectable resolver to point at a tempdir.

**Files:**
- Modify: `crates/path-cli/src/cmd_share.rs`

- [ ] **Step 5.1: Write the failing tests**

Append to the `mod tests` block in `cmd_share.rs`:

```rust
    use std::path::Path;
    use tempfile::TempDir;

    fn write_claude_session(claude_dir: &Path, project_slug: &str, session: &str, prompt: &str) {
        let project_dir = claude_dir.join("projects").join(project_slug);
        std::fs::create_dir_all(&project_dir).unwrap();
        let user = format!(
            r#"{{"type":"user","uuid":"u-{session}","timestamp":"2024-01-02T00:00:00Z","cwd":"/test/project","message":{{"role":"user","content":"{prompt}"}}}}"#
        );
        let asst = format!(
            r#"{{"type":"assistant","uuid":"a-{session}","timestamp":"2024-01-02T00:00:01Z","message":{{"role":"assistant","content":"hi"}}}}"#
        );
        std::fs::write(
            project_dir.join(format!("{session}.jsonl")),
            format!("{user}\n{asst}\n"),
        )
        .unwrap();
    }

    fn claude_only_bundle(home: &Path) -> HarnessBundle {
        let claude_dir = home.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        HarnessBundle {
            claude: Some(toolpath_claude::ClaudeConvo::with_resolver(resolver)),
            ..Default::default()
        }
    }

    #[test]
    fn gather_sessions_includes_claude_rows_for_a_project() {
        let temp = TempDir::new().unwrap();
        write_claude_session(
            &temp.path().join(".claude"),
            "-test-project",
            "abc-session-one",
            "Add a feature",
        );
        let bundle = claude_only_bundle(temp.path());
        let cwd = Path::new("/test/project");
        let rows = gather_sessions(&bundle, cwd, None, None);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].harness, Harness::Claude);
        assert_eq!(rows[0].session_id, "abc-session-one");
        assert_eq!(rows[0].project.as_deref(), Some("/test/project"));
        assert!(rows[0].matches_cwd, "cwd should match the project path");
    }

    #[test]
    fn gather_sessions_marks_non_matching_project_rows() {
        let temp = TempDir::new().unwrap();
        write_claude_session(
            &temp.path().join(".claude"),
            "-test-project",
            "abc-session-one",
            "Add a feature",
        );
        let bundle = claude_only_bundle(temp.path());
        let cwd = Path::new("/some/other/place");
        let rows = gather_sessions(&bundle, cwd, None, None);

        assert_eq!(rows.len(), 1);
        assert!(!rows[0].matches_cwd);
    }

    #[test]
    fn gather_sessions_skips_harness_with_no_home_dir() {
        // Empty bundle => no rows, no panic.
        let bundle = HarnessBundle::default();
        let rows = gather_sessions(&bundle, Path::new("/anywhere"), None, None);
        assert!(rows.is_empty());
    }

    #[test]
    fn gather_sessions_filters_by_harness() {
        let temp = TempDir::new().unwrap();
        write_claude_session(
            &temp.path().join(".claude"),
            "-test-project",
            "abc-session-one",
            "hi",
        );
        let bundle = claude_only_bundle(temp.path());
        let cwd = Path::new("/test/project");
        let rows = gather_sessions(&bundle, cwd, Some(Harness::Codex), None);
        assert!(rows.is_empty(), "filter to codex must drop claude rows");
    }
```

- [ ] **Step 5.2: Run the tests to confirm they fail**

```bash
cargo test -p path-cli --lib cmd_share::tests::gather
```

Expected: FAIL — `gather_sessions` doesn't exist.

- [ ] **Step 5.3: Implement `gather_sessions` for the three project-keyed harnesses**

Add above the `mod tests` block:

```rust
/// Aggregate sessions across the harnesses in `bundle`, ranked so that
/// rows whose project (or recorded cwd) canonicalizes to `cwd` come
/// first, sorted by descending `last_activity`.
///
/// Filters: `harness_filter` keeps only rows from one harness; `project_filter`
/// keeps only rows whose project (for keyed) or cwd (for session-keyed)
/// canonicalizes to that path.
pub(crate) fn gather_sessions(
    bundle: &HarnessBundle,
    cwd: &std::path::Path,
    harness_filter: Option<Harness>,
    project_filter: Option<&std::path::Path>,
) -> Vec<SessionRow> {
    let mut rows = Vec::new();
    let canonical_cwd = canonicalize_or_self(cwd);
    let canonical_project = project_filter.map(canonicalize_or_self);

    let want = |h: Harness| harness_filter.is_none_or(|f| f == h);

    if want(Harness::Claude) {
        if let Some(mgr) = &bundle.claude {
            collect_claude(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
        }
    }
    if want(Harness::Gemini) {
        if let Some(mgr) = &bundle.gemini {
            collect_gemini(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
        }
    }
    if want(Harness::Pi) {
        if let Some(mgr) = &bundle.pi {
            collect_pi(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
        }
    }

    rows.sort_by(|a, b| {
        b.matches_cwd
            .cmp(&a.matches_cwd)
            .then_with(|| b.last_activity.cmp(&a.last_activity))
    });
    rows
}

fn canonicalize_or_self(p: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn paths_match(a: &std::path::Path, b: &std::path::Path) -> bool {
    canonicalize_or_self(a) == canonicalize_or_self(b)
}

fn collect_claude(
    mgr: &toolpath_claude::ClaudeConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<SessionRow>,
) {
    let projects = match mgr.list_projects() {
        Ok(ps) if !ps.is_empty() => ps,
        Ok(_) => return,
        Err(e) if is_not_found(&e) => return,
        Err(e) => {
            eprintln!("warning: claude aggregation failed: {e}");
            return;
        }
    };
    for project in projects {
        let project_path = std::path::Path::new(&project);
        if let Some(filter) = project_filter
            && !paths_match(project_path, filter)
        {
            continue;
        }
        let metas = match mgr.list_conversation_metadata(&project) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("warning: claude project {project} failed: {e}");
                continue;
            }
        };
        let matches_cwd = paths_match(project_path, canonical_cwd);
        for m in metas {
            out.push(SessionRow {
                harness: Harness::Claude,
                project: Some(m.project_path),
                cwd: None,
                session_id: m.session_id,
                title: m
                    .first_user_message
                    .unwrap_or_else(|| "(no prompt)".to_string()),
                last_activity: m.last_activity,
                message_count: m.message_count,
                matches_cwd,
            });
        }
    }
}

fn collect_gemini(
    mgr: &toolpath_gemini::GeminiConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<SessionRow>,
) {
    let projects = match mgr.list_projects() {
        Ok(ps) if !ps.is_empty() => ps,
        Ok(_) => return,
        Err(e) if is_not_found(&e) => return,
        Err(e) => {
            eprintln!("warning: gemini aggregation failed: {e}");
            return;
        }
    };
    for project in projects {
        let project_path = std::path::Path::new(&project);
        if let Some(filter) = project_filter
            && !paths_match(project_path, filter)
        {
            continue;
        }
        let metas = match mgr.list_conversation_metadata(&project) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("warning: gemini project {project} failed: {e}");
                continue;
            }
        };
        let matches_cwd = paths_match(project_path, canonical_cwd);
        for m in metas {
            out.push(SessionRow {
                harness: Harness::Gemini,
                project: Some(m.project_path),
                cwd: None,
                session_id: m.session_uuid,
                title: m
                    .first_user_message
                    .unwrap_or_else(|| "(no prompt)".to_string()),
                last_activity: m.last_activity,
                message_count: m.message_count,
                matches_cwd,
            });
        }
    }
}

fn collect_pi(
    mgr: &toolpath_pi::PiConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<SessionRow>,
) {
    let projects = match mgr.list_projects() {
        Ok(ps) if !ps.is_empty() => ps,
        Ok(_) => return,
        Err(e) if is_not_found_pi(&e) => return,
        Err(e) => {
            eprintln!("warning: pi aggregation failed: {e}");
            return;
        }
    };
    for project in projects {
        let project_path = std::path::Path::new(&project);
        if let Some(filter) = project_filter
            && !paths_match(project_path, filter)
        {
            continue;
        }
        let metas = match mgr.list_sessions(&project) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("warning: pi project {project} failed: {e}");
                continue;
            }
        };
        let matches_cwd = paths_match(project_path, canonical_cwd);
        for m in metas {
            // SessionMeta.timestamp is a String; parse to DateTime when possible.
            let last_activity = chrono::DateTime::parse_from_rfc3339(&m.timestamp)
                .ok()
                .map(|d| d.with_timezone(&Utc));
            out.push(SessionRow {
                harness: Harness::Pi,
                project: Some(project.clone()),
                cwd: None,
                session_id: m.id,
                title: m
                    .first_user_message
                    .unwrap_or_else(|| "(no prompt)".to_string()),
                last_activity,
                message_count: m.entry_count,
                matches_cwd,
            });
        }
    }
}

fn is_not_found(err: &toolpath_claude::ConvoError) -> bool {
    use toolpath_claude::ConvoError;
    matches!(err, ConvoError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, ConvoError::NoHomeDirectory)
        || matches!(err, ConvoError::ClaudeDirectoryNotFound(_))
}

fn is_not_found_pi(err: &toolpath_pi::PiError) -> bool {
    use toolpath_pi::PiError;
    matches!(err, PiError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, PiError::ProjectNotFound(_))
}
```

Note: claude / gemini / codex / opencode all re-export `ConvoError` with `Io(io::Error)` and `NoHomeDirectory` variants. Pi re-exports `PiError` (different name) with `Io` and `ProjectNotFound` variants. The helpers above already account for that. Variant names were verified against `crates/toolpath-{claude,gemini,codex,opencode,pi}/src/error.rs` while writing this plan.

- [ ] **Step 5.4: Run the tests to verify they pass**

```bash
cargo test -p path-cli --lib cmd_share
```

Expected: PASS. If `is_not_found` doesn't compile, inspect the provider's `ConvoError` enum and adjust the match arms; the test set still passes once it compiles because the fixture has a real home.

- [ ] **Step 5.5: Run clippy to catch warning-as-error issues**

```bash
cargo clippy -p path-cli -- -D warnings
```

Expected: clean.

- [ ] **Step 5.6: Commit**

```bash
git add crates/path-cli/src/cmd_share.rs
git commit -m "feat(path-cli): implement gather_sessions for claude/gemini/pi

Aggregates SessionRow values from the three project-keyed providers,
sorts cwd-matching rows first then by recency, and silently skips
harnesses whose listing returns empty or NotFound. Codex and opencode
land in the next commit."
```

---

## Task 6: Extend `gather_sessions` to codex and opencode + add ranking/filter coverage

Codex and opencode address sessions by id; their `cwd` lives inside the session metadata, so the matching logic differs slightly from the project-keyed harnesses.

**Files:**
- Modify: `crates/path-cli/src/cmd_share.rs`

- [ ] **Step 6.1: Write the failing tests**

Append to the `mod tests` block in `cmd_share.rs`:

```rust
    fn codex_only_bundle(home: &Path) -> HarnessBundle {
        let codex_dir = home.join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let resolver = toolpath_codex::PathResolver::new().with_codex_dir(&codex_dir);
        HarnessBundle {
            codex: Some(toolpath_codex::CodexConvo::with_resolver(resolver)),
            ..Default::default()
        }
    }

    fn write_codex_session(codex_dir: &Path, id: &str, cwd: &str) {
        // Date-bucketed layout: ~/.codex/sessions/YYYY/MM/DD/rollout-*-<id>.jsonl
        let dir = codex_dir.join("sessions/2026/05/07");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("rollout-2026-05-07T00-00-00-{id}.jsonl"));
        let meta = format!(
            r#"{{"timestamp":"2026-05-07T00:00:00Z","type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-05-07T00:00:00Z","cwd":"{cwd}","originator":"codex-tui","cli_version":"test","source":"cli","model_provider":"openai"}}}}"#
        );
        let user = format!(
            r#"{{"timestamp":"2026-05-07T00:00:01Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"hi"}}]}}}}"#
        );
        std::fs::write(file, format!("{meta}\n{user}\n")).unwrap();
    }

    #[test]
    fn gather_sessions_includes_codex_rows_with_cwd_match() {
        let temp = TempDir::new().unwrap();
        write_codex_session(
            &temp.path().join(".codex"),
            "00000000-0000-0000-0000-0000000000aa",
            "/work/proj",
        );
        let bundle = codex_only_bundle(temp.path());
        let rows = gather_sessions(&bundle, Path::new("/work/proj"), None, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].harness, Harness::Codex);
        assert_eq!(rows[0].cwd.as_deref(), Some("/work/proj"));
        assert!(rows[0].matches_cwd);
    }

    #[test]
    fn gather_sessions_ranks_cwd_matches_first() {
        // Two claude sessions: one in cwd (older), one elsewhere (newer).
        // Despite the elsewhere row being newer, the cwd-match must come first.
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        write_claude_session(&claude_dir, "-cwd-project", "in-cwd-session", "hi");
        // Bump activity on the not-in-cwd session by writing a later timestamp.
        let not_dir = claude_dir.join("projects").join("-other-project");
        std::fs::create_dir_all(&not_dir).unwrap();
        std::fs::write(
            not_dir.join("not-in-cwd-session.jsonl"),
            r#"{"type":"user","uuid":"u-x","timestamp":"2030-01-01T00:00:00Z","cwd":"/other/project","message":{"role":"user","content":"later"}}"#.to_string()
                + "\n",
        )
        .unwrap();
        let bundle = claude_only_bundle(temp.path());
        let rows = gather_sessions(&bundle, Path::new("/cwd/project"), None, None);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session_id, "in-cwd-session");
        assert!(rows[0].matches_cwd);
        assert!(!rows[1].matches_cwd);
    }
```

- [ ] **Step 6.2: Run the tests to confirm they fail**

```bash
cargo test -p path-cli --lib cmd_share::tests::gather_sessions_includes_codex
cargo test -p path-cli --lib cmd_share::tests::gather_sessions_ranks
```

Expected: FAIL — codex collection isn't implemented.

- [ ] **Step 6.3: Add `collect_codex` and `collect_opencode` and dispatch them**

Inside `gather_sessions`, add the two extra blocks after the pi block:

```rust
    if want(Harness::Codex) {
        if let Some(mgr) = &bundle.codex {
            collect_codex(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
        }
    }
    if want(Harness::Opencode) {
        if let Some(mgr) = &bundle.opencode {
            collect_opencode(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
        }
    }
```

Add the two new collector functions next to the existing ones:

```rust
fn collect_codex(
    mgr: &toolpath_codex::CodexConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<SessionRow>,
) {
    let metas = match mgr.list_sessions() {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return,
        Err(e) if is_not_found_codex(&e) => return,
        Err(e) => {
            eprintln!("warning: codex aggregation failed: {e}");
            return;
        }
    };
    for m in metas {
        let cwd_str = m.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
        if let Some(filter) = project_filter {
            let stored = match cwd_str.as_deref() {
                Some(s) => std::path::PathBuf::from(s),
                None => continue,
            };
            if !paths_match(&stored, filter) {
                continue;
            }
        }
        let matches_cwd = m
            .cwd
            .as_deref()
            .map(|p| paths_match(p, canonical_cwd))
            .unwrap_or(false);
        out.push(SessionRow {
            harness: Harness::Codex,
            project: None,
            cwd: cwd_str,
            session_id: m.id,
            title: m
                .first_user_message
                .unwrap_or_else(|| "(no prompt)".to_string()),
            last_activity: m.last_activity,
            message_count: m.line_count,
            matches_cwd,
        });
    }
}

fn collect_opencode(
    mgr: &toolpath_opencode::OpencodeConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<SessionRow>,
) {
    let metas = match mgr.io().list_session_metadata(None) {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return,
        Err(e) if is_not_found_opencode(&e) => return,
        Err(e) => {
            eprintln!("warning: opencode aggregation failed: {e}");
            return;
        }
    };
    for m in metas {
        if let Some(filter) = project_filter
            && !paths_match(&m.directory, filter)
        {
            continue;
        }
        let matches_cwd = paths_match(&m.directory, canonical_cwd);
        let cwd_str = m.directory.to_string_lossy().into_owned();
        let title = match (&m.first_user_message, m.title.is_empty()) {
            (Some(s), _) if !s.is_empty() => s.clone(),
            (_, false) => m.title.clone(),
            _ => "(no prompt)".to_string(),
        };
        out.push(SessionRow {
            harness: Harness::Opencode,
            project: None,
            cwd: Some(cwd_str),
            session_id: m.id,
            title,
            last_activity: m.last_activity,
            message_count: m.message_count,
            matches_cwd,
        });
    }
}

fn is_not_found_codex(err: &toolpath_codex::ConvoError) -> bool {
    use toolpath_codex::ConvoError;
    matches!(err, ConvoError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, ConvoError::NoHomeDirectory)
}

fn is_not_found_opencode(err: &toolpath_opencode::ConvoError) -> bool {
    use toolpath_opencode::ConvoError;
    matches!(err, ConvoError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, ConvoError::NoHomeDirectory)
}
```

(Both `is_not_found_codex` and `is_not_found_opencode` use `ConvoError` since both providers re-export that name. Variant names verified against `crates/toolpath-{codex,opencode}/src/error.rs`.)

- [ ] **Step 6.4: Run the tests to verify they pass**

```bash
cargo test -p path-cli --lib cmd_share
cargo clippy -p path-cli -- -D warnings
```

Expected: PASS, clippy clean.

- [ ] **Step 6.5: Commit**

```bash
git add crates/path-cli/src/cmd_share.rs
git commit -m "feat(path-cli): cover codex+opencode in gather_sessions

Adds collect_codex/collect_opencode and the matching ranking/filter
tests. Session-keyed providers compare canonical(stored_cwd) to
canonical(cwd) for matches_cwd; project_filter applies to the same
recorded cwd."
```

---

## Task 7: Implement explicit-args path (skip picker, derive, upload)

This makes `path share --harness X --session Y [--project P] [--anon] ...` end-to-end functional. The picker path lands in task 8.

**Files:**
- Modify: `crates/path-cli/src/cmd_share.rs`
- Modify: `crates/path-cli/tests/integration.rs`

- [ ] **Step 7.1: Write the failing integration test**

Append to `crates/path-cli/tests/integration.rs`:

```rust
#[test]
fn share_explicit_args_uploads_via_anon() {
    use std::io::Write;
    use std::net::TcpListener;

    // Stand up a one-shot mock that returns a valid AnonUploadResponse.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        // Drain the request just enough to keep the OS happy.
        use std::io::Read;
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"abc-123","url":"https://example.test/anon/abc-123"}"#;
        let resp = format!(
            "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    // Build a claude fixture so the explicit-args path has something to derive.
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let claude_dir = temp.path().join(".claude");
    let project_slug = "-".to_string()
        + &project.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "-");
    let project_dir = claude_dir.join("projects").join(&project_slug);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("session-abc.jsonl"),
        format!(
            r#"{{"type":"user","uuid":"u-1","timestamp":"2024-01-01T00:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":"hi"}}}}
{{"type":"assistant","uuid":"a-1","timestamp":"2024-01-01T00:00:01Z","message":{{"role":"assistant","content":"hello"}}}}
"#,
            cwd = project.display()
        ),
    )
    .unwrap();

    let cfg = tempfile::tempdir().unwrap();
    cmd()
        .env("HOME", temp.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args([
            "share",
            "--harness",
            "claude",
            "--session",
            "session-abc",
            "--project",
        ])
        .arg(&project)
        .args(["--anon", "--no-cache", "--url"])
        .arg(format!("http://127.0.0.1:{port}"))
        .assert()
        .success()
        .stdout(predicate::str::contains("https://example.test/anon/abc-123"))
        .stderr(predicate::str::contains("Uploaded"));

    server.join().unwrap();
}
```

- [ ] **Step 7.2: Run the test to confirm it fails**

```bash
cargo test -p path-cli --test integration share_explicit_args_uploads_via_anon
```

Expected: FAIL — `path share` still bails with "not yet implemented".

- [ ] **Step 7.3: Implement the explicit-args path**

In `cmd_share.rs`, replace the stub `pub fn run` with:

```rust
pub fn run(args: ShareArgs) -> Result<()> {
    let harness = args.harness.map(Harness::from_arg);

    if let (Some(h), Some(session)) = (harness, &args.session) {
        return share_explicit(h, session.as_str(), &args);
    }

    if args.session.is_some() && harness.is_none() {
        anyhow::bail!("--session requires --harness");
    }

    // Picker path lands in the next task.
    anyhow::bail!("interactive `path share` is not yet implemented")
}

fn share_explicit(harness: Harness, session: &str, args: &ShareArgs) -> Result<()> {
    let project = match (harness.project_keyed(), args.project.as_ref()) {
        (true, Some(p)) => Some(p.to_string_lossy().into_owned()),
        (true, None) => anyhow::bail!(
            "--project required when --harness is {} and --session is set",
            harness.name()
        ),
        (false, _) => None,
    };

    let derived = derive_one(harness, project.as_deref(), session)?;
    let summary = format!(
        "{} session {}",
        harness.name(),
        derived.cache_id
    );

    if !args.no_cache {
        let path = crate::cmd_cache::write_cached(&derived.cache_id, &derived.doc, args.force)?;
        eprintln!(
            "Imported {} session → {} ({})",
            harness.name(),
            derived.cache_id,
            path.display()
        );
    }

    let body = derived.doc.to_json()?;
    let upload = crate::cmd_export::PathbaseUploadArgs {
        url: args.url.clone(),
        anon: args.anon,
        repo: args.repo.clone(),
        slug: args.slug.clone(),
        public: args.public,
    };
    crate::cmd_export::run_pathbase_inner(upload, &body, &summary)
}

fn derive_one(
    harness: Harness,
    project: Option<&str>,
    session: &str,
) -> Result<crate::cmd_import::DerivedDoc> {
    match harness {
        Harness::Claude => {
            crate::cmd_import::derive_claude_pair(project.expect("project_keyed"), session)
        }
        Harness::Gemini => crate::cmd_import::derive_gemini_pair(
            project.expect("project_keyed"),
            session,
            false,
        ),
        Harness::Pi => {
            crate::cmd_import::derive_pi_pair(project.expect("project_keyed"), session, None)
        }
        Harness::Codex => crate::cmd_import::derive_codex_one(session),
        Harness::Opencode => crate::cmd_import::derive_opencode_one(session, false),
    }
}
```

`RepoSpec` is `Clone`-able via the existing `#[derive(Debug, Clone)]` on the struct in `cmd_export`, so `args.repo.clone()` works.

- [ ] **Step 7.4: Run the test to verify it passes**

```bash
cargo test -p path-cli --test integration share_explicit_args_uploads_via_anon
```

Expected: PASS.

- [ ] **Step 7.5: Add cache-behavior integration tests**

Append to `crates/path-cli/tests/integration.rs`:

```rust
/// Helper for the cache tests. Spawns a one-shot mock anon-upload server
/// on a free port and returns (port, server-thread-handle, fixture-temp,
/// project-path, $HOME-path).
fn share_anon_fixture() -> (u16, std::thread::JoinHandle<()>, tempfile::TempDir, PathBuf, PathBuf)
{
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"abc","url":"https://example.test/anon/abc"}"#;
        let resp = format!(
            "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let claude_dir = temp.path().join(".claude");
    let project_slug = "-".to_string()
        + &project.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "-");
    let project_dir = claude_dir.join("projects").join(&project_slug);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("session-abc.jsonl"),
        format!(
            r#"{{"type":"user","uuid":"u-1","timestamp":"2024-01-01T00:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":"hi"}}}}
{{"type":"assistant","uuid":"a-1","timestamp":"2024-01-01T00:00:01Z","message":{{"role":"assistant","content":"hello"}}}}
"#,
            cwd = project.display()
        ),
    )
    .unwrap();

    let home = temp.path().to_path_buf();
    (port, server, temp, project, home)
}

#[test]
fn share_writes_cache_by_default() {
    let (port, server, _temp, project, home) = share_anon_fixture();
    let cfg = tempfile::tempdir().unwrap();

    cmd()
        .env("HOME", &home)
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args([
            "share",
            "--harness",
            "claude",
            "--session",
            "session-abc",
            "--project",
        ])
        .arg(&project)
        .args(["--anon", "--url"])
        .arg(format!("http://127.0.0.1:{port}"))
        .assert()
        .success();

    let docs = cfg.path().join("documents");
    let entries: Vec<_> = std::fs::read_dir(&docs)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one cache entry, got {entries:?}"
    );
    let name = entries[0].file_name().to_string_lossy().into_owned();
    assert!(
        name.starts_with("claude-"),
        "expected claude-* cache id, got {name}"
    );

    server.join().unwrap();
}

#[test]
fn share_no_cache_skips_write() {
    let (port, server, _temp, project, home) = share_anon_fixture();
    let cfg = tempfile::tempdir().unwrap();

    cmd()
        .env("HOME", &home)
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args([
            "share",
            "--harness",
            "claude",
            "--session",
            "session-abc",
            "--project",
        ])
        .arg(&project)
        .args(["--anon", "--no-cache", "--url"])
        .arg(format!("http://127.0.0.1:{port}"))
        .assert()
        .success();

    let docs = cfg.path().join("documents");
    if docs.exists() {
        let entries: Vec<_> = std::fs::read_dir(&docs)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            entries.is_empty(),
            "expected no cache entries with --no-cache, got {entries:?}"
        );
    }

    server.join().unwrap();
}
```

- [ ] **Step 7.6: Run the new tests**

```bash
cargo test -p path-cli --test integration share_writes_cache_by_default share_no_cache_skips_write
```

Expected: PASS.

- [ ] **Step 7.7: Run the full test suite + clippy**

```bash
cargo test -p path-cli
cargo clippy -p path-cli -- -D warnings
```

Expected: green.

- [ ] **Step 7.8: Commit**

```bash
git add crates/path-cli/src/cmd_share.rs crates/path-cli/tests/integration.rs
git commit -m "feat(path-cli): implement \`path share\` explicit-args path

When --harness and --session are both set, share derives the session
via cmd_import's pair helpers, optionally writes the cache, then
uploads via cmd_export::run_pathbase_inner. Picker path follows."
```

---

## Task 8: Implement the picker, non-TTY recipe, and empty-result probe summary

Adds the unified fzf picker, the recipe message when fzf isn't available, and the probe-summary error when no sessions exist anywhere.

**Files:**
- Modify: `crates/path-cli/src/cmd_share.rs`
- Modify: `crates/path-cli/tests/integration.rs`

- [ ] **Step 8.1: Write the failing tests**

Append to `crates/path-cli/tests/integration.rs`:

```rust
#[test]
fn share_filters_by_project_with_no_matches_errors() {
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let nonexistent = home.path().join("never");

    cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["share", "--project"])
        .arg(&nonexistent)
        .assert()
        .failure()
        .stderr(predicate::str::contains("No agent sessions found in project"));
}
```

Append to `crates/path-cli/src/cmd_share.rs` `mod tests`:

```rust
    #[test]
    fn parse_picker_row_roundtrips_keyed() {
        let row = SessionRow {
            harness: Harness::Claude,
            project: Some("/tmp/proj".to_string()),
            cwd: None,
            session_id: "sess-abc".to_string(),
            title: "Hello\tworld".to_string(),
            last_activity: None,
            message_count: 3,
            matches_cwd: true,
        };
        let line = format_picker_row(&row);
        let (harness, key, session) = parse_picker_row(&line).unwrap();
        assert_eq!(harness, Harness::Claude);
        assert_eq!(key, "/tmp/proj");
        assert_eq!(session, "sess-abc");
    }

    #[test]
    fn parse_picker_row_roundtrips_session_keyed() {
        let row = SessionRow {
            harness: Harness::Codex,
            project: None,
            cwd: Some("/work/proj".to_string()),
            session_id: "0190abcd".to_string(),
            title: "(no prompt)".to_string(),
            last_activity: None,
            message_count: 0,
            matches_cwd: false,
        };
        let line = format_picker_row(&row);
        let (harness, key, session) = parse_picker_row(&line).unwrap();
        assert_eq!(harness, Harness::Codex);
        assert_eq!(key, "/work/proj"); // codex has no project; cwd carried as the keyed slot
        assert_eq!(session, "0190abcd");
    }
```

Append to `crates/path-cli/tests/integration.rs`:

```rust
#[test]
fn share_no_harness_non_tty_prints_recipe() {
    let cfg = tempfile::tempdir().unwrap();
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["share"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("path import"))
        .stderr(predicate::str::contains("path export pathbase"));
}
```

- [ ] **Step 8.2: Run the tests to confirm they fail**

```bash
cargo test -p path-cli --lib cmd_share::tests::parse_picker_row
cargo test -p path-cli --test integration share_no_harness_non_tty_prints_recipe
cargo test -p path-cli --test integration share_filters_by_project_with_no_matches_errors
```

Expected: FAIL — picker functions, non-TTY message, and probe-summary path don't exist.

- [ ] **Step 8.3: Add picker formatting + dispatch**

Append to `cmd_share.rs`:

```rust
/// Build the TSV line fed to fzf. Cols 1–3 are hidden (harness/key/session,
/// used as parser keys); cols 4..8 are visible to the user.
fn format_picker_row(row: &SessionRow) -> String {
    let key = row
        .project
        .clone()
        .or_else(|| row.cwd.clone())
        .unwrap_or_default();
    let when = row
        .last_activity
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "          —     ".to_string());
    let scope = if row.matches_cwd { "·" } else { " " };
    let project_short = project_short(&key);
    let title = fzf_title(&row.title);
    format!(
        "{}\t{}\t{}\t{}\t{}\t{} msgs\t{}\t{}\t{}",
        row.harness.name(),
        tab_safe(&key),
        tab_safe(&row.session_id),
        row.harness.symbol(),
        when,
        row.message_count,
        scope,
        tab_safe(&project_short),
        title,
    )
}

/// Inverse of [`format_picker_row`] — pulls (harness, key, session) back
/// out of the line fzf returned. Returns `None` if the line is malformed.
fn parse_picker_row(line: &str) -> Option<(Harness, String, String)> {
    let mut parts = line.split('\t');
    let h = Harness::parse(parts.next()?)?;
    let key = parts.next()?.to_string();
    let session = parts.next()?.to_string();
    if session.is_empty() {
        return None;
    }
    Some((h, key, session))
}

fn tab_safe(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

fn fzf_title(s: &str) -> String {
    const MAX: usize = 120;
    let safe = tab_safe(s);
    if safe.chars().count() > MAX {
        let head: String = safe.chars().take(MAX - 1).collect();
        format!("{head}…")
    } else {
        safe
    }
}

fn project_short(p: &str) -> String {
    let trimmed = p.trim_end_matches('/');
    let parts: Vec<&str> = trimmed.rsplit('/').take(2).collect();
    if parts.is_empty() {
        return p.to_string();
    }
    let mut out: Vec<&str> = parts.into_iter().collect();
    out.reverse();
    out.join("/")
}
```

- [ ] **Step 8.4: Wire the picker into `run`**

Replace the second `anyhow::bail!` in `pub fn run` with the picker dispatch:

```rust
pub fn run(args: ShareArgs) -> Result<()> {
    let harness = args.harness.map(Harness::from_arg);

    if let (Some(h), Some(session)) = (harness, &args.session) {
        return share_explicit(h, session.as_str(), &args);
    }
    if args.session.is_some() && harness.is_none() {
        anyhow::bail!("--session requires --harness");
    }

    let cwd = std::env::current_dir()?;
    let bundle = HarnessBundle::from_environment();
    let project_filter = args.project.as_deref();
    let rows = gather_sessions(&bundle, &cwd, harness, project_filter);

    if rows.is_empty() {
        return bail_no_sessions(&bundle, project_filter);
    }

    if !crate::fzf::available() {
        eprintln!(
            "Interactive `path share` needs `fzf` on PATH and a TTY.\n\
             \n\
             Manual recipe:\n  \
             path import <harness>      # writes a cache entry, prints its id\n  \
             path export pathbase --input <id>"
        );
        anyhow::bail!("fzf unavailable; run `path import <harness>` then `path export pathbase`");
    }

    let lines: Vec<String> = rows.iter().map(format_picker_row).collect();
    let host = pathbase_host_for_picker(&args);
    let header = format!("share an agent session (Enter = upload to {host})");
    let opts = crate::fzf::PickOptions {
        with_nth: "4..",
        prompt: "share> ",
        preview: Some("path show {1} --project {2} --session {3}"),
        header: Some(&header),
        tiebreak: "index",
        multi: false,
    };
    let selected = crate::fzf::pick(&lines, &opts)?;
    let line = match selected.into_iter().next() {
        Some(l) => l,
        None => return Ok(()), // user cancelled
    };
    let (h, key, session) = parse_picker_row(&line)
        .ok_or_else(|| anyhow::anyhow!("internal: failed to parse picker row"))?;

    let mut explicit = ShareArgs {
        url: args.url.clone(),
        anon: args.anon,
        repo: args.repo.clone(),
        slug: args.slug.clone(),
        public: args.public,
        harness: Some(harness_to_arg(h)),
        session: Some(session.clone()),
        project: if h.project_keyed() {
            Some(PathBuf::from(&key))
        } else {
            None
        },
        force: args.force,
        no_cache: args.no_cache,
    };
    eprintln!(
        "Picked {} session {}",
        h.name(),
        explicit.session.as_deref().unwrap_or("?")
    );
    let session_id = explicit.session.take().unwrap();
    share_explicit(h, &session_id, &explicit)
}

fn harness_to_arg(h: Harness) -> HarnessArg {
    match h {
        Harness::Claude => HarnessArg::Claude,
        Harness::Gemini => HarnessArg::Gemini,
        Harness::Codex => HarnessArg::Codex,
        Harness::Opencode => HarnessArg::Opencode,
        Harness::Pi => HarnessArg::Pi,
    }
}

fn pathbase_host_for_picker(args: &ShareArgs) -> String {
    use crate::cmd_pathbase::resolve_url;
    if let Some(u) = &args.url {
        return resolve_url(Some(u.clone()));
    }
    // Best-effort: if there's a stored session, surface its URL; otherwise fall back to default.
    let path = match crate::cmd_pathbase::credentials_path() {
        Ok(p) => p,
        Err(_) => return resolve_url(None),
    };
    match crate::cmd_pathbase::load_session(&path) {
        Ok(Some(s)) => s.url,
        _ => resolve_url(None),
    }
}

fn bail_no_sessions(bundle: &HarnessBundle, project_filter: Option<&std::path::Path>) -> Result<()> {
    if let Some(p) = project_filter {
        anyhow::bail!(
            "No agent sessions found in project {}. Run without --project to see sessions across all projects.",
            p.display()
        );
    }

    let mut summary = String::from("No agent sessions found.\n");
    summary.push_str(&probe_summary_line("claude", bundle.claude.is_some()));
    summary.push_str(&probe_summary_line("gemini", bundle.gemini.is_some()));
    summary.push_str(&probe_summary_line("codex", bundle.codex.is_some()));
    summary.push_str(&probe_summary_line("opencode", bundle.opencode.is_some()));
    summary.push_str(&probe_summary_line("pi", bundle.pi.is_some()));
    eprint!("{summary}");
    anyhow::bail!("no shareable sessions");
}

fn probe_summary_line(name: &str, present: bool) -> String {
    if present {
        format!("  {name}: 0 sessions\n")
    } else {
        format!("  {name}: not configured\n")
    }
}
```

In `cmd_pathbase.rs`, the `credentials_path` and `load_session` helpers are already `pub(crate)` — no change needed.

`crate::cmd_pathbase` and `crate::cmd_cache` and `crate::cmd_export` and `crate::cmd_import` are all in scope by virtue of being sibling modules under `path_cli::`. Add `use` statements at the top of `cmd_share.rs` if rust-analyzer prefers — the qualified paths above also work.

The `pick` call's `preview` template substitutes col 1 (harness) into the `path show` invocation. `path show` already supports each harness as a subcommand. For codex/opencode the `--project {2}` arg becomes `--project /work/proj` even though those subcommands don't accept `--project`; if a future version of `path show` errors on that, swap to per-harness preview templates. Today they accept `--session` regardless, and unknown args print to stderr (preview pane) without aborting the picker.

If `path show codex --project foo --session bar` errors, drop the `--project` from the preview template entirely; the design allows that simplification.

- [ ] **Step 8.5: Run the tests to verify they pass**

```bash
cargo test -p path-cli --lib cmd_share
cargo test -p path-cli --test integration share_no_harness_non_tty_prints_recipe
cargo test -p path-cli --test integration share_explicit_args_uploads_via_anon
cargo clippy -p path-cli -- -D warnings
```

Expected: all green.

- [ ] **Step 8.6: Manual smoke test of the picker (locally only — not CI)**

```bash
cargo build -p path-cli
./target/debug/path share --url http://127.0.0.1:1
```

Expected on a machine with installed harnesses and fzf: an fzf list opens; cwd-matching sessions appear at the top; selecting one fails the upload (port 1) but proves the picker → derive → upload wiring. Press Esc to cancel — exit code should be 0 with nothing on stdout.

- [ ] **Step 8.7: Commit**

```bash
git add crates/path-cli/src/cmd_share.rs crates/path-cli/tests/integration.rs
git commit -m "feat(path-cli): wire the unified \`path share\` picker

Aggregates SessionRow values across installed harnesses, ranks
cwd-matches first, and pipes them through fzf. Falls back to a
manual-recipe message when fzf isn't available, and prints a probe
summary when no harness has any sessions to share."
```

---

## Task 9: Documentation — `CLAUDE.md`

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 9.1: Add a `path share` line to the CLI usage block**

In `CLAUDE.md`, under the `## CLI usage` section, after the `path import` group of examples and before the `# Export toolpath documents…` block, insert:

```markdown
# Share an agent session to Pathbase (interactive picker, single-shot)
cargo run -p path-cli -- share
cargo run -p path-cli -- share --harness claude --session <session-id> --project /path/to/project
cargo run -p path-cli -- share --url https://my-pathbase.example
```

- [ ] **Step 9.2: Add a "Things to know" entry**

In the `## Things to know` bullet list, append:

```markdown
- `path share` is the one-shot equivalent of `path import <harness> | path export pathbase`. It probes installed agent harnesses (claude/gemini/codex/opencode/pi), aggregates their sessions into a single fzf picker, and ranks rows whose project (claude/gemini/pi) or recorded cwd (codex/opencode) canonicalizes to the current directory at the top. `--harness` narrows the picker to one provider; `--harness X --session Y` (and `--project P` for keyed providers) skips the picker entirely. Pathbase flags (`--url`, `--anon`, `--repo`, `--slug`, `--public`) match `path export pathbase`. By default the derived doc is written to the cache like `import` does; pass `--no-cache` to skip.
```

- [ ] **Step 9.3: Build the workspace once more as a sanity check**

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Expected: clean.

- [ ] **Step 9.4: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document \`path share\` in CLAUDE.md"
```

---

## Done criteria

- `path share --help` lists all flags from the design.
- `path share --harness X --session Y [--project P]` derives + uploads in one shot, with the share URL on stdout.
- `path share` (no flags, fzf available) opens a unified picker with cwd-matching rows ranked first.
- `path share` (no flags, no fzf) prints the manual recipe and exits 1.
- `path share --project P` filters to that project; if no rows match, exits 1 with a focused error message.
- All existing tests still pass; `cargo clippy --workspace -- -D warnings` is clean.
- The `CLAUDE.md` CLI block and Things-to-know list reflect the new command.
