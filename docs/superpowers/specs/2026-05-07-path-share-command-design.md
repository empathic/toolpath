# `path share` — interactive Pathbase upload

**Status:** Design accepted, awaiting implementation plan.
**Date:** 2026-05-07

## Goal

Collapse the existing two-step "derive a session, upload it" workflow
(`path import <harness>` then `path export pathbase --input <id>`) into a
single command that's optimised for the most common case: an
interactive user wants to share *one* agent session from the project
they're currently sitting in.

Today this requires two commands and the user has to know which
harness ran the conversation. `path share` removes both of those.

## Non-goals

- Sharing git branches or GitHub PRs. Those flows already exist on
  `path import` / `path export pathbase` and the user explicitly
  scoped this command to "agent harnesses".
- Multi-session bundling. Multi-select is not exposed; one share, one
  URL.
- Streaming uploads. The document is materialized in memory (and, by
  default, in the cache) before posting.
- A `--include-thinking` flag for Gemini. Out of scope for v1.

## Surface

```
path share [--url <url>]
           [--harness <claude|gemini|codex|opencode|pi>]
           [--session <id>]
           [--project <path>]
           [--anon] [--repo <owner/name>] [--slug <s>] [--public]
           [--force] [--no-cache]
```

| Flag                  | Behavior |
| --------------------- | -------- |
| (no flags)            | Unified picker over all detected harnesses, current-project rows ranked first. |
| `--harness X`         | Pre-filter the picker to one harness. |
| `--harness X --session Y` | Skip the picker. `--project` required when X ∈ {claude, gemini, pi}. |
| `--session` alone     | Error: ambiguous without `--harness`. |
| `--project P`         | Filter the picker to sessions tied to that project (across all harnesses). |
| `--no-cache`          | Skip writing `~/.toolpath/documents/<id>.json`; derive in-memory only. |
| `--force`             | Allow overwriting an existing cache entry. Same semantics as `import --force`. |
| `--url`               | Override Pathbase server URL. Falls back to stored session, then `$PATHBASE_URL`, then `https://pathbase.dev`. |
| `--anon`              | Force anonymous endpoint; conflicts with `--repo`, `--public`. |
| `--repo`, `--slug`, `--public` | Same semantics as `path export pathbase`. |

When the user is logged out and passes none of `--anon` / `--repo` /
`--public` / `--slug`, the upload falls through to the anonymous
endpoint with a stderr notice — same default as `export pathbase`
today.

## Internal architecture

### New module: `cmd_share.rs`

Lives next to the other `cmd_*.rs` files in `crates/path-cli/src/`.
Wired into `lib.rs` as a new `Commands::Share { args: cmd_share::ShareArgs }`
arm.

### Session aggregation

```rust
struct SessionRow {
    harness: Harness,                // Claude | Gemini | Codex | Opencode | Pi
    project: Option<String>,         // project path for keyed providers; None for codex/opencode
    cwd: Option<String>,             // recorded cwd from the session (codex/opencode only)
    session_id: String,
    title: String,                   // first_user_message or "(no prompt)"
    last_activity: Option<DateTime<Utc>>,
    message_count: usize,
    matches_cwd: bool,               // computed during aggregation
}

fn gather_sessions(
    cwd: &Path,
    harness_filter: Option<Harness>,
    project_filter: Option<&Path>,
) -> Vec<SessionRow>;
```

**Detection-by-probing.** No explicit "is X installed" config. For
each of the five harnesses, instantiate `*Convo::new()` and attempt
the listing API. Skip silently when:
- `home_dir()` resolves to None,
- the harness's base directory does not exist,
- listing returns Err with `io::ErrorKind::NotFound`,
- listing returns an empty `Vec`.

Any other error emits a single `warning: <harness> aggregation failed: <err>`
to stderr and aggregation continues with the remaining harnesses.

**Per-harness rules:**

- **claude / gemini / pi** (project-keyed): `list_projects()` →
  `list_conversation_metadata(p)` for each. `matches_cwd =
  canonical(p) == canonical(cwd)`. Title from `first_user_message`.
- **codex / opencode** (session-keyed): `list_sessions()`. Codex
  stores `cwd` in rollout meta; opencode stores `directory`.
  `matches_cwd = canonical(stored_cwd) == canonical(cwd)`. No
  `project` field.

`canonicalize` failure on either side falls back to byte-equal string
compare; mismatch only affects ranking, never correctness.

### Picker

When `--session` is absent and stdin+stderr are TTYs and `fzf` is on
PATH, the rows are formatted into a TSV stream and fed to `fzf`:

```
col 1: harness          (hidden, parser key)
col 2: project_or_cwd   (hidden, derive arg)
col 3: session_id       (hidden, derive arg)
col 4: harness symbol   ("claude " / "gemini " / "codex  " / "opencode" / "pi    ")
col 5: when             ("YYYY-MM-DD HH:MM" or "—")
col 6: msgs             ("12 msgs")
col 7: scope            ("·" for cwd-match, " " otherwise)
col 8: project_short    (last two path segments)
col 9: title            (truncated to 120 chars)
```

`fzf` shows columns 4..; preview command runs
`path show <harness> [--project {2}] --session {3}`. Single-select
only (no `--multi`). Header line: `share an agent session (Enter = upload to <pathbase-host>)`.

**Sort order before piping to fzf:**
1. Rows with `matches_cwd = true`, descending by `last_activity`.
2. Rows with `matches_cwd = false`, descending by `last_activity`.

### Non-interactive paths

- `fzf` missing or non-TTY: print a generic recipe (use `path import
  <harness>` then `path export pathbase`) and exit 1. **No
  most-recent fallback** — sharing is consequential enough to require
  an explicit choice.
- `--harness X --session Y` (and `--project P` for keyed providers):
  skip aggregation entirely; derive directly.
- `--harness X` alone: still uses the unified aggregator pre-filtered
  to one harness; same fzf code path.
- Esc / Ctrl-C in fzf: exit 130, print nothing.

### Derive

Three small `pub(crate)` cuts in `cmd_import.rs`:

```rust
pub(crate) struct DerivedDoc { pub cache_id: String, pub doc: Graph }

pub(crate) fn derive_claude_pair(project: &str, session: &str) -> Result<DerivedDoc>;
pub(crate) fn derive_gemini_pair(project: &str, session: &str, include_thinking: bool) -> Result<DerivedDoc>;
pub(crate) fn derive_pi_pair(project: &str, session: &str, base: Option<PathBuf>) -> Result<DerivedDoc>;
pub(crate) fn derive_codex_one(session: &str) -> Result<DerivedDoc>;
pub(crate) fn derive_opencode_one(session: &str, no_snapshot_diffs: bool) -> Result<DerivedDoc>;
```

These extract the single-pair branches from the existing
`derive_claude` / `derive_gemini` / etc. dispatch functions in
`cmd_import.rs`. The existing dispatch keeps calling them — pure
mechanical refactor, no behavior change.

### Cache

Default behavior: write the derived `Graph` to
`~/.toolpath/documents/<cache_id>.json` via the existing
`write_cached(&id, &doc, force)`. Same `<source>-<inner-id>` cache id
format as `path import` — a `share`-produced entry is
indistinguishable from an `import`-produced one and can be re-uploaded
later with `export pathbase --input <id>`. `--no-cache` skips the
write. `--force` allows overwrite.

### Upload

`cmd_export::run_pathbase` is split:

```rust
pub(crate) fn run_pathbase_inner(args: PathbaseExportArgs, body: &str) -> Result<UploadResult>;
```

`run_pathbase` becomes a thin wrapper that reads the cache file then
calls the inner. `cmd_share` calls the inner directly with the
in-memory body (`doc.to_json()`). Same `--anon` / `--repo` / `--slug`
/ `--public` / `--url` semantics inherited from `export pathbase`.

`UploadResult` carries the share URL and a short summary string for
stderr.

## Output contract

- **stdout**: the share URL, exactly one line. Scriptable.
- **stderr**: progress messages —
  ```
  Picked claude session "Add share command"
  Imported claude session → claude-abc          (omitted with --no-cache)
  Uploaded → alex/pathstash/<slug> (secret path, 12 KB)
  ```
- The cache id is **not** echoed to stdout (unlike `path import`)
  because the share URL is the primary product. The cache id appears
  in the stderr "Imported …" line, which is enough to find it via
  `cache ls`.

**Exit codes.** 0 success; 130 user cancelled fzf; 1 anything else.

## Error handling

| Situation | Behavior |
| --- | --- |
| `home_dir()` None / harness base dir missing | Skip silently. |
| Per-file metadata read fails inside a harness | Underlying provider already handles this per-file; we don't second-guess. |
| Whole-harness listing returns Err other than NotFound | Single `warning: ...` to stderr; continue with other harnesses. |
| No sessions found anywhere | Print probe summary (one line per harness, with path and count or "not found"); exit 1. |
| No sessions match `--project P` | Print message naming the project; suggest running without `--project`; exit 1. |
| `--session` without `--harness` | Clap-level error (clap `requires = "harness"`). |
| `--anon` with `--repo`/`--public` | Clap-level conflict (copy from `export pathbase`). |
| `--harness <keyed>` + `--session` without `--project` | Runtime error: `"--project required when --harness is claude/gemini/pi and --session is set"`. |
| Logged out, no `--anon`, no auth-requiring flags | Anonymous upload with stderr notice (matches `export pathbase`). |
| Logged out, `--repo`/`--public`/`--slug` set | Error: "log in first" (inherited from `export pathbase`). |
| Logged in, `--url` host differs from stored session host | stderr warning, attempt anyway (inherited from `export pathbase`). |
| Server applies different `is_public` than requested | stderr note; share URL form follows what was actually applied (inherited). |

## Testing

### Unit tests in `cmd_share.rs`

1. `gather_sessions` produces rows in the right order (cwd-match first, then by recency) — fixture builds tempdir layouts for two or three harnesses.
2. `gather_sessions` skips harnesses whose home dir is missing (no panic, no warning).
3. `gather_sessions` honors `--harness` and `--project` filters.
4. `parse_picker_row` round-trips `(harness, project, session_id)` through the TSV format.
5. `matches_cwd` uses canonicalized paths (test via temp-dir symlink that both forms match).

Reuses the existing `setup_claude_manager` / `setup_claude_manager_with_two_sessions` helpers from `cmd_import` tests; adds a `setup_multi_harness` helper that wires two or three fake home dirs at once.

### Integration tests in `crates/path-cli/tests/`

1. `share_explicit_args.rs` — `path share --harness claude --project /tmp/x --session abc --no-cache --anon --url http://127.0.0.1:<port>` against the existing `MockServer`. Asserts a single URL on stdout and that the request body is the derived Graph.
2. `share_no_harness_no_tty.rs` — non-TTY, no flags → exits 1 with the recipe message; nothing on stdout.
3. `share_filters_by_project.rs` — explicit `--project P` with no matches → exits 1 with the per-project not-found message.
4. `share_logged_out_anon_default.rs` — no credentials, no `--anon` → uploads via anon endpoint; stderr carries the "not logged in — uploading anonymously" notice.
5. `share_writes_cache_by_default.rs` — default behavior, explicit args → a file appears in the test config dir's `documents/` matching the derived cache id.
6. `share_no_cache_skips_write.rs` — same with `--no-cache` → no file appears.

### Out of scope for tests

- The fzf-driven path. Not exercised in CI (matches the existing import tests). The aggregator — the genuinely new logic — is fully unit-tested.

### Documentation

- A one-line entry in `CLAUDE.md`'s "Things to know" pointing at
  `path share` and the unified-picker behavior, alongside the existing
  fzf picker docs.
- A short paragraph in the CLI usage block at the top of `CLAUDE.md`.
- A `path share` section in any place README/CLI docs enumerate
  commands.

## Open questions

None blocking. Future:
- `--include-thinking` could be added if Gemini sharing is common.
- Multi-select bundling could be added later if a user pattern emerges.
- A `--web` flag (or `path share --open`) that opens the resulting
  URL via `open` / `xdg-open` is a small future addition.
