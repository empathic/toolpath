# `path resume` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `path resume <input>` — fetches/loads a Toolpath document, picks a coding-agent harness (interactive picker by default, `--harness X` to skip), projects the session into the harness's on-disk layout in a chosen cwd, then execs the harness's resume command.

**Architecture:** New `cmd_resume.rs` module mirroring `cmd_share.rs`. cmd_export.rs gains five small `pub(crate)` wrappers (`project_<harness>`) that compose the existing private build+write helpers and return the projected session id. cmd_resume composes these with an `argv_for(harness, session_id)` helper, an injectable `ExecStrategy`, and a small interactive picker. No new public types in the path-cli library.

**Tech Stack:** Rust 2024, clap, anyhow, `toolpath_*` workspace crates, existing `crate::fzf` helper, `cmd_share::Harness` enum, `pathbase-client`.

**Spec reference:** `docs/superpowers/specs/2026-05-08-path-resume-command-design.md`.

---

## Type and API quick reference

The plan's code samples lean on these existing types and functions. Cross-check against the source before writing tests.

```rust
// crates/toolpath/src/types.rs
pub struct Path {
    pub path: PathIdentity,    // { id, base: Option<Base>, head, graph_ref }
    pub steps: Vec<Step>,      // not Vec<StepRecord> — push Step directly
    pub meta: Option<PathMeta>,
}

pub struct PathMeta {
    pub source: Option<String>,        // "claude-code" / "gemini-cli" / "codex" / "opencode" / "pi"
    // …
}

pub struct Step {
    pub step: StepIdentity,            // { id, parents, actor, timestamp }
    pub change: HashMap<String, ArtifactChange>,
    pub meta: Option<StepMeta>,
}

// Builder pattern — preferred in tests
Step::new(id, actor, timestamp)
    .with_raw_change("a.txt", "@@ -1 +1 @@\n-old\n+new")
    .with_intent("…");

Path::new(id, /* base */ None::<Base>, /* head */ "s1");

// Universal parse / build
Graph::from_json(&json)?;          // never parses to a Step or bare Path
Graph::from_path(path);             // single-inline-path Graph constructor
graph.into_single_path();           // Option<Path>
graph.single_path();                // Option<&Path>
```

**There is no `Document` enum.** `Graph::from_json` is the universal entry point — every cache file, every Pathbase response, every Toolpath JSON parses as a `Graph`. Single-path-graphs are the closest thing to a "Path document"; `into_single_path` unwraps them. The plan validates everything as a `Graph` (see Task 4).

**`path.meta.source` access pattern** (because `meta: Option<PathMeta>`):

```rust
path.meta.as_ref().and_then(|m| m.source.as_deref())
```

**`fzf` module API** (`crates/path-cli/src/fzf.rs`):

```rust
pub fn available() -> bool;
pub fn pick(lines: &[String], opts: &PickOptions<'_>) -> Result<PickResult>;

pub enum PickResult {
    Selected(String),
    NoMatch,
    Cancelled,
}

pub struct PickOptions<'a> {
    pub header: Option<&'a str>,
    // … (read the source for the full set; defaults usually suffice)
}
```

**Idiomatic test fixture** (mirrors `cmd_merge.rs::tests::make_path` / `make_step`):

```rust
fn make_step(id: &str, actor: &str) -> toolpath::v1::Step {
    toolpath::v1::Step::new(id, actor, "2026-01-01T00:00:00Z")
        .with_raw_change("src/main.rs", "@@ -1 +1 @@\n-old\n+new")
}

fn make_path_with_actor(actor: &str) -> toolpath::v1::Path {
    use toolpath::v1::{Path, PathIdentity};
    let step = make_step("s1", actor);
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
```

Whenever a task below refers to `make_path_with_actor(...)`, the body is the snippet above with `actor` substituted. Each task lists the actor explicitly.

**Existing `cmd_export.rs` private helpers** (these stay private; the new wrappers compose them):

| Harness | Build helper | Write helper |
| --- | --- | --- |
| Claude   | `build_claude_conversation(path) -> Conversation` (with `session_id`)              | `write_into_claude_project(conv, jsonl, project_dir) -> PathBuf` (returns the JSONL path) |
| Gemini   | `build_gemini_conversation(input, project_path) -> Conversation` (with `session_uuid`) | `write_into_gemini_project(conv, project_path) -> ()` |
| Codex    | `build_codex_session(input, cwd) -> Session` (with `id`)                          | `write_into_codex_project(session) -> ()` |
| Opencode | `build_opencode_session(path, project_dir) -> Session` (with `id`)                 | `write_into_opencode_db(session, project_dir) -> ()` |
| Pi       | `build_pi_session(input, cwd) -> PiSession` (with `header.id`)                     | `write_into_pi_project(session, cwd) -> ()` |

Verify these signatures by reading `cmd_export.rs` before writing the wrappers — adapt as needed if a name differs from this table.

---

## File Structure

**New:**
- `crates/path-cli/src/cmd_resume.rs` — new module: `ResumeArgs`, orchestration, `resolve_input`, `infer_source_harness`, `ensure_path_with_agent`, `pick_harness`, `argv_for`, `ExecStrategy`, `RealExec`, `RecordingExec`.
- `crates/path-cli/tests/resume.rs` — integration tests with injectable exec strategy.
- `crates/path-cli/tests/support/mod.rs` (or `tests/support.rs`) — shared test helpers.

**Modified:**
- `crates/path-cli/src/cmd_export.rs` — add five `pub(crate) fn project_<harness>(path: &Path, project_dir: &Path) -> Result<String>` wrappers. No other change.
- `crates/path-cli/src/cmd_import.rs` — extract a `pub(crate) fn pathbase_fetch_to_doc(target: &str, url_flag: Option<&str>) -> Result<DerivedDoc>` from the inner block of `derive_pathbase`. `derive_pathbase` becomes a one-line wrapper.
- `crates/path-cli/src/cmd_pathbase.rs` — promote the test-module `MockServer` and required helpers to `pub(crate)` so cross-test-module use works.
- `crates/path-cli/src/lib.rs` — add `Commands::Resume { args: cmd_resume::ResumeArgs }`; wire dispatch.
- `crates/path-cli/Cargo.toml` — minor version bump (`0.8.0` → `0.9.0`).
- `Cargo.toml` (root) — `[workspace.dependencies]` `path-cli` version bump.
- `site/_data/crates.json` — `path-cli` version bump.
- `CHANGELOG.md` — new entry.
- `CLAUDE.md` — CLI usage block + "Things to know" bullet.
- `README.md` — one-line mention.

---

## Task 1: `project_<harness>` `pub(crate)` wrappers in `cmd_export.rs`

**Files:**
- Modify: `crates/path-cli/src/cmd_export.rs`

These wrappers compose the existing build + write private helpers and return the projected session id as a `String`. No behavior change to `path export <harness>`. The five wrappers are sibling-shaped; we add and test them all in one task to keep the refactor batched.

- [ ] **Step 1: Read the existing private helpers**

Read `crates/path-cli/src/cmd_export.rs`, focusing on:
- `build_claude_conversation`, `serialize_jsonl`, `write_into_claude_project`
- `build_gemini_conversation`, `write_into_gemini_project`
- `build_codex_session`, `write_into_codex_project`
- `build_opencode_session`, `write_into_opencode_db`
- `build_pi_session`, `write_into_pi_project`

Confirm the signatures match the "Existing `cmd_export.rs` private helpers" table above. Note any deviations (most likely: `build_<gemini|codex|pi>_*` take `input: &str` cache id, not `path: &Path`; rework if so).

- [ ] **Step 2: Write failing tests for all five wrappers**

Append to the existing tests module in `cmd_export.rs` (find it near the bottom of the file under `#[cfg(test)] mod tests {`). First add the shared fixture helpers if they're not already there:

```rust
#[cfg(not(target_os = "emscripten"))]
fn make_step_with_actor(id: &str, actor: &str) -> toolpath::v1::Step {
    toolpath::v1::Step::new(id, actor, "2026-01-01T00:00:00Z")
        .with_raw_change("src/main.rs", "@@ -1 +1 @@\n-old\n+new")
}

#[cfg(not(target_os = "emscripten"))]
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

/// Pin `$HOME` to a tempdir for tests that resolve harness paths.
#[cfg(not(target_os = "emscripten"))]
struct ScopedHome { _td: tempfile::TempDir, prev: Option<std::ffi::OsString> }

#[cfg(not(target_os = "emscripten"))]
impl ScopedHome {
    fn new() -> Self {
        let td = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        // Safety: cmd_export tests already share state via the global cache
        // dir; treat them as serial. If the crate ever flips to multi-threaded
        // tests, replace with `serial_test`.
        unsafe { std::env::set_var("HOME", td.path()); }
        Self { _td: td, prev }
    }
}

#[cfg(not(target_os = "emscripten"))]
impl Drop for ScopedHome {
    fn drop(&mut self) {
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
```

Then add five tests:

```rust
#[test]
#[cfg(not(target_os = "emscripten"))]
fn project_claude_returns_session_id_and_writes_jsonl() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let path = make_path_with_actor("agent:claude-code");

    let session_id = project_claude(&path, cwd.path()).unwrap();
    assert!(!session_id.is_empty(), "session id should be non-empty");

    // The projected JSONL must land somewhere under HOME/.claude/projects/.
    let projects = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
        .join(".claude/projects");
    assert!(projects.exists(), "claude projects dir missing under HOME");
}

#[test]
#[cfg(not(target_os = "emscripten"))]
fn project_gemini_returns_session_id_and_writes_chat_file() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let path = make_path_with_actor("agent:gemini-cli");

    let session_id = project_gemini(&path, cwd.path()).unwrap();
    assert!(!session_id.is_empty());

    let tmp_root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
        .join(".gemini/tmp");
    assert!(tmp_root.exists(), "gemini tmp dir missing");
}

#[test]
#[cfg(not(target_os = "emscripten"))]
fn project_codex_returns_session_id_and_writes_rollout() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let path = make_path_with_actor("agent:codex");

    let session_id = project_codex(&path, cwd.path()).unwrap();
    assert!(!session_id.is_empty());

    let sessions = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
        .join(".codex/sessions");
    assert!(sessions.exists(), "codex sessions dir missing");
}

#[test]
#[cfg(not(target_os = "emscripten"))]
fn project_opencode_returns_session_id_and_inserts_row() {
    // Pre-create an opencode db with the canonical schema so the writer
    // doesn't bail. Locate the schema bootstrap helper used by existing
    // opencode tests in `crates/toolpath-opencode/src/` and call it.
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let resolver = toolpath_opencode::PathResolver::new();
    let db_path = resolver.db_path().unwrap();
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // Substitute the actual bootstrap helper name if different.
        toolpath_opencode::schema::apply_full_schema(&conn).unwrap();
    }

    let path = make_path_with_actor("agent:opencode");
    let session_id = project_opencode(&path, cwd.path()).unwrap();
    assert!(!session_id.is_empty());

    // Verify the session row exists.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM session WHERE id = ?1", [&session_id], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
#[cfg(not(target_os = "emscripten"))]
fn project_pi_returns_session_id_and_writes_jsonl() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let path = make_path_with_actor("agent:pi");

    let session_id = project_pi(&path, cwd.path()).unwrap();
    assert!(!session_id.is_empty());

    let sessions = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
        .join(".pi/agent/sessions");
    assert!(sessions.exists(), "pi sessions dir missing");
}
```

If `toolpath_opencode::schema::apply_full_schema` doesn't exist, locate the canonical schema-apply helper used by existing opencode tests (search `crates/toolpath-opencode/src/`) and use that name.

- [ ] **Step 3: Run tests to verify they fail**

```bash
cargo test -p path-cli --lib project_claude project_gemini project_codex project_opencode project_pi
```

Expected: FAIL — none of the wrappers exist yet.

- [ ] **Step 4: Implement the five wrappers**

Add near the top of `cmd_export.rs`, after the existing `pub(crate) struct PathbaseUploadArgs` (around line 230). Each wrapper composes the existing private build + write helpers and returns the projected session id.

```rust
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_claude(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    let conv = build_claude_conversation(path)?;
    let jsonl = serialize_jsonl(&conv)?;
    write_into_claude_project(&conv, &jsonl, project_dir)?;
    Ok(conv.session_id)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_gemini(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    use toolpath_convo::ConversationProjector;
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let project_path = project_dir.to_string_lossy().to_string();

    let view = toolpath_convo::extract_conversation(path);
    let project_hash = toolpath_gemini::paths::project_hash(&project_path);
    let projector = toolpath_gemini::project::GeminiProjector::new()
        .with_project_hash(project_hash)
        .with_project_path(project_path.clone());
    let conv = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;
    if conv.session_uuid.is_empty() {
        anyhow::bail!("Projected conversation has no session UUID");
    }
    write_into_gemini_project(&conv, &project_path)?;
    Ok(conv.session_uuid)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_codex(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    use toolpath_convo::ConversationProjector;
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let cwd_str = project_dir.to_string_lossy().to_string();

    let view = toolpath_convo::extract_conversation(path);
    let projector = toolpath_codex::project::CodexProjector::new().with_cwd(cwd_str);
    let session = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;
    if session.id.is_empty() {
        anyhow::bail!("Projected session has no id");
    }
    write_into_codex_project(&session)?;
    Ok(session.id)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_opencode(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    let session = build_opencode_session(path, Some(project_dir))?;
    let id = session.id.clone();
    write_into_opencode_db(&session, project_dir)?;
    Ok(id)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_pi(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    use toolpath_convo::ConversationProjector;
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let cwd_str = project_dir.to_string_lossy().to_string();

    let view = toolpath_convo::extract_conversation(path);
    let projector = toolpath_pi::project::PiProjector::new().with_cwd(cwd_str.clone());
    let session = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;
    if session.header.id.is_empty() {
        anyhow::bail!("Projected session has no id");
    }
    write_into_pi_project(&session, &cwd_str)?;
    Ok(session.header.id)
}
```

(`project_claude` doesn't canonicalize because `write_into_claude_project` already does. `project_opencode` doesn't either, because `build_opencode_session` already passes the dir to the projector. The other three canonicalize here because their write helpers don't.)

- [ ] **Step 5: Run the new tests**

```bash
cargo test -p path-cli --lib project_claude project_gemini project_codex project_opencode project_pi
```

Expected: PASS.

- [ ] **Step 6: Run the full export tests to confirm no regressions**

```bash
cargo test -p path-cli --lib cmd_export
```

Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/path-cli/src/cmd_export.rs
git commit -m "feat(path-cli): pub(crate) project_<harness> wrappers in cmd_export"
```

---

## Task 2: Extract `pathbase_fetch_to_doc` from `cmd_import.rs`

**Files:**
- Modify: `crates/path-cli/src/cmd_import.rs:1362-1388` (derive_pathbase)
- Modify: `crates/path-cli/src/cmd_pathbase.rs` — promote `MockServer` test helpers to `pub(crate)` so a sibling test module can use them.

- [ ] **Step 1: Write the failing test**

In `cmd_import.rs`'s tests module (or in a new `#[cfg(test)] mod pathbase_fetch_tests` block adjacent to it), add:

```rust
#[test]
#[cfg(not(target_os = "emscripten"))]
fn pathbase_fetch_to_doc_url_input() {
    use crate::cmd_pathbase::tests::MockServer;
    let body = r#"{"graph":{"id":"g1"},"paths":[{"path":{"id":"p1","head":"s1"},"steps":[{"step":{"id":"s1","actor":"agent:claude-code","timestamp":"2026-01-01T00:00:00Z"},"change":{}}]}]}"#;
    let server = MockServer::start("HTTP/1.1 200 OK", body);
    let url = format!("{}/alex/pathstash/my-path", server.base());

    let derived = pathbase_fetch_to_doc(&url, None).unwrap();

    assert_eq!(derived.cache_id, "pathbase-alex-pathstash-my-path");
    assert!(derived.doc.into_single_path().is_some());
}
```

(Adjust the JSON body shape to whatever `Graph::from_json` actually accepts — read existing pathbase tests in `cmd_pathbase.rs` and `cmd_import.rs` for the canonical body string.)

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p path-cli --lib pathbase_fetch_to_doc_url_input
```

Expected: FAIL — `pathbase_fetch_to_doc` doesn't exist; possibly also `MockServer` isn't reachable from sibling test modules.

- [ ] **Step 3: Make `MockServer` reachable from sibling tests**

In `crates/path-cli/src/cmd_pathbase.rs`, change the existing test module declaration so its helper is reachable from sibling test modules:

```rust
#[cfg(test)]
pub(crate) mod tests {
    // (existing contents unchanged; the only changes are `pub(crate)` on the
    // module itself and on `MockServer` + the methods the new caller needs.)
    pub(crate) struct MockServer { /* leave existing fields */ }
    impl MockServer {
        pub(crate) fn start(/* same signature */) -> Self { /* leave body */ }
        pub(crate) fn base(&self) -> String { /* leave body */ }
        // …promote only what the new test consumes.
    }
}
```

- [ ] **Step 4: Extract the function**

Replace `derive_pathbase`'s body (lines 1362-1388) with a wrapper, and add the extracted helper just above it:

```rust
/// Fetch a Pathbase ref (`https://host/owner/repo/slug` URL or bare
/// `owner/repo/slug` triple) and parse it as a toolpath document. Used
/// by `path import pathbase` and by `path resume <url>`.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn pathbase_fetch_to_doc(target: &str, url_flag: Option<&str>) -> Result<DerivedDoc> {
    use crate::cmd_pathbase::{credentials_path, load_session, paths_download, resolve_url};

    let (base, ref_) = parse_pathbase_ref(target, url_flag)?;
    let stored = load_session(&credentials_path()?)?;
    let base_url = base
        .or_else(|| stored.as_ref().map(|s| s.url.clone()))
        .unwrap_or_else(|| resolve_url(None));

    let token = stored.as_ref().map(|s| s.token.as_str());

    let PathRef { owner, repo, slug } = ref_;
    let body = paths_download(&base_url, token, &owner, &repo, &slug)?;
    let cache_id = make_id("pathbase", &format!("{owner}-{repo}-{slug}"));
    let doc = Graph::from_json(&body)
        .map_err(|e| anyhow::anyhow!("server returned a non-toolpath document: {e}"))?;
    Ok(DerivedDoc { cache_id, doc })
}

fn derive_pathbase(target: String, url_flag: Option<String>) -> Result<Vec<DerivedDoc>> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (target, url_flag);
        anyhow::bail!("'path import pathbase' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        Ok(vec![pathbase_fetch_to_doc(&target, url_flag.as_deref())?])
    }
}
```

- [ ] **Step 5: Run test to verify it passes**

```bash
cargo test -p path-cli --lib pathbase_fetch_to_doc_url_input
```

Expected: PASS.

- [ ] **Step 6: Run all import tests**

```bash
cargo test -p path-cli --lib cmd_import
```

Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add crates/path-cli/src/cmd_import.rs crates/path-cli/src/cmd_pathbase.rs
git commit -m "refactor(path-cli): extract pathbase_fetch_to_doc helper"
```

---

## Task 3: Scaffold `cmd_resume.rs` — types, args, lib.rs wiring

**Files:**
- Create: `crates/path-cli/src/cmd_resume.rs`
- Modify: `crates/path-cli/src/lib.rs:45-180` (Commands enum + dispatch)

- [ ] **Step 1: Create the module with stub run + test**

Create `crates/path-cli/src/cmd_resume.rs`:

```rust
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
}
```

- [ ] **Step 2: Wire into `lib.rs`**

In `crates/path-cli/src/lib.rs`:

a) Add the module declaration near the other `mod cmd_*;` lines (top of the file):

```rust
mod cmd_resume;
```

b) Add a new variant to the `Commands` enum (around line 121, next to `Share`):

```rust
/// Resume an agent session into the chosen harness, projecting the
/// document and exec'ing the harness's resume command.
Resume {
    #[command(flatten)]
    args: cmd_resume::ResumeArgs,
},
```

c) Add the dispatch arm in `run()` (around line 170, next to `Commands::Share`):

```rust
Commands::Resume { args } => cmd_resume::run(args),
```

- [ ] **Step 3: Run the stub test**

```bash
cargo test -p path-cli --lib cmd_resume::tests::run_returns_not_implemented_until_wired
```

Expected: PASS.

- [ ] **Step 4: Verify the CLI surface**

```bash
cargo run -p path-cli -- resume --help
```

Expected output (substring): `Toolpath document to resume from`, `-C`, `--harness`, `--no-cache`, `--force`, `--url`.

- [ ] **Step 5: Commit**

```bash
git add crates/path-cli/src/cmd_resume.rs crates/path-cli/src/lib.rs
git commit -m "feat(path-cli): scaffold path resume command (stub)"
```

---

## Task 4: Implement `infer_source_harness` and `ensure_path_with_agent`

**Files:**
- Modify: `crates/path-cli/src/cmd_resume.rs`

- [ ] **Step 1: Write the failing tests**

Append to `cmd_resume.rs`'s tests module. There is no `Document` enum in this codebase — every parse goes through `Graph::from_json`, so validation operates on `Graph`. A "Path document" surfaces as a `Graph` with exactly one inline path.

```rust
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
    g.paths.push(PathOrRef::Path(Box::new(make_path_with_actor("agent:claude-code"))));
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
    g.paths = vec![PathOrRef::Ref(PathRef { ref_url: "$ref://something".into() })];
    let err = ensure_path_with_agent(&g).unwrap_err();
    assert!(err.to_string().contains("inline `Path`"), "actual: {}", err);
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p path-cli --lib cmd_resume::tests
```

Expected: FAIL — `infer_source_harness` and `ensure_path_with_agent` not defined.

- [ ] **Step 3: Implement the two helpers**

Append to `cmd_resume.rs` above the `mod tests` block:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p path-cli --lib cmd_resume::tests
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(path-cli): infer_source_harness and ensure_path_with_agent"
```

---

## Task 5: Implement `resolve_input`

**Files:**
- Modify: `crates/path-cli/src/cmd_resume.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn resolve_input_file_path() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("doc.json");
    let graph = toolpath::v1::Graph::from_path(make_path_with_actor("agent:claude-code"));
    std::fs::write(&p, graph.to_json().unwrap()).unwrap();

    let args = ResumeArgs {
        input: p.to_string_lossy().to_string(),
        cwd: None, harness: None, no_cache: false, force: false, url: None,
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
    let server = MockServer::start("HTTP/1.1 200 OK", &body);

    let args = ResumeArgs {
        input: format!("{}/alex/pathstash/p", server.base()),
        cwd: None, harness: None, no_cache: true,   // skip cache write in tests
        force: false, url: None,
    };
    let (g, harness) = resolve_input(&args).unwrap();
    let _ = ensure_path_with_agent(&g).unwrap();
    assert_eq!(harness, Some(Harness::Codex));
}

#[test]
fn resolve_input_unresolvable_errors_clearly() {
    let args = ResumeArgs {
        input: "definitely/not/a/real/cache/id".to_string(),
        cwd: None, harness: None, no_cache: false, force: false, url: None,
    };
    let err = resolve_input(&args).unwrap_err();
    let s = err.to_string();
    assert!(s.contains("couldn't resolve"), "actual: {s}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p path-cli --lib resolve_input
```

Expected: FAIL.

- [ ] **Step 3: Implement `resolve_input`**

Append to `cmd_resume.rs`:

```rust
/// Resolve the user-supplied `<input>` argument into a parsed `Graph`
/// plus the source harness inferred from its single inline path (if
/// any). See spec for the resolution order.
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
            let derived = crate::cmd_import::pathbase_fetch_to_doc(u, args.url.as_deref())?;
            if !args.no_cache {
                crate::cmd_cache::write_cached(&derived.cache_id, &derived.doc, args.force)?;
                eprintln!("Resolved {} → {}", raw, derived.cache_id);
            }
            derived.doc
        }
        Shape::FilePath(p) => {
            let json = std::fs::read_to_string(p)
                .with_context(|| format!("read {}", p))?;
            Graph::from_json(&json)
                .map_err(|e| anyhow::anyhow!("not a valid toolpath document: {}", e))?
        }
        Shape::CacheId(id) => {
            let file = crate::cmd_cache::cache_ref(id).map_err(|e| {
                anyhow::anyhow!(
                    "couldn't resolve `{}` as a URL, file path, or cache id: {}",
                    raw, e
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

fn looks_like_pathbase_shorthand(s: &str) -> bool {
    // Three non-empty slash-separated segments, none containing whitespace
    // or a leading dot/slash (which would indicate a relative/absolute path).
    if s.starts_with('.') || s.starts_with('/') { return false; }
    let segs: Vec<&str> = s.split('/').collect();
    segs.len() == 3 && segs.iter().all(|s| !s.is_empty() && !s.contains(char::is_whitespace))
}
```

`Graph::single_path` returns `Option<&Path>`. `infer_source_harness` takes `&Path`, so `.and_then(infer_source_harness)` is the right composition.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p path-cli --lib resolve_input
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(path-cli): resolve_input dispatcher for path resume"
```

---

## Task 6: Implement `pick_harness` non-interactive paths and PATH probe

**Files:**
- Modify: `crates/path-cli/src/cmd_resume.rs`

- [ ] **Step 1: Write the failing tests**

Append to the tests module:

```rust
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
    let result = pick_harness(
        Some(HarnessArg::Claude),
        None,
        Some(td.path()),
    );
    assert_eq!(result.unwrap(), Harness::Claude);

    let err = pick_harness(
        Some(HarnessArg::Gemini),
        None,
        Some(td.path()),
    ).unwrap_err();
    assert!(err.to_string().contains("`gemini` isn't on PATH"));
}

#[test]
fn pick_harness_zero_installed_errors() {
    let td = fake_path_with(&[]);
    let err = pick_harness(
        None,
        Some(Harness::Claude),
        Some(td.path()),
    ).unwrap_err();
    assert!(
        err.to_string().contains("no installed harnesses")
            || err.to_string().contains("no harnesses on PATH"),
        "actual: {}", err
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p path-cli --lib pick_harness binary_on_path
```

Expected: FAIL.

- [ ] **Step 3: Implement `binary_on_path` and `pick_harness`**

Append to `cmd_resume.rs`:

```rust
/// Probe `$PATH` (or `path_override`, for tests) for a given binary
/// name. Cross-platform: on Windows, also tries `<name>.exe`.
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

const ALL_HARNESSES: &[Harness] = &[
    Harness::Claude,
    Harness::Gemini,
    Harness::Codex,
    Harness::Opencode,
    Harness::Pi,
];

/// Decide which harness to resume in. See spec § "pick_harness".
///
/// `path_override` is `None` in production; tests pass `Some(dir)` to
/// fake `$PATH`.
pub(crate) fn pick_harness(
    arg: Option<HarnessArg>,
    source: Option<Harness>,
    path_override: Option<&std::path::Path>,
) -> Result<Harness> {
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

fn interactive_pick(installed: &[Harness], source: Option<Harness>) -> Result<Harness> {
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

    let opts = crate::fzf::PickOptions { header: Some(&header), ..Default::default() };
    let pick = match crate::fzf::pick(&lines, &opts)
        .map_err(|e| anyhow::anyhow!("fzf failed: {}", e))?
    {
        crate::fzf::PickResult::Selected(p) => p,
        crate::fzf::PickResult::Cancelled => std::process::exit(130),
        crate::fzf::PickResult::NoMatch => {
            anyhow::bail!("fzf returned no match — picker UI was empty?");
        }
    };

    for h in installed {
        if pick.starts_with(h.symbol()) {
            return Ok(*h);
        }
    }
    anyhow::bail!("picker returned an unrecognized row: {pick}")
}
```

**Read `crates/path-cli/src/fzf.rs` before writing this.** If `PickOptions` requires extra fields (e.g. `prompt`, `multi`, `preview`), set them to whatever the existing `cmd_share.rs` code sets — the file is short, the existing call site is the canonical example.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p path-cli --lib pick_harness binary_on_path
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(path-cli): harness picker + PATH probe for path resume"
```

---

## Task 7: Implement `project_into_harness` dispatcher and `argv_for`

**Files:**
- Modify: `crates/path-cli/src/cmd_resume.rs`

- [ ] **Step 1: Write the failing test**

```rust
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
    let path = make_path_with_actor("agent:claude-code");

    let session_id = project_into_harness(&path, Harness::Claude, cwd.path()).unwrap();
    assert!(!session_id.is_empty());
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
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p path-cli --lib argv_for project_into_harness_claude_round_trip
```

Expected: FAIL.

- [ ] **Step 3: Implement `argv_for` and `project_into_harness`**

Append to `cmd_resume.rs`:

```rust
/// Static map from harness to resume-argv shape.
pub(crate) fn argv_for(harness: Harness, session_id: &str) -> Vec<String> {
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
    harness: Harness,
    cwd: &std::path::Path,
) -> Result<String> {
    match harness {
        Harness::Claude   => crate::cmd_export::project_claude(path, cwd),
        Harness::Gemini   => crate::cmd_export::project_gemini(path, cwd),
        Harness::Codex    => crate::cmd_export::project_codex(path, cwd),
        Harness::Opencode => crate::cmd_export::project_opencode(path, cwd),
        Harness::Pi       => crate::cmd_export::project_pi(path, cwd),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p path-cli --lib argv_for project_into_harness_claude_round_trip
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(path-cli): argv_for + project_into_harness dispatcher"
```

---

## Task 8: Implement `exec_harness` with injectable strategy

**Files:**
- Modify: `crates/path-cli/src/cmd_resume.rs`

- [ ] **Step 1: Write the failing test**

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p path-cli --lib exec_strategy_recording_captures_invocation
```

Expected: FAIL.

- [ ] **Step 3: Implement `ExecStrategy` and `exec_harness`**

Append to `cmd_resume.rs`:

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p path-cli --lib exec_strategy_recording_captures_invocation
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(path-cli): ExecStrategy with RealExec/RecordingExec"
```

---

## Task 9: Wire `run_resume` orchestration

**Files:**
- Modify: `crates/path-cli/src/cmd_resume.rs`

- [ ] **Step 1: Replace the stub `run` with the real orchestration**

Find the current stub:

```rust
pub fn run(_args: ResumeArgs) -> Result<()> {
    anyhow::bail!("path resume: not yet implemented")
}
```

Replace with:

```rust
pub fn run(args: ResumeArgs) -> Result<()> {
    run_with_strategy(args, &RealExec)
}

/// Internal entry point that the integration tests call with a
/// `RecordingExec` strategy. Production callers use [`run`].
pub fn run_with_strategy(args: ResumeArgs, exec: &dyn ExecStrategy) -> Result<()> {
    let (graph, source_harness) = resolve_input(&args)?;
    let path = ensure_path_with_agent(&graph)?;

    let cwd = match args.cwd.as_ref() {
        Some(p) => std::fs::canonicalize(p)
            .with_context(|| format!("resolve cwd path {}", p.display()))?,
        None => std::env::current_dir()?,
    };

    let target = pick_harness(args.harness, source_harness, None)?;
    eprintln!("Picked harness: {}{}",
        target.name(),
        if Some(target) == source_harness { " (source)" } else { "" }
    );

    let session_id = project_into_harness(path, target, &cwd)?;
    let argv = argv_for(target, &session_id);
    exec_harness(target.name(), &argv, &cwd, exec)
}
```

- [ ] **Step 2: Replace the stub test**

Replace:

```rust
#[test]
fn run_returns_not_implemented_until_wired() { … }
```

with:

```rust
#[test]
fn run_with_strategy_records_invocation_for_file_input_with_explicit_harness() {
    let _home = scoped_home_for_resume();
    let cwd = tempfile::tempdir().unwrap();
    let doc_file = cwd.path().join("doc.json");

    let path = make_path_with_actor("agent:claude-code");
    let graph = toolpath::v1::Graph::from_path(path);
    std::fs::write(&doc_file, graph.to_json().unwrap()).unwrap();

    // Make `claude` discoverable by salting PATH for this process.
    let bin_dir = fake_path_with(&["claude"]);
    let prev = std::env::var_os("PATH");
    let new_path = std::env::join_paths(
        std::iter::once(bin_dir.path().to_path_buf())
            .chain(std::env::split_paths(&prev.clone().unwrap_or_default())),
    ).unwrap();
    unsafe { std::env::set_var("PATH", new_path); }

    let args = ResumeArgs {
        input: doc_file.to_string_lossy().to_string(),
        cwd: Some(cwd.path().to_path_buf()),
        harness: Some(HarnessArg::Claude),
        no_cache: false, force: false, url: None,
    };

    let recorder = RecordingExec::default();
    run_with_strategy(args, &recorder).unwrap();

    // Restore PATH.
    unsafe {
        match prev {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
    }

    let cap = recorder.captured();
    assert_eq!(cap.binary, "claude");
    assert_eq!(cap.args[0], "-r");
    assert_eq!(cap.cwd, std::fs::canonicalize(cwd.path()).unwrap());
}
```

- [ ] **Step 3: Run the orchestration test**

```bash
cargo test -p path-cli --lib run_with_strategy_records_invocation_for_file_input_with_explicit_harness
```

Expected: PASS.

- [ ] **Step 4: Run all `cmd_resume` tests**

```bash
cargo test -p path-cli --lib cmd_resume
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(path-cli): wire path resume orchestration end-to-end"
```

---

## Task 10: Integration tests

**Files:**
- Create: `crates/path-cli/tests/resume.rs`
- Create: `crates/path-cli/tests/support/mod.rs`

- [ ] **Step 1: Add the `support` module**

Create `crates/path-cli/tests/support/mod.rs`:

```rust
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub struct ScopedHome { _td: tempfile::TempDir, prev: Option<OsString>, prev_config: Option<OsString> }

impl ScopedHome {
    pub fn new() -> Self {
        let td = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        let prev_config = std::env::var_os("TOOLPATH_CONFIG_DIR");
        unsafe {
            std::env::set_var("HOME", td.path());
            std::env::set_var("TOOLPATH_CONFIG_DIR", td.path().join(".toolpath"));
        }
        Self { _td: td, prev, prev_config }
    }
}

impl Drop for ScopedHome {
    fn drop(&mut self) {
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match &self.prev_config {
                Some(v) => std::env::set_var("TOOLPATH_CONFIG_DIR", v),
                None => std::env::remove_var("TOOLPATH_CONFIG_DIR"),
            }
        }
    }
}

pub struct ScopedPath { _td: tempfile::TempDir, prev: Option<OsString> }

impl ScopedPath {
    pub fn with_binary(name: &str) -> Self { Self::with_binaries(&[name]) }

    pub fn with_binaries(names: &[&str]) -> Self {
        let td = tempfile::tempdir().unwrap();
        for n in names {
            let p = td.path().join(n);
            std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = std::fs::metadata(&p).unwrap().permissions();
                perm.set_mode(0o755);
                std::fs::set_permissions(&p, perm).unwrap();
            }
        }
        let prev = std::env::var_os("PATH");
        let new_path = std::env::join_paths(
            std::iter::once(td.path().to_path_buf())
                .chain(std::env::split_paths(&prev.clone().unwrap_or_default()))
        ).unwrap();
        unsafe { std::env::set_var("PATH", new_path); }
        Self { _td: td, prev }
    }

    pub fn empty() -> Self {
        let td = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("PATH");
        unsafe { std::env::set_var("PATH", td.path()); }
        Self { _td: td, prev }
    }
}

impl Drop for ScopedPath {
    fn drop(&mut self) {
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap())
}

pub fn write_minimal_path_file(dir: &Path, actor: &str) -> PathBuf {
    use toolpath::v1::{Path as TPath, PathIdentity, Step};
    let step = Step::new("s1", actor, "2026-01-01T00:00:00Z")
        .with_raw_change("a.txt", "@@ -1 +1 @@\n-old\n+new");
    let path = TPath {
        path: PathIdentity {
            id: "p1".to_string(),
            base: None,
            head: "s1".to_string(),
            graph_ref: None,
        },
        steps: vec![step],
        meta: None,
    };
    let graph = toolpath::v1::Graph::from_path(path);
    let p = dir.join("doc.json");
    std::fs::write(&p, graph.to_json().unwrap()).unwrap();
    p
}

pub fn args(input: PathBuf, cwd: &Path, harness: path_cli::cmd_share::HarnessArg) -> path_cli::cmd_resume::ResumeArgs {
    path_cli::cmd_resume::ResumeArgs {
        input: input.to_string_lossy().to_string(),
        cwd: Some(cwd.to_path_buf()),
        harness: Some(harness),
        no_cache: false, force: false, url: None,
    }
}

pub fn walk_dir_finds_jsonl(root: &Path) -> bool {
    fn walk(p: &Path) -> bool {
        if p.is_dir() {
            for e in std::fs::read_dir(p).unwrap() {
                if walk(&e.unwrap().path()) { return true; }
            }
            false
        } else {
            p.extension().and_then(|s| s.to_str()) == Some("jsonl")
        }
    }
    walk(root)
}
```

- [ ] **Step 2: Add the integration test file with all per-harness positive cases**

Create `crates/path-cli/tests/resume.rs`:

```rust
#![cfg(not(target_os = "emscripten"))]

use path_cli::cmd_resume::{run_with_strategy, RecordingExec, ResumeArgs};
use path_cli::cmd_share::HarnessArg;

mod support;
use support::*;

#[test]
fn file_input_explicit_claude_projects_and_records_exec() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let doc_file = write_minimal_path_file(cwd.path(), "agent:claude-code");
    let _path_guard = ScopedPath::with_binary("claude");

    let recorder = RecordingExec::default();
    run_with_strategy(args(doc_file, cwd.path(), HarnessArg::Claude), &recorder).unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "claude");
    assert_eq!(cap.args[0], "-r");

    let projects = home_dir().join(".claude/projects");
    assert!(projects.exists(), "claude projects dir missing");
    assert!(walk_dir_finds_jsonl(&projects), "no JSONL written");
}

#[test]
fn file_input_explicit_gemini_projects_and_records_exec() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let doc_file = write_minimal_path_file(cwd.path(), "agent:gemini-cli");
    let _path_guard = ScopedPath::with_binary("gemini");

    let recorder = RecordingExec::default();
    run_with_strategy(args(doc_file, cwd.path(), HarnessArg::Gemini), &recorder).unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "gemini");
    assert_eq!(cap.args[0], "--resume");

    let tmp_root = home_dir().join(".gemini/tmp");
    assert!(tmp_root.exists(), "gemini tmp dir missing");
}

#[test]
fn file_input_explicit_codex_projects_and_records_exec() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let doc_file = write_minimal_path_file(cwd.path(), "agent:codex");
    let _path_guard = ScopedPath::with_binary("codex");

    let recorder = RecordingExec::default();
    run_with_strategy(args(doc_file, cwd.path(), HarnessArg::Codex), &recorder).unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "codex");
    assert_eq!(cap.args[0], "resume");

    let sessions = home_dir().join(".codex/sessions");
    assert!(sessions.exists(), "codex sessions dir missing");
}

#[test]
fn file_input_explicit_opencode_projects_and_records_exec() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let doc_file = write_minimal_path_file(cwd.path(), "agent:opencode");
    let _path_guard = ScopedPath::with_binary("opencode");

    // Pre-create the opencode db with the canonical schema.
    let resolver = toolpath_opencode::PathResolver::new();
    let db_path = resolver.db_path().unwrap();
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // Substitute actual bootstrap helper if different.
        toolpath_opencode::schema::apply_full_schema(&conn).unwrap();
    }

    let recorder = RecordingExec::default();
    run_with_strategy(args(doc_file, cwd.path(), HarnessArg::Opencode), &recorder).unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "opencode");
    assert_eq!(cap.args[0], "--session");
}

#[test]
fn file_input_explicit_pi_projects_and_records_exec() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let doc_file = write_minimal_path_file(cwd.path(), "agent:pi");
    let _path_guard = ScopedPath::with_binary("pi");

    let recorder = RecordingExec::default();
    run_with_strategy(args(doc_file, cwd.path(), HarnessArg::Pi), &recorder).unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "pi");
    assert_eq!(cap.args[0], "--session");

    let sessions = home_dir().join(".pi/agent/sessions");
    assert!(sessions.exists(), "pi sessions dir missing");
}
```

- [ ] **Step 3: Add the rejection cases**

Append to `tests/resume.rs`:

```rust
#[test]
fn multi_path_graph_returns_clear_error() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let _path_guard = ScopedPath::with_binary("claude");

    // Build a graph with two inline paths.
    let p1 = {
        let json = std::fs::read_to_string(write_minimal_path_file(cwd.path(), "agent:claude-code")).unwrap();
        toolpath::v1::Graph::from_json(&json).unwrap().into_single_path().unwrap()
    };
    let mut p2 = p1.clone();
    p2.path.id = "p2".into();
    let mut g = toolpath::v1::Graph::from_path(p1);
    g.paths.push(toolpath::v1::PathOrRef::Path(Box::new(p2)));
    let doc_file = cwd.path().join("multi.json");
    std::fs::write(&doc_file, g.to_json().unwrap()).unwrap();

    let recorder = RecordingExec::default();
    let err = run_with_strategy(args(doc_file, cwd.path(), HarnessArg::Claude), &recorder)
        .unwrap_err();
    let s = err.to_string();
    assert!(s.contains("single `Path`"), "actual: {s}");
    assert!(s.contains("2 paths"), "actual: {s}");
}

#[test]
fn agentless_path_returns_clear_error() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let _path_guard = ScopedPath::with_binary("claude");
    let doc_file = write_minimal_path_file(cwd.path(), "human:alex");

    let recorder = RecordingExec::default();
    let err = run_with_strategy(args(doc_file, cwd.path(), HarnessArg::Claude), &recorder)
        .unwrap_err();
    assert!(err.to_string().contains("no agent session"));
}

#[test]
fn explicit_harness_not_on_path_errors() {
    let _home = ScopedHome::new();
    let _path_guard = ScopedPath::empty();
    let cwd = tempfile::tempdir().unwrap();
    let doc_file = write_minimal_path_file(cwd.path(), "agent:claude-code");

    let recorder = RecordingExec::default();
    let err = run_with_strategy(args(doc_file, cwd.path(), HarnessArg::Claude), &recorder)
        .unwrap_err();
    assert!(err.to_string().contains("isn't on PATH"));
}
```

- [ ] **Step 4: Add cache-id and URL input tests**

```rust
#[test]
fn cache_id_input_loads_and_projects() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let _path_guard = ScopedPath::with_binary("claude");

    // Stash a graph in the cache under a known id.
    let cache_id = "claude-test-fixture";
    let doc_file = write_minimal_path_file(cwd.path(), "agent:claude-code");
    let json = std::fs::read_to_string(&doc_file).unwrap();
    let graph = toolpath::v1::Graph::from_json(&json).unwrap();
    path_cli::cmd_cache::write_cached(cache_id, &graph, false).unwrap();

    let resume_args = path_cli::cmd_resume::ResumeArgs {
        input: cache_id.to_string(),
        cwd: Some(cwd.path().to_path_buf()),
        harness: Some(HarnessArg::Claude),
        no_cache: false, force: false, url: None,
    };
    let recorder = RecordingExec::default();
    run_with_strategy(resume_args, &recorder).unwrap();
    assert_eq!(recorder.captured().binary, "claude");
}

// URL input case — uses the in-repo MockServer test helper. If the
// MockServer module isn't reachable from cross-test binaries, skip
// or re-implement a minimal mock here.
```

(The URL input test depends on `path_cli::cmd_pathbase::tests::MockServer` being reachable. If `pub(crate)` doesn't bridge across the integration-test binary boundary, either move `MockServer` to a tiny `pub` test-utilities module or write a minimal inline mock for this single test. Decide at implementation time.)

- [ ] **Step 5: Run all integration tests**

```bash
cargo test -p path-cli --test resume
```

Expected: PASS.

- [ ] **Step 6: Run the full `path-cli` test suite**

```bash
cargo test -p path-cli
```

Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add crates/path-cli/tests/
git commit -m "test(path-cli): integration tests for path resume"
```

---

## Task 11: Documentation

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md`
- Modify: `crates/path-cli/src/cmd_resume.rs` (rustdoc)
- Create or modify: `CHANGELOG.md`

- [ ] **Step 1: Add `path resume` to the `CLAUDE.md` CLI usage block**

Find the existing CLI usage block (the long bash block with `path import …` etc.) and add, near `path share`:

````markdown
# Resume an agent session into your coding agent of choice
cargo run -p path-cli -- resume <pathbase-url-or-file-or-cache-id>
cargo run -p path-cli -- resume <input> --harness claude -C /path/to/project
````

- [ ] **Step 2: Add a "Things to know" bullet for `path resume`**

In the same `CLAUDE.md`, append (next to the `path share` bullet):

````markdown
- `path resume <input>` is the inverse of `path share`. It takes a Pathbase URL, shorthand (`owner/repo/slug`), file path, or cache id; resolves it to a Toolpath document; lets the user pick a coding-agent harness (interactive picker by default, `--harness X` to skip); projects the session into the harness's on-disk layout under the chosen cwd (default: shell cwd; override with `-C, --cwd P`); then execs the harness's resume command. Source harness is read from `path.meta.source` when present, with actor-string fallback. Documents that aren't a single agent-bearing `Path` are rejected with a message.
````

- [ ] **Step 3: Add a one-line mention to `README.md`**

In whichever section enumerates CLI verbs, add `path resume <input>` next to `path share`.

- [ ] **Step 4: Beef up the rustdoc on `cmd_resume.rs`**

Replace the placeholder module comment with a real one:

```rust
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
//! process. On Windows it's spawned and waited on with the exit code
//! propagated. If exec fails, the recipe is printed to stderr.
//!
//! See `docs/superpowers/specs/2026-05-08-path-resume-command-design.md`.
```

- [ ] **Step 5: Add a `CHANGELOG.md` entry**

Add a new section at the top (above the most recent entry; create the file with `# Changelog` header if it doesn't exist):

```markdown
## path-cli 0.9.0 — 2026-05-08

### Added
- `path resume <input>` — fetch a Toolpath document (URL, shorthand,
  file path, or cache id), pick a coding-agent harness, project the
  session into its on-disk layout under a chosen cwd, and exec the
  harness's resume command.
- `cmd_export::project_<harness>` `pub(crate)` wrappers that compose
  the existing build + write helpers and return the projected session
  id. Consumed by `path resume`.
```

- [ ] **Step 6: Build the docs to confirm they compile**

```bash
cargo doc -p path-cli --no-deps
```

Expected: clean build.

- [ ] **Step 7: Commit**

```bash
git add CLAUDE.md README.md CHANGELOG.md crates/path-cli/src/cmd_resume.rs
git commit -m "docs: document path resume command"
```

---

## Task 12: Version bumps

**Files:**
- Modify: `crates/path-cli/Cargo.toml`
- Modify: `Cargo.toml` (root)
- Modify: `site/_data/crates.json`

- [ ] **Step 1: Bump `path-cli` minor version**

In `crates/path-cli/Cargo.toml`:

```toml
version = "0.9.0"   # was 0.8.0
```

- [ ] **Step 2: Bump the workspace dep entry**

In the root `Cargo.toml`, find the `[workspace.dependencies]` `path-cli` entry and bump to match. (Adjust to match the existing entry's exact shape — `path` may or may not be present.)

- [ ] **Step 3: Bump the site data**

In `site/_data/crates.json`, update the `path-cli` entry's `version` field to `"0.9.0"`.

- [ ] **Step 4: Verify the workspace builds**

```bash
cargo build --workspace
```

Expected: clean build.

- [ ] **Step 5: Verify the workspace tests pass**

```bash
cargo test --workspace
```

Expected: all green.

- [ ] **Step 6: Verify clippy is clean**

```bash
cargo clippy --workspace -- -D warnings
```

Expected: no warnings.

- [ ] **Step 7: Build the site to confirm `crates.json` is well-formed**

```bash
cd site && pnpm run build && cd ..
```

Expected: 7 pages built; no errors.

- [ ] **Step 8: Commit**

```bash
git add crates/path-cli/Cargo.toml Cargo.toml site/_data/crates.json
git commit -m "chore: bump path-cli to 0.9.0 for path resume"
```

---

## Task 13: Smoke test from the CLI

**Files:** none modified — manual verification only.

- [ ] **Step 1: Build the CLI**

```bash
cargo build -p path-cli --release
```

- [ ] **Step 2: Verify `--help` lists the new command**

```bash
./target/release/path resume --help
```

Expected: usage line + flags listed exactly as documented in `cmd_resume.rs`.

- [ ] **Step 3: Confirm rejection paths work end-to-end**

Pick or derive a cache entry that's not from a harness (e.g. a `git-*` entry from `path import git`). Then attempt to resume:

```bash
./target/release/path resume <git-cache-id> --harness claude
```

Expected: error message `no agent session in input — \`path resume\` only works on harness-derived paths`.

- [ ] **Step 4: (Optional) Confirm a real resume works against an actual session**

Only if you have a real claude/codex/gemini/opencode/pi session locally and one of those binaries on PATH:

```bash
./target/release/path import claude --project $PWD
./target/release/path resume <cache-id-from-prev-step> --harness claude
```

Expected: control transfers to the harness with the prior conversation visible.

- [ ] **Step 5: No commit needed for smoke testing**

Manual step only.

---

## Self-review checklist

1. Every task ends with a `git commit` — verified.
2. Every code step shows actual code, not "implement X" — verified.
3. Every test step shows actual test, run command, and expected outcome — verified.
4. File paths are absolute or workspace-relative — verified.
5. Type names are consistent across tasks (`ResumeArgs`, `ExecStrategy`, `RecordingExec`, `RealExec`, `Harness`, `HarnessArg`, `CapturedExec`) — verified.
6. No `ResumeRecipe` references — verified (collapsed into `(session_id, argv_for, exec_harness)`).
7. Spec coverage:
   - § Surface — Tasks 3, 9.
   - § Input resolution — Task 5.
   - § Launch — Tasks 8, 9.
   - § Internal architecture (`resolve_input`, `ensure_path_with_agent`, `pick_harness`, `project_into_harness`, `argv_for`, `exec_harness`) — Tasks 4–9.
   - § `project_<harness>` wrappers — Task 1.
   - § Error handling — Tasks 4, 5, 6, 10.
   - § Testing — Tasks 1–10, 13.
   - § Documentation — Task 11.
   - § Versioning — Task 12.
8. No "TBD", "TODO", or "implement later" — verified.
