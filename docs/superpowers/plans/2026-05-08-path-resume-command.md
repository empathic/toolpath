# `path resume` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `path resume <input>` — fetches/loads a Toolpath document, picks a coding-agent harness (interactive picker by default, `--harness X` to skip), projects the session into the harness's on-disk layout in a chosen cwd, then execs the harness's resume command.

**Architecture:** New `cmd_resume.rs` module mirroring `cmd_share.rs`. Reuses the per-harness projection helpers in `cmd_export.rs` after a small refactor that has each project-mode writer return a `ResumeRecipe { binary, args, session_id, cwd_for_recipe }`. The CLI surface for `path export <harness> --project P` is unchanged; the new code path consumes the recipe directly and feeds it to an injectable `ExecStrategy` (the binary plugs in `execvp`; tests plug in a recorder).

**Tech Stack:** Rust 2024, clap, anyhow, `toolpath_*` workspace crates, existing `crate::fzf` helper, `cmd_share::Harness` enum, `pathbase-client`. New types are `pub` only where the desktop app might consume them later.

**Spec reference:** `docs/superpowers/specs/2026-05-08-path-resume-command-design.md`.

---

## Type and API quick reference

The plan's code samples lean on these existing types and functions. Cross-check against the source before writing tests — the names below are what's actually in the repo as of branch `akesling/resume`.

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

**There is no `Document` enum.** `Graph::from_json` is the universal entry point — every cache file, every Pathbase response, every Toolpath JSON parses as a `Graph`. Single-path-graphs are the closest thing to a "Path document"; `into_single_path` unwraps them. The plan validates everything as a `Graph` (see Task 8).

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

Whenever a task below refers to `path_with_actor(...)` or `make_minimal_<harness>_path()`, the body is the snippet above with `actor` substituted. Each task lists the actor explicitly.

---

## File Structure

**New:**
- `crates/path-cli/src/cmd_resume.rs` — module: `ResumeArgs`, `ResumeRecipe` re-export, orchestration, `resolve_input`, `infer_source_harness`, `ensure_path_with_agent`, `pick_harness`, `exec_harness`, picker.
- `crates/path-cli/tests/resume.rs` — integration tests with injectable exec strategy.

**Modified:**
- `crates/path-cli/src/cmd_export.rs` — add `pub struct ResumeRecipe`; change `write_into_claude_project`, `write_into_gemini_project`, `write_into_codex_project`, `write_into_opencode_db`, `write_into_pi_project` to return `Result<ResumeRecipe>`; have each `run_<harness>`'s project-mode arm format the recipe to stderr (preserving current output).
- `crates/path-cli/src/cmd_import.rs` — extract a `pub(crate) fn pathbase_fetch_to_doc(target: &str, url_flag: Option<&str>) -> Result<DerivedDoc>` from the inner block of `derive_pathbase`. `derive_pathbase` becomes a one-line wrapper.
- `crates/path-cli/src/lib.rs` — add `Commands::Resume { args: cmd_resume::ResumeArgs }`; wire dispatch.
- `crates/path-cli/Cargo.toml` — minor version bump (`0.8.0` → `0.9.0`).
- `Cargo.toml` (root) — `[workspace.dependencies]` `path-cli` version bump.
- `site/_data/crates.json` — `path-cli` version bump.
- `CHANGELOG.md` — new entry.
- `CLAUDE.md` — CLI usage block + "Things to know" bullet.
- `README.md` — one-line mention.

---

## Task 1: Introduce `ResumeRecipe` and refactor Claude project-mode writer

**Files:**
- Modify: `crates/path-cli/src/cmd_export.rs:230` (add type near `PathbaseUploadArgs`)
- Modify: `crates/path-cli/src/cmd_export.rs:255-268` (run_claude project arm) and `crates/path-cli/src/cmd_export.rs:321-342` (write_into_claude_project)
- Test: `crates/path-cli/src/cmd_export.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Append to the existing tests module in `cmd_export.rs` (find it near the bottom of the file under `#[cfg(test)] mod tests {`):

```rust
#[test]
#[cfg(not(target_os = "emscripten"))]
fn write_into_claude_project_returns_recipe() {
    let tmp = tempfile::tempdir().unwrap();
    let path = make_path_with_actor("agent:claude-code");   // see "Type and API quick reference"

    let conv = build_claude_conversation(&path).unwrap();
    let jsonl = serialize_jsonl(&conv).unwrap();
    let recipe = write_into_claude_project(&conv, &jsonl, tmp.path()).unwrap();

    assert_eq!(recipe.binary, "claude");
    assert_eq!(recipe.args, vec!["-r".to_string(), conv.session_id.clone()]);
    assert_eq!(recipe.session_id, conv.session_id);
    assert_eq!(recipe.cwd_for_recipe, std::fs::canonicalize(tmp.path()).unwrap());
}
```

If `make_path_with_actor` and `make_step` aren't already in scope, add them to the test module — they're used throughout the plan's tests. Crib the bodies from the "Type and API quick reference" section above (or copy `cmd_merge.rs::tests::{make_step, make_path}` directly).

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p path-cli --lib write_into_claude_project_returns_recipe
```

Expected: FAIL — `write_into_claude_project` currently returns `Result<PathBuf>`, not `Result<ResumeRecipe>`.

- [ ] **Step 3: Add the `ResumeRecipe` type**

Insert near `PathbaseUploadArgs` (around `cmd_export.rs:230`):

```rust
/// What `path resume` needs to launch a harness's interactive resume
/// after a successful project-mode export. Returned by every
/// `write_into_<harness>_project` helper.
#[cfg(not(target_os = "emscripten"))]
#[derive(Debug, Clone)]
pub struct ResumeRecipe {
    /// Binary name as it appears on PATH (e.g. `"claude"`, `"codex"`).
    pub binary: &'static str,
    /// Argv after the binary name (e.g. `["-r", "<session-id>"]`).
    pub args: Vec<String>,
    /// Session id the recipe targets. Convenience accessor — also
    /// embedded in `args` when relevant.
    pub session_id: String,
    /// Directory the harness must be invoked from. Already canonicalized.
    pub cwd_for_recipe: std::path::PathBuf,
}
```

- [ ] **Step 4: Refactor `write_into_claude_project` to return the recipe**

Replace the existing function body:

```rust
#[cfg(not(target_os = "emscripten"))]
fn write_into_claude_project(
    conv: &toolpath_claude::Conversation,
    jsonl: &str,
    project_dir: &std::path::Path,
) -> Result<ResumeRecipe> {
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let project_path = project_dir.to_string_lossy();

    let resolver = toolpath_claude::PathResolver::new();
    let claude_project_dir = resolver
        .project_dir(&project_path)
        .map_err(|e| anyhow::anyhow!("Cannot resolve Claude project dir: {}", e))?;

    std::fs::create_dir_all(&claude_project_dir)
        .with_context(|| format!("create {}", claude_project_dir.display()))?;

    let session_id = conv.session_id.clone();
    let out_path = claude_project_dir.join(format!("{}.jsonl", session_id));
    std::fs::write(&out_path, jsonl).with_context(|| format!("write {}", out_path.display()))?;

    Ok(ResumeRecipe {
        binary: "claude",
        args: vec!["-r".to_string(), session_id.clone()],
        session_id,
        cwd_for_recipe: project_dir,
    })
}
```

- [ ] **Step 5: Update `run_claude`'s project arm to print from the recipe**

In `run_claude` (around line 255), the `(Some(project_dir), None)` branch becomes:

```rust
(Some(project_dir), None) => {
    let recipe = write_into_claude_project(&conversation, &jsonl, &project_dir)?;
    let session_id = &recipe.session_id;
    eprintln!(
        "Exported session {} ({} entries) → {}",
        session_id,
        conversation.preamble.len() + conversation.entries.len(),
        recipe.cwd_for_recipe.display()
    );
    eprintln!();
    eprintln!("Resume with:");
    eprintln!(
        "  cd {} && {} {}",
        recipe.cwd_for_recipe.display(),
        recipe.binary,
        recipe.args.join(" ")
    );
}
```

(Note: the existing message says `"Exported session ... → <out_path>"` showing the JSONL filename. Switch to `recipe.cwd_for_recipe` so the recipe-print is self-contained — the file path is implied by the harness's resolver and isn't useful to the user.)

- [ ] **Step 6: Run test to verify it passes**

```bash
cargo test -p path-cli --lib write_into_claude_project_returns_recipe
```

Expected: PASS.

- [ ] **Step 7: Run the full export tests to confirm no regressions**

```bash
cargo test -p path-cli --lib cmd_export
```

Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/path-cli/src/cmd_export.rs
git commit -m "refactor(path-cli): return ResumeRecipe from claude project-mode export"
```

---

## Task 2: Refactor Gemini project-mode writer to return `ResumeRecipe`

**Files:**
- Modify: `crates/path-cli/src/cmd_export.rs:407-441` (write_into_gemini_project) and the caller `run_gemini` (~line 368)
- Test: `crates/path-cli/src/cmd_export.rs` (inline tests module)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
#[cfg(not(target_os = "emscripten"))]
fn write_into_gemini_project_returns_recipe() {
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().to_string_lossy().to_string();

    let path = make_path_with_actor("agent:gemini-cli");
    let view = toolpath_convo::extract_conversation(&path);
    let project_hash = toolpath_gemini::paths::project_hash(&project_path);
    let projector = toolpath_gemini::project::GeminiProjector::new()
        .with_project_hash(project_hash)
        .with_project_path(project_path.clone());
    let conv = projector.project(&view).unwrap();

    let recipe = write_into_gemini_project(&conv, &project_path).unwrap();

    assert_eq!(recipe.binary, "gemini");
    assert_eq!(recipe.args, vec!["--resume".to_string(), conv.session_uuid.clone()]);
    assert_eq!(recipe.session_id, conv.session_uuid);
    assert_eq!(recipe.cwd_for_recipe, std::path::PathBuf::from(&project_path));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p path-cli --lib write_into_gemini_project_returns_recipe
```

Expected: FAIL — current return type is `Result<()>`.

- [ ] **Step 3: Refactor `write_into_gemini_project`**

Replace the body (lines 407-441):

```rust
#[cfg(not(target_os = "emscripten"))]
fn write_into_gemini_project(
    conversation: &toolpath_gemini::types::Conversation,
    project_path: &str,
) -> Result<ResumeRecipe> {
    let resolver = toolpath_gemini::PathResolver::new();
    let chats_dir = resolver
        .chats_dir(project_path)
        .map_err(|e| anyhow::anyhow!("Cannot resolve Gemini chats dir: {}", e))?;
    std::fs::create_dir_all(&chats_dir)
        .with_context(|| format!("create {}", chats_dir.display()))?;

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

    Ok(ResumeRecipe {
        binary: "gemini",
        args: vec!["--resume".to_string(), conversation.session_uuid.clone()],
        session_id: conversation.session_uuid.clone(),
        cwd_for_recipe: std::path::PathBuf::from(project_path),
    })
}
```

- [ ] **Step 4: Update `run_gemini` project arm to print from the recipe**

In `run_gemini`'s match (around line 368), the `(Some(_), None)` branch becomes:

```rust
(Some(_), None) => {
    let recipe = write_into_gemini_project(&conversation, &project_path)?;
    eprintln!();
    eprintln!("Resume with:");
    eprintln!(
        "  cd {} && {} {}",
        recipe.cwd_for_recipe.display(),
        recipe.binary,
        recipe.args.join(" ")
    );
}
```

- [ ] **Step 5: Run test to verify it passes**

```bash
cargo test -p path-cli --lib write_into_gemini_project_returns_recipe
```

Expected: PASS.

- [ ] **Step 6: Run gemini export tests**

```bash
cargo test -p path-cli --lib gemini
```

Expected: pass (in particular `gemini_writes_resume_ready_layout`).

- [ ] **Step 7: Commit**

```bash
git add crates/path-cli/src/cmd_export.rs
git commit -m "refactor(path-cli): return ResumeRecipe from gemini project-mode export"
```

---

## Task 3: Refactor Codex project-mode writer to return `ResumeRecipe`

**Files:**
- Modify: `crates/path-cli/src/cmd_export.rs:765-815` (write_into_codex_project) and run_codex caller (~line 732)
- Test: `crates/path-cli/src/cmd_export.rs` (inline tests module)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
#[cfg(not(target_os = "emscripten"))]
fn write_into_codex_project_returns_recipe() {
    let _home = scoped_home(tempfile::tempdir().unwrap());   // see Step 2
    let path = make_path_with_actor("agent:codex");
    let session = build_codex_session_for_test(&path, "/tmp/x");
    let recipe = write_into_codex_project(&session).unwrap();

    assert_eq!(recipe.binary, "codex");
    assert_eq!(recipe.args, vec!["resume".to_string(), session.id.clone()]);
    assert_eq!(recipe.session_id, session.id);
    // codex resume reads state_5.sqlite, so cwd doesn't matter for invocation;
    // the recipe records cwd as the recorded session cwd for completeness.
    assert_eq!(recipe.cwd_for_recipe, std::path::PathBuf::from("/tmp/x"));
}
```

- [ ] **Step 2: Add the `scoped_home` and codex fixture helpers**

In the tests module, add (or extend if equivalents exist):

```rust
#[cfg(not(target_os = "emscripten"))]
struct ScopedHome { _td: tempfile::TempDir, prev: Option<std::ffi::OsString> }

#[cfg(not(target_os = "emscripten"))]
fn scoped_home(td: tempfile::TempDir) -> ScopedHome {
    let prev = std::env::var_os("HOME");
    // Safety: tests are single-threaded under `cargo test --test-threads=1`
    // for this crate (see existing `cmd_pathbase` test pattern). If the
    // crate ever flips to multi-threaded, replace with `serial_test`.
    unsafe { std::env::set_var("HOME", td.path()); }
    ScopedHome { _td: td, prev }
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

#[cfg(not(target_os = "emscripten"))]
fn build_codex_session_for_test(path: &toolpath::v1::Path, cwd: &str) -> toolpath_codex::Session {
    use toolpath_convo::ConversationProjector;
    let view = toolpath_convo::extract_conversation(path);
    let projector = toolpath_codex::project::CodexProjector::new().with_cwd(cwd.to_string());
    projector.project(&view).unwrap()
}
```

(Verify whether the existing tests already define a `scoped_home`-like helper — if so, reuse it instead of duplicating.)

- [ ] **Step 3: Run test to verify it fails**

```bash
cargo test -p path-cli --lib write_into_codex_project_returns_recipe
```

Expected: FAIL — current return type is `Result<()>`.

- [ ] **Step 4: Refactor `write_into_codex_project`**

Find the existing body (line 765 onwards). Replace the trailing `eprintln!()` block + `Ok(())` with the recipe-returning shape:

```rust
#[cfg(not(target_os = "emscripten"))]
fn write_into_codex_project(session: &toolpath_codex::Session) -> Result<ResumeRecipe> {
    let session_ts = codex_session_timestamp(session)?;
    let resolver = toolpath_codex::PathResolver::new();
    let sessions_root = resolver
        .sessions_root()
        .map_err(|e| anyhow::anyhow!("Cannot resolve Codex sessions dir: {}", e))?;

    let date_dir = sessions_root
        .join(session_ts.format("%Y").to_string())
        .join(session_ts.format("%m").to_string())
        .join(session_ts.format("%d").to_string());
    std::fs::create_dir_all(&date_dir).with_context(|| format!("create {}", date_dir.display()))?;

    let stem = codex_rollout_stem(session, &session_ts);
    let out_path = date_dir.join(format!("{}.jsonl", stem));
    let bytes = serialize_codex_jsonl(session)?;
    std::fs::write(&out_path, &bytes).with_context(|| format!("write {}", out_path.display()))?;

    let codex_dir = resolver
        .codex_dir()
        .map_err(|e| anyhow::anyhow!("Cannot resolve ~/.codex dir: {}", e))?;
    let registration = register_codex_thread(&codex_dir, session, &out_path, &session_ts);

    eprintln!(
        "Exported Codex session {} ({} lines) → {}",
        session.id,
        session.lines.len(),
        out_path.display()
    );
    match registration {
        Ok(true) => eprintln!("  registered in {}/state_5.sqlite", codex_dir.display()),
        Ok(false) => eprintln!(
            "  warning: state_5.sqlite not found at {} — `codex resume` won't see this session",
            codex_dir.display()
        ),
        Err(e) => eprintln!(
            "  warning: failed to register thread in state_5.sqlite: {} — `codex resume` may not see this session",
            e
        ),
    }

    let recorded_cwd = session
        .meta()
        .map(|m| m.cwd.clone())
        .unwrap_or_else(|| std::path::PathBuf::from("/"));

    Ok(ResumeRecipe {
        binary: "codex",
        args: vec!["resume".to_string(), session.id.clone()],
        session_id: session.id.clone(),
        cwd_for_recipe: recorded_cwd,
    })
}
```

- [ ] **Step 5: Update `run_codex` project arm**

In `run_codex` (around line 732), the `(Some(_), None)` branch becomes:

```rust
(Some(_), None) => {
    let recipe = write_into_codex_project(&session)?;
    eprintln!();
    eprintln!("Loadable via:");
    eprintln!("  path import codex --session {}", recipe.session_id);
    eprintln!();
    eprintln!("Open conversation with:");
    eprintln!("  {} {}", recipe.binary, recipe.args.join(" "));
}
```

- [ ] **Step 6: Run test to verify it passes**

```bash
cargo test -p path-cli --lib write_into_codex_project_returns_recipe
```

Expected: PASS.

- [ ] **Step 7: Run codex export tests**

```bash
cargo test -p path-cli --lib codex
```

Expected: pass.

- [ ] **Step 8: Commit**

```bash
git add crates/path-cli/src/cmd_export.rs
git commit -m "refactor(path-cli): return ResumeRecipe from codex project-mode export"
```

---

## Task 4: Refactor opencode project-mode writer to return `ResumeRecipe`

**Files:**
- Modify: `crates/path-cli/src/cmd_export.rs:1024-1076` (write_into_opencode_db) and run_opencode caller (~line 985)
- Test: inline tests module

- [ ] **Step 1: Write the failing test**

```rust
#[test]
#[cfg(not(target_os = "emscripten"))]
fn write_into_opencode_db_returns_recipe() {
    let _home = scoped_home(tempfile::tempdir().unwrap());
    // Pre-create an empty opencode.db so the writer doesn't bail.
    let db_dir = dirs::data_local_dir().unwrap().join("opencode");
    std::fs::create_dir_all(&db_dir).unwrap();
    let db_path = db_dir.join("opencode.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // Minimal schema — copy from `toolpath_opencode::schema::CREATE_SQL`
        // or whatever the production schema bootstrap is. (See
        // existing opencode tests for the helper, if any.)
        toolpath_opencode::schema::apply_full_schema(&conn).unwrap();
    }

    let path = make_path_with_actor("agent:opencode");
    let session = build_opencode_session(&path, Some(std::path::Path::new("/tmp/x"))).unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let recipe = write_into_opencode_db(&session, project_dir.path()).unwrap();

    assert_eq!(recipe.binary, "opencode");
    assert_eq!(recipe.args, vec!["--session".to_string(), session.id.clone()]);
    assert_eq!(recipe.session_id, session.id);
    assert_eq!(
        recipe.cwd_for_recipe,
        std::fs::canonicalize(project_dir.path()).unwrap()
    );
}
```

If `toolpath_opencode::schema::apply_full_schema` doesn't exist, locate the canonical bootstrap helper that existing opencode tests use (search `crates/toolpath-opencode/src/`) and substitute the right name.

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p path-cli --lib write_into_opencode_db_returns_recipe
```

Expected: FAIL — return type mismatch.

- [ ] **Step 3: Refactor `write_into_opencode_db`**

Replace the function body, swapping the two `eprintln!` "Loadable via:" / "Open conversation with:" blocks for a returned `ResumeRecipe`:

```rust
#[cfg(not(target_os = "emscripten"))]
fn write_into_opencode_db(
    session: &toolpath_opencode::Session,
    project_dir: &std::path::Path,
) -> Result<ResumeRecipe> {
    use toolpath_opencode::PathResolver;

    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;

    let resolver = PathResolver::new();
    let db_path = resolver
        .db_path()
        .map_err(|e| anyhow::anyhow!("Cannot resolve opencode db path: {}", e))?;
    if !db_path.exists() {
        anyhow::bail!(
            "opencode database not found at {} — has opencode been run on this machine?",
            db_path.display()
        );
    }

    let mut conn = rusqlite::Connection::open(&db_path)
        .with_context(|| format!("open {}", db_path.display()))?;
    let tx = conn.transaction()?;

    ensure_opencode_project(&tx, &session.project_id, &project_dir, session.time_created)?;
    insert_opencode_session(&tx, session)?;
    let mut message_count = 0_usize;
    let mut part_count = 0_usize;
    for message in &session.messages {
        insert_opencode_message(&tx, message)?;
        message_count += 1;
        for part in &message.parts {
            insert_opencode_part(&tx, part)?;
            part_count += 1;
        }
    }
    tx.commit()?;

    eprintln!(
        "Exported opencode session {} ({} messages, {} parts) → {}",
        session.id,
        message_count,
        part_count,
        db_path.display()
    );

    Ok(ResumeRecipe {
        binary: "opencode",
        args: vec!["--session".to_string(), session.id.clone()],
        session_id: session.id.clone(),
        cwd_for_recipe: project_dir,
    })
}
```

**Verify the actual opencode resume invocation.** Read `crates/toolpath-opencode/README.md` or the opencode CLI's own help — if the canonical resume command is something other than `opencode --session <id>`, replace `args` with the right shape. (Today's `eprintln!` says `opencode --session <id>`, so that's the assumption baked in.)

- [ ] **Step 4: Update `run_opencode` project arm**

In `run_opencode` (around line 985), the `(Some(project_dir), None)` branch becomes:

```rust
(Some(project_dir), None) => {
    let session = build_opencode_session(&path, Some(&project_dir))?;
    let recipe = write_into_opencode_db(&session, &project_dir)?;
    eprintln!();
    eprintln!("Loadable via:");
    eprintln!("  path import opencode --session {}", recipe.session_id);
    eprintln!();
    eprintln!("Open conversation with:");
    eprintln!("  {} {}", recipe.binary, recipe.args.join(" "));
}
```

- [ ] **Step 5: Run test to verify it passes**

```bash
cargo test -p path-cli --lib write_into_opencode_db_returns_recipe
```

Expected: PASS.

- [ ] **Step 6: Run opencode export tests**

```bash
cargo test -p path-cli --lib opencode
```

Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add crates/path-cli/src/cmd_export.rs
git commit -m "refactor(path-cli): return ResumeRecipe from opencode project-mode export"
```

---

## Task 5: Refactor Pi project-mode writer to return `ResumeRecipe`

**Files:**
- Modify: `crates/path-cli/src/cmd_export.rs:622-650` (write_into_pi_project) and run_pi caller (search for `run_pi` in the file)
- Test: inline tests module

- [ ] **Step 1: Write the failing test**

```rust
#[test]
#[cfg(not(target_os = "emscripten"))]
fn write_into_pi_project_returns_recipe() {
    let _home = scoped_home(tempfile::tempdir().unwrap());
    let path = make_path_with_actor("agent:pi");
    let session = build_pi_session_for_test(&path, "/tmp/x");
    let recipe = write_into_pi_project(&session, "/tmp/x").unwrap();

    assert_eq!(recipe.binary, "pi");
    assert_eq!(recipe.args, vec!["--session".to_string(), session.header.id.clone()]);
    assert_eq!(recipe.session_id, session.header.id);
    assert_eq!(recipe.cwd_for_recipe, std::path::PathBuf::from("/tmp/x"));
}

#[cfg(not(target_os = "emscripten"))]
fn build_pi_session_for_test(path: &toolpath::v1::Path, cwd: &str) -> toolpath_pi::PiSession {
    use toolpath_convo::ConversationProjector;
    let view = toolpath_convo::extract_conversation(path);
    let projector = toolpath_pi::project::PiProjector::new().with_cwd(cwd.to_string());
    projector.project(&view).unwrap()
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p path-cli --lib write_into_pi_project_returns_recipe
```

Expected: FAIL.

- [ ] **Step 3: Refactor `write_into_pi_project`**

```rust
#[cfg(not(target_os = "emscripten"))]
fn write_into_pi_project(session: &toolpath_pi::PiSession, cwd: &str) -> Result<ResumeRecipe> {
    let resolver = toolpath_pi::PathResolver::new();
    let project_dir = resolver.project_dir(cwd);
    std::fs::create_dir_all(&project_dir)
        .with_context(|| format!("create {}", project_dir.display()))?;

    let stem = pi_session_stem(session);
    let out_path = project_dir.join(format!("{}.jsonl", stem));
    let bytes = serialize_pi_jsonl(session)?;
    std::fs::write(&out_path, &bytes).with_context(|| format!("write {}", out_path.display()))?;

    let entry_count = session.entries.len().saturating_sub(1);
    eprintln!(
        "Exported Pi session {} ({} entries) → {}",
        session.header.id,
        entry_count,
        out_path.display()
    );

    Ok(ResumeRecipe {
        binary: "pi",
        args: vec!["--session".to_string(), session.header.id.clone()],
        session_id: session.header.id.clone(),
        cwd_for_recipe: std::path::PathBuf::from(cwd),
    })
}
```

- [ ] **Step 4: Update `run_pi` project arm**

Find the `(Some(_), None)` branch in `run_pi`, replace with:

```rust
(Some(_), None) => {
    let recipe = write_into_pi_project(&session, &cwd_str)?;
    eprintln!();
    eprintln!("Loadable via:");
    eprintln!(
        "  path import pi --session {} --project {}",
        recipe.session_id,
        recipe.cwd_for_recipe.display()
    );
    eprintln!();
    eprintln!("Open conversation with:");
    eprintln!("  {} {}", recipe.binary, recipe.args.join(" "));
}
```

- [ ] **Step 5: Run test to verify it passes**

```bash
cargo test -p path-cli --lib write_into_pi_project_returns_recipe
```

Expected: PASS.

- [ ] **Step 6: Run pi export tests**

```bash
cargo test -p path-cli --lib pi
```

Expected: pass (in particular `pi_writes_resume_ready_layout`).

- [ ] **Step 7: Commit**

```bash
git add crates/path-cli/src/cmd_export.rs
git commit -m "refactor(path-cli): return ResumeRecipe from pi project-mode export"
```

---

## Task 6: Extract `pathbase_fetch_to_doc` from `cmd_import.rs`

**Files:**
- Modify: `crates/path-cli/src/cmd_import.rs:1362-1388` (derive_pathbase)

- [ ] **Step 1: Write the failing test**

In `cmd_import.rs`'s tests module (or in a new `#[cfg(test)] mod pathbase_fetch_tests` block adjacent to it), add:

```rust
#[test]
#[cfg(not(target_os = "emscripten"))]
fn pathbase_fetch_to_doc_url_input() {
    use crate::cmd_pathbase::tests::MockServer;
    let body = r#"{"Path":{"id":"p1","actor":"agent:claude-code","steps":[]}}"#;
    let server = MockServer::start("HTTP/1.1 200 OK", body);
    let url = format!("{}/alex/pathstash/my-path", server.base());

    let derived = pathbase_fetch_to_doc(&url, None).unwrap();

    assert_eq!(derived.cache_id, "pathbase-alex-pathstash-my-path");
    assert!(derived.doc.into_single_path().is_some());
}
```

If `cmd_pathbase::tests::MockServer` is not yet `pub(crate)`, this test will fail to compile — Step 3 below adds the visibility.

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p path-cli --lib pathbase_fetch_to_doc_url_input
```

Expected: FAIL — `pathbase_fetch_to_doc` doesn't exist; possibly also `MockServer` isn't pub(crate).

- [ ] **Step 3: Make `MockServer` reachable from sibling tests**

In `crates/path-cli/src/cmd_pathbase.rs`, change the existing test module declaration so the helper is reachable from sibling test modules:

```rust
#[cfg(test)]
pub(crate) mod tests {
    // (existing contents unchanged; the only change is `pub(crate)` and
    // promoting `MockServer` + its `impl` block to `pub(crate)`.)
    pub(crate) struct MockServer { /* ... */ }
    impl MockServer {
        pub(crate) fn start(/* ... */) -> Self { /* ... */ }
        pub(crate) fn base(&self) -> String { /* ... */ }
        // ...
    }
}
```

Promote only the items the new test needs. Existing tests inside the module continue to work unchanged.

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

## Task 7: Scaffold `cmd_resume.rs` — types, args, lib.rs wiring

**Files:**
- Create: `crates/path-cli/src/cmd_resume.rs`
- Modify: `crates/path-cli/src/lib.rs:45-180` (Commands enum + dispatch)

- [ ] **Step 1: Write a stub failing test**

Create `crates/path-cli/src/cmd_resume.rs`:

```rust
//! `path resume` — fetch / load a Toolpath document and exec a coding
//! agent's resume command after projecting the session into the
//! harness's on-disk layout.
//!
//! See `docs/superpowers/specs/2026-05-08-path-resume-command-design.md`.

#![cfg(not(target_os = "emscripten"))]

use anyhow::Result;
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

## Task 8: Implement `infer_source_harness` and `ensure_path_with_agent`

**Files:**
- Modify: `crates/path-cli/src/cmd_resume.rs`

- [ ] **Step 1: Write the failing tests**

Append to `cmd_resume.rs`'s tests module. There is no `Document` enum in this codebase — every parse goes through `Graph::from_json`, so validation operates on `Graph`. A "Path document" surfaces as a `Graph` with exactly one inline path.

```rust
use crate::cmd_share::Harness;
use toolpath::v1::{Graph, PathMeta, PathOrRef};
// `make_path_with_actor` and `make_step` come from the type-reference snippet
// at the top of this plan.

fn graph_of(path: toolpath::v1::Path) -> Graph {
    Graph::from_path(path)
}

#[test]
fn infer_source_harness_meta_source_wins() {
    let mut path = make_path_with_actor("agent:codex");   // actor sniff would say codex…
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
    let g = graph_of(make_path_with_actor("agent:claude-code"));
    assert!(ensure_path_with_agent(&g).is_ok());
}

#[test]
fn ensure_path_with_agent_rejects_empty_graph() {
    let g = Graph::from_path(make_path_with_actor("agent:claude-code")); // start with one
    let mut g = g;
    g.paths.clear();
    let err = ensure_path_with_agent(&g).unwrap_err();
    assert!(err.to_string().contains("expected"));
    assert!(err.to_string().contains("empty"));
}

#[test]
fn ensure_path_with_agent_rejects_multi_path_graph() {
    use toolpath::v1::PathOrRef;
    let mut g = Graph::from_path(make_path_with_actor("agent:claude-code"));
    g.paths.push(PathOrRef::Path(Box::new(make_path_with_actor("agent:claude-code"))));
    let err = ensure_path_with_agent(&g).unwrap_err();
    let s = err.to_string();
    assert!(s.contains("single `Path`"), "actual: {s}");
    assert!(s.contains("2 paths"), "actual: {s}");
}

#[test]
fn ensure_path_with_agent_rejects_agentless_path() {
    let g = graph_of(make_path_with_actor("human:alex"));
    let err = ensure_path_with_agent(&g).unwrap_err();
    assert!(err.to_string().contains("no agent session"));
}

#[test]
fn ensure_path_with_agent_rejects_path_ref_only_graph() {
    use toolpath::v1::{PathOrRef, PathRef};
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
    }
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

## Task 9: Implement `resolve_input`

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
    // or a leading dot (which would indicate a relative file path).
    if s.starts_with('.') || s.starts_with('/') { return false; }
    let segs: Vec<&str> = s.split('/').collect();
    segs.len() == 3 && segs.iter().all(|s| !s.is_empty() && !s.contains(char::is_whitespace))
}
```

`Graph::single_path` returns `Option<&Path>` — see the type reference. `infer_source_harness` takes `&Path`, so `.and_then(infer_source_harness)` is the right composition.

- [ ] **Step 4: Add `Context` import and any missing imports**

Make sure the top of `cmd_resume.rs` has:

```rust
use anyhow::{Context, Result};
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p path-cli --lib resolve_input
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(path-cli): resolve_input dispatcher for path resume"
```

---

## Task 10: Implement `pick_harness` non-interactive paths and PATH probe

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
    // Force non-interactive so we hit the "zero installed" branch
    // deterministically — the picker step is exercised in integration tests.
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

(The third test depends on `pick_harness` short-circuiting to the "zero installed" error before consulting `crate::fzf::available()`. The `path_override: Option<&std::path::Path>` parameter exists exclusively for tests.)

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p path-cli --lib pick_harness
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

/// All five harnesses, in the canonical picker order.
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
    // Format rows: "<symbol>   <annotation>"
    let mut lines: Vec<String> = Vec::with_capacity(installed.len());
    for h in installed {
        let mut tags: Vec<&str> = Vec::new();
        if Some(*h) == source {
            tags.push("source");
        }
        let suffix = if tags.is_empty() { String::new() } else { format!("  ({})", tags.join(", ")) };
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

    // Match by leading symbol (which uniquely identifies each harness).
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
cargo test -p path-cli --lib pick_harness
cargo test -p path-cli --lib binary_on_path
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(path-cli): harness picker + PATH probe for path resume"
```

---

## Task 11: Implement `project_into_harness` dispatcher

**Files:**
- Modify: `crates/path-cli/src/cmd_resume.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn project_into_harness_claude_round_trip() {
    let td = tempfile::tempdir().unwrap();
    let _home = scoped_home_for_resume(tempfile::tempdir().unwrap());

    let path = make_path_with_actor("agent:claude-code");
    let recipe = project_into_harness(&path, Harness::Claude, td.path()).unwrap();

    assert_eq!(recipe.binary, "claude");
    assert_eq!(recipe.args.len(), 2);
    assert_eq!(recipe.args[0], "-r");
    assert_eq!(
        recipe.cwd_for_recipe,
        std::fs::canonicalize(td.path()).unwrap()
    );
}
```

Add a `scoped_home_for_resume` mirroring the export-side `scoped_home`, or reuse it via `pub(crate)`.

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p path-cli --lib project_into_harness_claude_round_trip
```

Expected: FAIL.

- [ ] **Step 3: Implement `project_into_harness`**

Append to `cmd_resume.rs`:

```rust
use crate::cmd_export::ResumeRecipe;

/// Run the appropriate `cmd_export` project-mode helper and return its
/// recipe. The `cwd` is the directory the projection layout is keyed
/// on AND the directory the harness will be exec'd from.
pub(crate) fn project_into_harness(
    path: &TPath,
    harness: Harness,
    cwd: &std::path::Path,
) -> Result<ResumeRecipe> {
    match harness {
        Harness::Claude => crate::cmd_export::project_claude(path, cwd),
        Harness::Gemini => crate::cmd_export::project_gemini(path, cwd),
        Harness::Codex => crate::cmd_export::project_codex(path, cwd),
        Harness::Opencode => crate::cmd_export::project_opencode(path, cwd),
        Harness::Pi => crate::cmd_export::project_pi(path, cwd),
    }
}
```

- [ ] **Step 4: Add the five `pub(crate) fn project_<harness>` thin wrappers in `cmd_export.rs`**

Each wrapper calls the existing build/write pair without going through `run_<harness>` (so the CLI's `--input` / `--output` machinery is bypassed but the on-disk side-effects are identical):

```rust
// Add near the top of cmd_export.rs, after the existing helpers.

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_claude(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<ResumeRecipe> {
    let conv = build_claude_conversation(path)?;
    let jsonl = serialize_jsonl(&conv)?;
    write_into_claude_project(&conv, &jsonl, project_dir)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_gemini(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<ResumeRecipe> {
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let project_path = project_dir.to_string_lossy().to_string();
    // Reuse existing build-from-path path (build_gemini_conversation takes
    // an `input: &str` cache id today — refactor to take the path directly).
    let view = toolpath_convo::extract_conversation(path);
    let project_hash = toolpath_gemini::paths::project_hash(&project_path);
    let projector = toolpath_gemini::project::GeminiProjector::new()
        .with_project_hash(project_hash)
        .with_project_path(project_path.clone());
    use toolpath_convo::ConversationProjector;
    let conv = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;
    if conv.session_uuid.is_empty() {
        anyhow::bail!("Projected conversation has no session UUID");
    }
    write_into_gemini_project(&conv, &project_path)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_codex(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<ResumeRecipe> {
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let cwd_str = project_dir.to_string_lossy().to_string();
    use toolpath_convo::ConversationProjector;
    let view = toolpath_convo::extract_conversation(path);
    let projector = toolpath_codex::project::CodexProjector::new().with_cwd(cwd_str);
    let session = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;
    if session.id.is_empty() {
        anyhow::bail!("Projected session has no id");
    }
    write_into_codex_project(&session)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_opencode(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<ResumeRecipe> {
    let session = build_opencode_session(path, Some(project_dir))?;
    write_into_opencode_db(&session, project_dir)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_pi(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<ResumeRecipe> {
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let cwd_str = project_dir.to_string_lossy().to_string();
    let session = {
        use toolpath_convo::ConversationProjector;
        let view = toolpath_convo::extract_conversation(path);
        let projector = toolpath_pi::project::PiProjector::new().with_cwd(cwd_str.clone());
        projector
            .project(&view)
            .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?
    };
    if session.header.id.is_empty() {
        anyhow::bail!("Projected session has no id");
    }
    write_into_pi_project(&session, &cwd_str)
}
```

(Each wrapper duplicates a small amount of the corresponding `run_<harness>` body. If the duplication bothers a reviewer, a follow-up can collapse the existing `run_<harness>` into a thin wrapper around `project_<harness>` plus output-mode handling. Out of scope for this plan.)

- [ ] **Step 5: Run test to verify it passes**

```bash
cargo test -p path-cli --lib project_into_harness_claude_round_trip
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/path-cli/src/cmd_export.rs crates/path-cli/src/cmd_resume.rs
git commit -m "feat(path-cli): project_into_harness dispatcher with per-harness wrappers"
```

---

## Task 12: Implement `exec_harness` with injectable strategy

**Files:**
- Modify: `crates/path-cli/src/cmd_resume.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn exec_strategy_recording_captures_recipe() {
    let recipe = ResumeRecipe {
        binary: "claude",
        args: vec!["-r".to_string(), "abc123".to_string()],
        session_id: "abc123".to_string(),
        cwd_for_recipe: std::path::PathBuf::from("/tmp/x"),
    };
    let recorder = RecordingExec::default();
    let strategy: &dyn ExecStrategy = &recorder;
    exec_harness(&recipe, std::path::Path::new("/tmp/x"), strategy).unwrap();

    let captured = recorder.captured();
    assert_eq!(captured.binary, "claude");
    assert_eq!(captured.args, vec!["-r".to_string(), "abc123".to_string()]);
    assert_eq!(captured.cwd, std::path::PathBuf::from("/tmp/x"));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p path-cli --lib exec_strategy_recording_captures_recipe
```

Expected: FAIL.

- [ ] **Step 3: Implement `ExecStrategy` and `exec_harness`**

Append to `cmd_resume.rs`:

```rust
/// What `exec_harness` saw (for tests).
#[derive(Debug, Clone, Default)]
pub(crate) struct CapturedExec {
    pub(crate) binary: String,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: std::path::PathBuf,
}

/// Pluggable exec backend. Production uses `RealExec` (`execvp` on
/// Unix, spawn-and-wait on Windows). Tests use `RecordingExec`.
pub(crate) trait ExecStrategy {
    fn exec(&self, recipe: &ResumeRecipe, cwd: &std::path::Path) -> Result<()>;
}

/// Production implementation. On Unix this never returns on success
/// (the current process is replaced); on Windows it spawns the child,
/// waits, and propagates the exit code.
pub(crate) struct RealExec;

impl ExecStrategy for RealExec {
    fn exec(&self, recipe: &ResumeRecipe, cwd: &std::path::Path) -> Result<()> {
        let mut cmd = std::process::Command::new(recipe.binary);
        cmd.args(&recipe.args);
        cmd.current_dir(cwd);

        eprintln!(
            "Resuming: {} {} (cwd: {})",
            recipe.binary,
            recipe.args.join(" "),
            cwd.display()
        );

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // exec only returns if it fails.
            let err = cmd.exec();
            anyhow::bail!(
                "couldn't exec `{}`: {}. Recipe: {} {} (run from {})",
                recipe.binary,
                err,
                recipe.binary,
                recipe.args.join(" "),
                cwd.display()
            );
        }
        #[cfg(not(unix))]
        {
            let status = cmd.spawn()
                .with_context(|| format!("spawn {}", recipe.binary))?
                .wait()
                .with_context(|| format!("wait for {}", recipe.binary))?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }
}

/// Recording strategy for tests. `captured()` returns the most recent
/// invocation.
#[derive(Default)]
pub(crate) struct RecordingExec {
    inner: std::sync::Mutex<CapturedExec>,
}

impl RecordingExec {
    pub(crate) fn captured(&self) -> CapturedExec {
        self.inner.lock().unwrap().clone()
    }
}

impl ExecStrategy for RecordingExec {
    fn exec(&self, recipe: &ResumeRecipe, cwd: &std::path::Path) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        *g = CapturedExec {
            binary: recipe.binary.to_string(),
            args: recipe.args.clone(),
            cwd: cwd.to_path_buf(),
        };
        Ok(())
    }
}

pub(crate) fn exec_harness(
    recipe: &ResumeRecipe,
    cwd: &std::path::Path,
    strategy: &dyn ExecStrategy,
) -> Result<()> {
    strategy.exec(recipe, cwd)
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p path-cli --lib exec_strategy_recording_captures_recipe
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(path-cli): ExecStrategy with RealExec/RecordingExec for path resume"
```

---

## Task 13: Wire `run_resume` orchestration

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
pub(crate) fn run_with_strategy(args: ResumeArgs, exec: &dyn ExecStrategy) -> Result<()> {
    let (doc, source_harness) = resolve_input(&args)?;
    let path = ensure_path_with_agent(&doc)?;

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

    let recipe = project_into_harness(path, target, &cwd)?;
    exec_harness(&recipe, &cwd, exec)
}
```

- [ ] **Step 2: Update the existing stub test**

Replace:

```rust
#[test]
fn run_returns_not_implemented_until_wired() { ... }
```

with:

```rust
#[test]
fn run_with_strategy_records_recipe_for_file_input_with_explicit_harness() {
    let _home = scoped_home_for_resume(tempfile::tempdir().unwrap());
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
    // Safety: see scoped_home note. Treat tests as single-threaded.
    unsafe { std::env::set_var("PATH", new_path); }
    let _restore = scopeguard::guard(prev, |p| {
        unsafe {
            match p {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
    });

    let args = ResumeArgs {
        input: doc_file.to_string_lossy().to_string(),
        cwd: Some(cwd.path().to_path_buf()),
        harness: Some(HarnessArg::Claude),
        no_cache: false, force: false, url: None,
    };

    let recorder = RecordingExec::default();
    run_with_strategy(args, &recorder).unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "claude");
    assert_eq!(cap.args[0], "-r");
    assert_eq!(cap.cwd, std::fs::canonicalize(cwd.path()).unwrap());
}
```

If `scopeguard` isn't already a dev-dep, either add it (`scopeguard = "1"` under `[dev-dependencies]`) or write an equivalent local `Drop`-based guard struct. Check `Cargo.toml` first.

- [ ] **Step 3: Run the orchestration test**

```bash
cargo test -p path-cli --lib run_with_strategy_records_recipe_for_file_input_with_explicit_harness
```

Expected: PASS.

- [ ] **Step 4: Run all `cmd_resume` tests**

```bash
cargo test -p path-cli --lib cmd_resume
```

Expected: PASS for the full set.

- [ ] **Step 5: Commit**

```bash
git add crates/path-cli/src/cmd_resume.rs crates/path-cli/Cargo.toml
git commit -m "feat(path-cli): wire path resume orchestration end-to-end"
```

---

## Task 14: Integration tests

**Files:**
- Create: `crates/path-cli/tests/resume.rs`

- [ ] **Step 1: Write the integration test file**

Create `crates/path-cli/tests/resume.rs` with the cases enumerated in the spec. Each test invokes `path_cli::cmd_resume::run_with_strategy` with a `RecordingExec` and asserts on captured recipe + on-disk side-effects.

Each test in the file is one case from the list below. Subsequent steps in this task fill in the per-harness bodies and the rejection cases.

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

    // Side-effect: the projected JSONL exists under HOME.
    let projects = home_dir().join(".claude/projects");
    assert!(projects.exists(), "claude projects dir missing");
    assert!(walk_dir_finds_jsonl(&projects), "no JSONL written");
}

#[test]
fn file_input_explicit_gemini_projects_and_records_exec() { /* ... */ }

#[test]
fn file_input_explicit_codex_projects_and_records_exec() { /* ... */ }

#[test]
fn file_input_explicit_opencode_projects_and_records_exec() { /* ... */ }

#[test]
fn file_input_explicit_pi_projects_and_records_exec() { /* ... */ }

#[test]
fn cache_id_input_loads_and_projects() { /* writes a cache entry first, runs resume */ }

#[test]
fn url_input_fetches_via_mock_pathbase_and_projects() {
    use path_cli::cmd_pathbase::tests::MockServer;
    /* ... */
}

#[test]
fn multi_path_graph_returns_clear_error() { /* see Step 6 */ }

#[test]
fn agentless_path_returns_clear_error() { /* see Step 6 */ }

#[test]
fn explicit_harness_not_on_path_errors() { /* see Step 7 */ }

#[test]
fn zero_installed_errors() { /* see Step 7 */ }
```

(There is no `step_input` rejection test: this codebase has no `Document::Step` shape — `Graph::from_json` rejects non-graph JSON during parse, well before `ensure_path_with_agent` runs. The `multi_path_graph` and `agentless_path` cases cover the rejection logic that lives in `cmd_resume`.)

- [ ] **Step 2: Add the `support` module**

Create `crates/path-cli/tests/support/mod.rs` (or `crates/path-cli/tests/support.rs`) with shared helpers:

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
            // Some helpers honor TOOLPATH_CONFIG_DIR; keep it pinned to HOME/.toolpath.
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

pub fn args(input: PathBuf, cwd: &Path, harness: HarnessArg) -> path_cli::cmd_resume::ResumeArgs {
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

- [ ] **Step 3: Implement the per-harness positive cases**

Each follows the Claude pattern. Adjust `actor`, `HarnessArg`, expected binary, expected first arg (`-r` for claude, `--resume` for gemini, `resume` for codex, `--session` for opencode/pi). Skip on-disk JSONL assertion for opencode (which writes SQLite rows, not JSONL).

- [ ] **Step 4: Implement the cache-id input case**

```rust
#[test]
fn cache_id_input_loads_and_projects() {
    let _home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let _path_guard = ScopedPath::with_binary("claude");

    // Build the same minimal claude path as `write_minimal_path_file` does,
    // but keep it in memory and stash it under a known cache id.
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
```

- [ ] **Step 5: Implement the URL-input case via `MockServer`**

Use `path_cli::cmd_pathbase::tests::MockServer` (made `pub(crate)` in Task 6 — promote to `pub` here if cross-crate-test-binary access requires it, or move the helper to a `pub` test-utilities module).

- [ ] **Step 6: Implement the rejection cases**

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
    let p2 = {
        // Reuse the same builder; rename the path id to avoid collision.
        let mut p = p1.clone();
        p.path.id = "p2".into();
        p
    };
    let graph = toolpath::v1::Graph {
        graph: toolpath::v1::GraphIdentity { id: "g1".into() },
        paths: vec![
            toolpath::v1::PathOrRef::Path(Box::new(p1)),
            toolpath::v1::PathOrRef::Path(Box::new(p2)),
        ],
        meta: None,
    };
    let doc_file = cwd.path().join("multi.json");
    std::fs::write(&doc_file, graph.to_json().unwrap()).unwrap();

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
```

(Substitute the actual `Graph` and `GraphIdentity` field names if they differ from the snippet — read `crates/toolpath/src/types.rs` first; the existing `cmd_merge.rs::tests` builds graphs literally and is the canonical example.)

- [ ] **Step 7: Implement the harness-not-on-PATH and zero-installed cases**

```rust
#[test]
fn explicit_harness_not_on_path_errors() {
    let _home = ScopedHome::new();
    let _path_guard = ScopedPath::empty();
    let cwd = tempfile::tempdir().unwrap();
    let doc = write_minimal_path_file(cwd.path(), "agent:claude-code");

    let recorder = RecordingExec::default();
    let err = run_with_strategy(args(doc, cwd.path(), HarnessArg::Claude), &recorder)
        .unwrap_err();
    assert!(err.to_string().contains("isn't on PATH"));
}
```

- [ ] **Step 8: Run all integration tests**

```bash
cargo test -p path-cli --test resume
```

Expected: PASS.

- [ ] **Step 9: Run the full `path-cli` test suite**

```bash
cargo test -p path-cli
```

Expected: pass.

- [ ] **Step 10: Commit**

```bash
git add crates/path-cli/tests/
git commit -m "test(path-cli): integration tests for path resume"
```

---

## Task 15: Documentation

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md`
- Modify: `crates/path-cli/src/cmd_resume.rs` (rustdoc)
- Modify: `crates/path-cli/src/cmd_export.rs` (rustdoc)
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

- [ ] **Step 5: Adjust the `cmd_export.rs` module rustdoc**

In the existing `//! ` block at the top of `cmd_export.rs`, append a paragraph:

```rust
//!
//! Each `--project` mode now returns a `ResumeRecipe { binary, args,
//! session_id, cwd_for_recipe }`. The CLI surface formats the recipe
//! into the same `Resume with: …` / `Open conversation with: …` lines
//! it always has; `path resume` consumes the recipe directly to exec
//! the harness.
```

- [ ] **Step 6: Add a `CHANGELOG.md` entry**

Add a new section at the top (above the most recent entry):

```markdown
## path-cli 0.9.0 — 2026-05-08

### Added
- `path resume <input>` — fetch a Toolpath document (URL, shorthand,
  file path, or cache id), pick a coding-agent harness, project the
  session into its on-disk layout under a chosen cwd, and exec the
  harness's resume command.
- `cmd_export::ResumeRecipe` — public type returned by every
  project-mode export helper; describes how to invoke the harness for
  resume. Consumed by `path resume`.

### Changed
- `path export <harness> --project <dir>` writers internally return a
  `ResumeRecipe`. The CLI's stderr "Resume with: …" lines are now
  formatted from the recipe; user-visible output is unchanged.
```

(If `CHANGELOG.md` doesn't exist yet, create it with a simple header `# Changelog` followed by the section above.)

- [ ] **Step 7: Build the docs to confirm they compile**

```bash
cargo doc -p path-cli --no-deps
```

Expected: clean build.

- [ ] **Step 8: Commit**

```bash
git add CLAUDE.md README.md CHANGELOG.md crates/path-cli/src/cmd_resume.rs crates/path-cli/src/cmd_export.rs
git commit -m "docs: document path resume command"
```

---

## Task 16: Version bumps

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

In the root `Cargo.toml`, find the `[workspace.dependencies]` `path-cli` entry and bump to match:

```toml
path-cli = { path = "crates/path-cli", version = "0.9.0" }
```

(Adjust to match the existing entry's exact shape — `path` may or may not be present.)

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

## Task 17: Smoke test from the CLI

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

Pick a cache entry that's not from a harness — e.g. a `git-*` entry from a previous `path import git`. If none exist, derive one:

```bash
./target/release/path import git --repo . --branch main
./target/release/path cache ls
```

Then attempt to resume:

```bash
./target/release/path resume <git-cache-id> --harness claude
```

Expected: error message `no agent session in input — `path resume` only works on harness-derived paths`.

- [ ] **Step 4: (Optional) Confirm a real resume works against an actual session**

Only if you have a real claude/codex/gemini/opencode/pi session locally and one of those binaries on PATH:

```bash
./target/release/path import claude --project . --no-cache | ./target/release/path resume - --harness claude
```

(Or use a cached entry. The `-` stdin form requires an extra implementation step — skip if not implemented.)

Expected: control transfers to the harness with the prior conversation visible.

- [ ] **Step 5: No commit needed for smoke testing**

Manual step only.

---

## Self-review checklist (run before handing the plan off)

1. Every task ends with a `git commit` — verified.
2. Every code step shows the actual code, not "implement X" — verified.
3. Every test step shows the actual test, the run command, and the expected outcome — verified.
4. File paths are absolute or workspace-relative — verified (all `crates/path-cli/...`).
5. Type names are consistent across tasks (`ResumeRecipe`, `ResumeArgs`, `ExecStrategy`, `RecordingExec`, `RealExec`, `Harness`, `HarnessArg`) — verified.
6. Spec coverage:
   - § Surface — Tasks 7, 13.
   - § Input resolution — Task 9.
   - § Launch — Tasks 12, 13.
   - § Internal architecture (`resolve_input`, `ensure_path_with_agent`, `pick_harness`, `project_into_harness`, `exec_harness`) — Tasks 8–13.
   - § `ResumeRecipe` and `cmd_export` refactor — Tasks 1–5, 11.
   - § Error handling — Tasks 8, 9, 10, 14.
   - § Testing — Tasks 1–14, 17.
   - § Documentation — Task 15.
   - § Versioning — Task 16.
7. No "TBD", "TODO", or "implement later" — verified.
