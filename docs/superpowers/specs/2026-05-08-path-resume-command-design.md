# `path resume` — one-shot resume into a coding agent

**Status:** Design accepted, awaiting implementation plan.
**Date:** 2026-05-08

## Goal

Collapse the existing two-step "fetch a session, replay it locally"
workflow (`path import pathbase <ref>` then `path export <harness>
--input <id> --project <dir>` then run the harness's resume command)
into a single command that ends with the user's chosen coding agent
running in interactive mode against the projected session.

`path share` is the upstream of this flow: someone clicks share, sends
a Pathbase URL to a teammate, and the teammate runs `path resume <url>`
to land in claude / codex / etc. with the conversation in front of
them.

## Non-goals

- **Git context.** A Toolpath document may carry a `path.base` git URL
  + ref, but `path resume` does not clone, fetch, or check out anything.
  The user is responsible for their working tree. (Possible follow-up.)
- **File-artifact replay.** The doc may carry file changes; we do not
  apply them to the working tree. The harness session alone is what
  gets reconstructed.
- **Multi-path resume from a `Graph`.** v1 accepts a single `Path`;
  `Graph` inputs are rejected with a message.
- **Cross-harness fidelity warnings.** The user picks the harness; we
  do not second-guess matches/mismatches with the source.
- **`--print` opt-out for exec.** Default and only behavior is exec.
  Recipe-print is the *fallback* when exec fails (binary missing
  between PATH check and exec), not a user-facing flag.
- **Deprecation aliases.** Fresh command, no prior name to honor.

## Surface

```
path resume <input>
            [-C, --cwd <path>]
            [--harness <claude|gemini|codex|opencode|pi>]
            [--no-cache] [--force] [--url <url>]
```

| Flag / arg | Behavior |
| --- | --- |
| `<input>` | URL, Pathbase shorthand, file path, or cache id. See "Input resolution" below. |
| `-C, --cwd P` | chdir to P before projecting and before exec'ing the harness. Layout is keyed on P. Default: shell cwd. |
| `--harness X` | Pin the resume target. Skips the interactive picker. Errors if X is not on PATH. |
| (no `--harness`) | fzf picker over installed harnesses; doc's source harness pre-selected when installed. Rows annotated `(source)` and/or `(not on PATH)`. |
| `--no-cache` | URL/shorthand inputs only: skip writing the fetched doc to `~/.toolpath/documents/`. |
| `--force` | URL/shorthand inputs only: overwrite an existing cache entry. Same semantics as `import --force`. |
| `--url <url>` | Override Pathbase server URL. Same fallback chain as `import pathbase`: `--url` > stored session > `$PATHBASE_URL` > `https://pathbase.dev`. |

### Input resolution

The single `<input>` argument is resolved in this order, matching the
precedent set by `import pathbase`:

1. **URL** — starts with `http://` or `https://` → fetched via the
   pathbase client, written to cache (unless `--no-cache`), parsed.
2. **Pathbase shorthand** — three slash-separated segments
   (`owner/repo/slug`) → same fetch + cache flow.
3. **Existing file path** — resolves as a real file on disk → read and
   parsed.
4. **Cache id** — falls back to `~/.toolpath/documents/<input>.json`
   via the existing `cache_ref` helper.

Ambiguity (e.g. a string that looks like a shorthand *and* is a real
file) resolves in the order above. This matches `import pathbase` and
is documented in the error path: a fail-to-resolve message names all
four shapes.

### Launch

After projection completes, the command:
1. `chdir`s to the resolved cwd (whether default or `-C P`).
2. **Unix:** `execvp`s the harness binary with its resume args, replacing
   the current process.
3. **Windows:** `spawn`s the harness, waits, propagates the exit code.

If the binary is not on PATH at exec time (race between the picker's
PATH check and exec, or a `--harness` value that fails the validation
gate), exit non-zero with `couldn't exec <binary>: <err>. Recipe:
<binary> <args> (run from <cwd>)` so the user can recover by hand.

## Internal architecture

### New module: `cmd_resume.rs`

Lives next to the other `cmd_*.rs` files in `crates/path-cli/src/`.
Wired into `lib.rs` as a new `Commands::Resume { args:
cmd_resume::ResumeArgs }` arm. Same pattern as `cmd_share.rs`.

### Top-level orchestration

```rust
pub async fn run_resume(args: ResumeArgs) -> Result<()> {
    let (doc, source_harness) = resolve_input(&args).await?;
    ensure_path_with_agent(&doc)?;

    let cwd = args.cwd.map(canonicalize_existing)
                 .unwrap_or_else(|| std::env::current_dir())?;

    let target = pick_harness(args.harness, source_harness)?;
    let session_id = project_into_harness(&doc, target, &cwd)?;
    let argv = argv_for(target, &session_id);
    exec_harness(target.binary_name(), &argv, &cwd)
}
```

### `resolve_input`

Small dispatcher that delegates, in order:

- URL / `owner/repo/slug` → factor out the existing pathbase fetch
  flow that lives inline in `cmd_import.rs` (calls
  `cmd_pathbase::paths_download` for the body, then `cache::write_cached`
  unless `--no-cache`) into a small `pub(crate)` helper that returns
  `(Graph, String /* cache_id */)`. `cmd_resume` calls it; `cmd_import`'s
  pathbase branch keeps using it. Honors `--no-cache`, `--force`, `--url`.
- File path / cache id → `cmd_cache::cache_ref` then read+parse.

Returns `(Graph, Option<Harness>)` — there is no `Document` enum in
the codebase; `Graph::from_json` is the universal parse entry. The
source harness is read from the single inline path's `meta.source` —
set by `toolpath-convo::derive_path` to the provider's `provider_id`:

| `meta.source` | Harness |
| --- | --- |
| `"claude-code"` | Claude |
| `"gemini-cli"` | Gemini |
| `"codex"` | Codex |
| `"opencode"` | Opencode |
| `"pi"` | Pi |

Fallback when `meta.source` is absent: actor-string prefix sniffing
across `path.steps[*].actor` (`agent:claude-code`, `agent:codex`,
`agent:gemini-cli`, `agent:opencode`, `agent:pi`). `None` when neither
source is conclusive — the picker still works, just without a
pre-selection.

### `ensure_path_with_agent`

Pure validation operating on a `Graph`. Rejects:

- Empty graph → "resume needs a `Path`; expected one path, got an
  empty graph".
- Graph with more than one inline path → "resume needs a single
  `Path`; `<input>` is a graph with N paths. Pick one with
  `path query …` or split first."
- Single-path graph whose steps contain zero `agent:*` actors → "no
  agent session in `<input>` — `path resume` only works on
  harness-derived paths".
- Single-path graph whose only entry is a `$ref` (not an inline
  path) → "resume needs an inline `Path`; got a $ref. Resolve it
  first."

A bare `Step` JSON document never reaches this function — it would
fail `Graph::from_json` parse. No dedicated rejection branch needed.

### `pick_harness`

Reuses the `Harness` enum from `cmd_share.rs` (Claude / Gemini /
Codex / Opencode / Pi), including its `binary_name()` helper. Logic:

- If `args.harness` is set → validate the binary is on PATH (small
  inline `$PATH`-walking helper; or pull in the `which` crate as a new
  dep — pick at implementation time, the surface is the same), return
  it. Error if not on PATH.
- Else build the installed list (probe each harness binary on PATH);
  pre-select source if installed; fzf-prompt with annotations.
  Picker header: `pick a harness to resume in (source: <name>)` when
  source is known, otherwise `pick a harness to resume in`.
- If zero harnesses are installed → error naming all five.
- Esc / Ctrl-C → exit 130 (matches `path share`).

Non-TTY environment with no `--harness`: error with the recipe (no
silent default — picking is consequential).

### `project_into_harness`

Each `run_<harness>` in `cmd_export.rs` is already split into two
private helpers:

- `build_<harness>_<x>(...)` — projects a `Path` into the harness's
  in-memory session struct (which carries a stable `session_id` field).
- `write_into_<harness>_project(...)` — writes that struct to disk.

We add five thin `pub(crate)` wrappers in `cmd_export.rs`:

```rust
pub(crate) fn project_claude(path: &Path, project_dir: &Path) -> Result<String>;
pub(crate) fn project_gemini(path: &Path, project_dir: &Path) -> Result<String>;
pub(crate) fn project_codex(path: &Path, project_dir: &Path) -> Result<String>;
pub(crate) fn project_opencode(path: &Path, project_dir: &Path) -> Result<String>;
pub(crate) fn project_pi(path: &Path, project_dir: &Path) -> Result<String>;
```

Each composes its build + write pair, returning the projected
session id. cmd_resume's `project_into_harness` is a five-arm match
that dispatches to the right wrapper.

No public type, no refactor of the existing private writers, and no
change to `path export <harness>`'s user-visible behavior.

### `argv_for`

```rust
fn argv_for(harness: Harness, session_id: &str) -> Vec<String> {
    match harness {
        Harness::Claude   => vec!["-r".into(), session_id.into()],
        Harness::Gemini   => vec!["--resume".into(), session_id.into()],
        Harness::Codex    => vec!["resume".into(), session_id.into()],
        Harness::Opencode => vec!["--session".into(), session_id.into()],
        Harness::Pi       => vec!["--session".into(), session_id.into()],
    }
}
```

A static map from harness to resume-argv shape. Lives in
`cmd_resume.rs` because it's a per-harness CLI convention, not a
projection concern. `Harness::binary_name()` already exists in
`cmd_share.rs` and supplies the program name.

### `exec_harness`

Unix:

```rust
use std::os::unix::process::CommandExt;

let err = std::process::Command::new(binary)
    .args(args)
    .current_dir(cwd)
    .exec();          // returns std::io::Error on failure only
```

Windows: `Command::new(...).spawn()?.wait()?`, propagate exit code.
Both paths fall through to the recipe-print fallback (§ Launch) on
spawn/exec error.

### Wiring

One new arm in `lib.rs`'s dispatch match alongside `Commands::Share`.
The fzf wrapper (`crate::fzf`) and `cmd_share::Harness` are already in
`path-cli`. The only candidate new dep is `which` for PATH probing —
optional; a 15-line homegrown helper does the same job.

## Output contract

- **stdout**: nothing under normal exec. The harness owns the TTY
  after exec. (On the recipe-print fallback path, the recipe goes to
  stderr.)
- **stderr**: progress messages —
  ```
  Resolved <input> → claude-abc        (cache id; omitted with --no-cache)
  Picked harness: claude (source)
  Projected → ~/.claude/projects/<sanitized>/<id>.jsonl
  Resuming: claude -r <id> (cwd: <cwd>)
  ```
  Last line printed immediately before exec.

**Exit codes.** Unix exec succeeds → process replaced; the harness's
exit code is what the caller sees. Windows / recipe-print fallback /
errors → propagate. Picker cancel → 130. Validation errors → 1.

## Error handling

| Situation | Behavior |
| --- | --- |
| URL fetch fails (network) | Propagated from `pathbase-client`. |
| URL fetch returns 401/403 | "auth failed for `<url>`; run `path auth login` or pass `--anon`" (mirrors `import pathbase`). |
| Cache hit on URL fetch, no `--force` | "cache entry `<id>` already exists; pass `--force` to overwrite". |
| Input doesn't resolve as URL / shorthand / file / cache id | "couldn't resolve `<input>` as a URL, file path, or cache id". |
| Empty graph | "resume needs a `Path`; expected one path, got an empty graph". |
| Multi-path graph | "resume needs a single `Path`; `<input>` is a graph with N paths. Pick one with `path query …` or split first." |
| Single-path graph with no `agent:*` actors | "no agent session in `<input>` — `path resume` only works on harness-derived paths". |
| Single-path graph entry is a `$ref` | "resume needs an inline `Path`; got a $ref. Resolve it first." |
| `--harness X` given, X not on PATH | "harness `<x>` isn't on PATH; install it or pick another with `--harness`". |
| Zero harnesses on PATH (interactive mode) | "no installed harnesses found; install one of: claude, gemini, codex, opencode, pi". |
| No `--harness` and stderr/stdin not a TTY | "interactive picker requires a TTY; pass `--harness <X>` or rerun in a terminal". |
| Picker cancelled (Esc / Ctrl-C) | Silent; exit 130. |
| Projection fails mid-write | Propagated from `cmd_export`; partial files left behind (same as `export <harness> --project`). |
| `exec` fails (binary disappeared between PATH check and exec) | Print recipe to stderr with `couldn't exec`; exit non-zero. |

Notes that drive design but not behavior:

- All "couldn't" messages start lowercase to match the style elsewhere
  in `path-cli`.
- We do not validate that `cwd` is a git repo. The harnesses don't
  require it; we shouldn't either.
- We do not warn if the recorded cwd in the doc (codex/opencode)
  differs from `--cwd`. The user's flag wins; their problem to know
  what they're doing.

## Testing

### Unit tests in `cmd_resume.rs`

1. `resolve_input` dispatch — URL detection (`https://`), shorthand
   detection (three-segment), file-path detection, cache-id fallback.
   Each branch tested against a tmpdir + mock cache.
2. `infer_source_harness` — `meta.source` tag wins; actor-string
   sniffing fallback; `None` when neither is conclusive.
3. `ensure_path_with_agent` — accepts `Path` with at least one
   `agent:*` step; rejects `Step` / `Graph` / agent-less `Path` with
   the exact error strings from "Error handling".
4. `pick_harness` non-interactive paths — `--harness` set + on PATH →
   returns it; `--harness` set + not on PATH → error; zero installed
   → error. PATH membership is faked via an injectable lookup helper.

### `project_<harness>` round-trip in `cmd_export.rs` tests

One test per harness (claude / gemini / codex / opencode / pi):
project a fixture path, assert the returned `session_id` is
non-empty and the on-disk side-effects landed (the `.jsonl` exists,
the SQLite row was inserted, etc.).

### Integration tests in `crates/path-cli/tests/resume.rs`

Exec is the one untestable line. `cmd_resume` accepts an injectable
"exec strategy" (a small trait object or boxed closure) — the binary
calls the real `execvp` strategy; tests substitute a strategy that
records `(binary, args, cwd)` and returns success. No public
`--dry-run` flag.

Cases:

1. File-path input + `--harness claude` + `-C <tmp>` → projects under
   `<tmp>/.claude/projects/<sanitized>/<id>.jsonl`; recorded
   `(binary, args)` is `("claude", ["-r", <id>])`.
2. Same shape, one per harness (gemini / codex / opencode / pi).
3. Cache-id input → loads from a tmp cache, projects, records
   `(binary, args, cwd)`.
4. URL input → reuses the in-repo `MockServer` test helper from
   `cmd_pathbase.rs`'s test module (extract into a `pub(crate)` test
   util if needed), fetches, caches, projects.
5. Multi-path graph → returns the error verbatim.
6. Agent-less `Path` (git-derived fixture) → returns the error.
7. `--harness` not on PATH → error.
8. Zero installed harnesses → error.
9. Picker cancel → exit 130 (reuses the existing fzf-cancel test
   pattern from `cmd_share`).

### Out of scope for tests

- Real harness exec. Not exercised in CI.
- The fzf-driven harness picker UX. The picker code is small and
  reuses `cmd_share`'s helpers, which are already covered.

## Documentation

- `CLAUDE.md` — add `path resume` to the CLI usage list (next to `path
  share`); add a "Things to know" bullet describing the
  resolve→pick→project→exec flow and `-C` semantics.
- `README.md` — one-line mention in the workspace listing.
- `crates/path-cli/src/cmd_resume.rs` — module-level rustdoc covering
  inputs, resolution order, harness picker, and exec semantics. Same
  density as the doc comment at the top of `cmd_share.rs` and
  `cmd_export.rs`.
- `cmd_export.rs` — no rustdoc change required; the new
  `project_<harness>` wrappers carry their own doc comments.
- Site (`site/`) — no new page; `path resume` gets one bullet wherever
  the CLI surface is enumerated.

## Versioning

- `path-cli` minor bump (additive command). Update
  `crates/path-cli/Cargo.toml`, `[workspace.dependencies]` in root
  `Cargo.toml`, `site/_data/crates.json`, and add a `CHANGELOG.md`
  entry.
- `toolpath-cli` shim follows along (no version bump needed).
- No bumps for the `toolpath-*` provider crates.

## Open questions

None blocking. Future:

- A git-aware mode that, given a doc with a `path.base`, offers to
  clone/fetch and check out the recorded ref before projection. Would
  need its own scope discussion.
- File-artifact replay onto the working tree, gated behind an explicit
  flag because of clobber risk.
- Multi-path resume from a `Graph` (interactive sub-pick or a
  `--path-id` flag).
- A `--browse` flag that, instead of exec'ing the harness, opens the
  doc in the desktop `pathbase-app` if installed.
