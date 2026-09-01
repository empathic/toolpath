# CLAUDE.md

## What is this project?

Toolpath is a format for artifact transformation provenance. It records who changed what, why, what they tried that didn't work, and how to verify all of it. Think "git blame, but for everything that happens to code, including the stuff git doesn't see."

Three core objects: **Step** (a single change), **Path** (a sequence of steps, e.g. a PR), **Graph** (a collection of paths, e.g. a release). Steps form a DAG via parent references. Dead ends are implicit -- steps not on the ancestry of `path.head`.

## Repository layout

```
Cargo.toml                      # workspace root (edition 2024, resolver 2)
crates/
  toolpath/                     # core types, builders, serde, query API
  toolpath-convo/               # provider-agnostic conversation types, traits, and ConversationView -> Path derivation
  toolpath-git/                 # derive from git repos (git2)
  toolpath-github/              # derive from GitHub pull requests (REST API)
  toolpath-claude/              # derive from Claude conversation logs
  toolpath-gemini/              # derive from Gemini CLI conversation logs
  toolpath-codex/               # derive from Codex CLI rollout files
  toolpath-copilot/             # derive from + project to GitHub Copilot CLI session logs (preview)
  toolpath-opencode/            # derive from opencode SQLite databases
  toolpath-cursor/              # derive from Cursor (IDE) state.vscdb bubble store
  toolpath-pi/                  # derive from Pi (pi.dev) agent session logs
  toolpath-dot/                 # Graphviz DOT rendering
  toolpath-md/                  # Markdown rendering for LLM consumption
  path-cli/                     # unified CLI (binary: path)
  toolpath-cli/                 # deprecated shim that re-exports path-cli (excluded from the workspace; see below)
  pathbase-client/              # progenitor-derived client for the Pathbase HTTP API
                                # (spec at crates/pathbase-client/openapi.json; refresh via scripts/refresh-pathbase-openapi.sh)
.claude-plugin/
  marketplace.json              # Claude Code plugin marketplace (add with `/plugin marketplace add empathic/toolpath`)
plugins/
  claude-code/                  # Claude Code plugin "path": /path:share + /path:query, bundles the CLI
docs/agents/formats/            # format references for the agent on-disk formats we derive from
schema/toolpath.schema.json     # JSON Schema for the toolpath format
examples/*.json                 # example documents (step, path, graph)
RFC.md                          # full format specification
FAQ.md                          # design rationale, FAQ, and open questions
```

## Dependency graph

```
path-cli (binary: path)
 ├── toolpath           (core types)
 ├── toolpath-convo   → toolpath (conversation abstraction + shared derivation)
 ├── toolpath-git     → toolpath
 ├── toolpath-github  → toolpath
 ├── toolpath-claude  → toolpath, toolpath-convo
 ├── toolpath-gemini  → toolpath, toolpath-convo
 ├── toolpath-codex   → toolpath, toolpath-convo
 ├── toolpath-copilot → toolpath, toolpath-convo  (preview)
 ├── toolpath-opencode → toolpath, toolpath-convo
 ├── toolpath-cursor  → toolpath, toolpath-convo
 ├── toolpath-pi      → toolpath, toolpath-convo
 ├── toolpath-dot     → toolpath
 └── toolpath-md      → toolpath

pathbase-client      (no toolpath deps; built from schema/pathbase-openapi.json)

toolpath-cli (deprecated shim, binary: path)
 └── path-cli
```

## Build and test

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Requires Rust 1.85+ (edition 2024). Pinned to 1.94.0 via `rust-toolchain.toml`.

If `cargo` is not on your PATH, `flake.nix` carries a devShell with everything the
justfile and `scripts/quality_gates.sh` assume — the Rust toolchain plus shellcheck,
node/pnpm, jq, curl and fzf, with openssl wired up for `openssl-sys`:

```bash
nix develop                                   # or: nix develop --command <cmd>
nix develop --command ./scripts/quality_gates.sh
```

The shell's Rust comes from nixpkgs and is **ahead of** the 1.94.0 pin — `rust-toolchain.toml`
is read by rustup, which the shell does not provide. Clippy gains lints between releases, so
green in the shell is evidence, not proof; the pinned toolchain is the real gate.

## CLI usage

The binary is called `path` (package: `path-cli`; the older `toolpath-cli` package is a deprecated shim that still installs the same binary for users running `cargo install toolpath-cli`).

The top-level surface is the porcelain (`show`, `share`, `resume`, `query`, `kind`, `auth`, `config`, `haiku`). Lower-level building blocks live under `path p …` (plumbing): `p list`, `p import`, `p export`, `p cache`, `p render`, `p merge`, `p validate`, `p derive`, `p project`, `p incept`, `p track`, `p query` (graph traversal: `ancestors`). The old top-level spellings of the plumbing commands were removed in 0.10.0 — no alias, no shim.

```bash
# Plumbing: import from external formats into the local toolpath cache
# (~/.toolpath/documents/). claude/gemini/pi are project-keyed (--project),
# codex/opencode/cursor are session-keyed (--session).
cargo run -p path-cli -- p import git --repo . --branch main
cargo run -p path-cli -- p import github https://github.com/owner/repo/pull/42
cargo run -p path-cli -- p import claude --project /path/to/project
cargo run -p path-cli -- p import codex --session <uuid>
cargo run -p path-cli -- p import pathbase <pathbase-url-or-owner/repo/slug>
cargo run -p path-cli -- p import claude --project . --no-cache | path p render md --input -

# Share an agent session to Pathbase (interactive picker, single-shot)
cargo run -p path-cli -- share
cargo run -p path-cli -- share --harness claude --session <session-id> --project /path/to/project

# Resume a Toolpath document into a coding agent (interactive harness picker)
cargo run -p path-cli -- resume <pathbase-url-or-shorthand-or-file-or-cache-id>
cargo run -p path-cli -- resume <input> --harness claude -C /path/to/project

# Plumbing: export to external formats. <ref> is a cache id or a file path;
# --project writes into the harness's real on-disk store, --output to a file.
cargo run -p path-cli -- p export claude --input <ref> --project /tmp/sandbox
cargo run -p path-cli -- p export pathbase --input <ref>

# Plumbing: manage the cache
cargo run -p path-cli -- p cache ls
cargo run -p path-cli -- p cache rm <cache-id>
cargo run -p path-cli -- p cache sync                # ingest new/changed sessions from every harness
cargo run -p path-cli -- p cache sync claude codex --project-under ~/work/proj  # scope by type and project subtree

# Inspect / analyze
cargo run -p path-cli -- p render dot --input doc.json
cargo run -p path-cli -- p render md --input doc.json --detail full
# Query the whole local cache with a jaq (jq) filter over wrapped steps.
# Queries auto-sync their scope first; --no-sync opts out.
cargo run -p path-cli -- query 'map(select(.dead_end)) | map(.step.id)'
cargo run -p path-cli -- query --source claude 'map(select(any(.change[].structural.token_usage; .input_tokens > 50000)))'
cargo run -p path-cli -- query --input doc.json 'map(select(.step.actor | startswith("agent:")))'
cargo run -p path-cli -- kind agent-coding-session     # print a kind's schema; bare `kind` lists them
cargo run -p path-cli -- p query ancestors --input doc.json --step-id step-003
cargo run -p path-cli -- p merge doc1.json doc2.json --title "Combined"
cargo run -p path-cli -- p list claude --format tsv  # any provider; tsv is one session per line, fzf-friendly
cargo run -p path-cli -- show claude --project /path/to/project --session <session-id>  # markdown summary; used by fzf preview
cargo run -p path-cli -- p track init --file src/main.rs --actor "human:alex"
cargo run -p path-cli -- p validate --input doc.json
cargo run -p path-cli -- auth login   # also: status, whoami, logout
cargo run -p path-cli -- config edit  # $VISUAL/$EDITOR on ~/.toolpath/config.toml, validated after
```

The **cache** at `~/.toolpath/documents/<cache-id>.json` is the single landing zone for every `import` (and for `import pathbase` downloads). Cache id is `<source>-<inner-id>` — e.g. `claude-abc123`, `git-main` (Pathbase paths key on `<owner>-<repo>-<slug>`, anon paths on `anon-pathstash-<uuid>`). Files are `0600`, parent directory `0700`. `$TOOLPATH_CONFIG_DIR` overrides the root. Imports error on cache hit (`--force` overwrites); `--no-cache` sends the JSON to stdout for shell composition. `p cache sync` fills the cache incrementally from the installed agent harnesses (see "Things to know") and always overwrites what it re-derives.

`path auth login` prints `<base>/auth/cli`; the user logs in there and pastes the 8-character code back, which the CLI redeems (`POST /api/v1/auth/cli/redeem`) for a bearer token stored at `~/.toolpath/credentials.json` (`0600`; `$TOOLPATH_CONFIG_DIR` overrides). Server URL comes from `--url`, then `$PATHBASE_URL`, then `https://pathbase.dev`. The redeem endpoint is real but absent from `schema/pathbase-openapi.json` — so the progenitor-derived `pathbase-client` has no `redeem` method; the hand-rolled call in `cmd_pathbase.rs` is the source of truth.

## Key conventions

- Actor strings follow the pattern `type:name` (e.g. `human:alex`, `agent:claude-code`, `tool:rustfmt`)
- Artifact keys in `change` are URLs; bare paths are relative to `path.base`
- Change perspectives: `raw` (unified diff) and `structural` (AST-level operations)
- The `meta` object is always optional; minimal documents need only `step` + `change`
- IDs must be unique within their containing scope (steps within a path, paths within a graph)

## Testing

Tests live alongside the code (`#[cfg(test)] mod tests`); provider crates also have integration tests in `tests/`, and `path-cli` carries the cross-cutting suites (cross-harness conformance matrix at `crates/path-cli/tests/cross_harness_matrix.rs`, pathbase mock-server tests, cache-sync fixtures, query-planner streamed-equals-slurp checks). `cargo test --workspace` runs everything except the `toolpath-cli` shim (which has no tests).

- Live end-to-end check against a real Pathbase deployment: `scripts/test-pathbase-live.sh <url>` (anon round-trip in a sandboxed config dir; plus an authed pathstash round-trip if you're logged into that URL)
- Validate example documents: `for f in examples/*.json; do cargo run -p path-cli -- p validate --input "$f"; done`

## Feature flags

- `toolpath-claude` has a `watcher` feature (default: on) gating `notify`/`tokio` dependencies for filesystem watching
- `toolpath-gemini` has a `watcher` feature (default: on) gating the polling-based `ConversationWatcher` module

## Desktop app

The Tauri 2 desktop GUI lives in the private [pathbase](https://github.com/empathic/pathbase) repo as `pathbase-app`. It consumes the toolpath crates via git/crates.io deps. Don't look for it in this workspace — it was moved out when Pathbase went closed-source.

## Versioning and release checklist

When changing a crate's public API (new types, new trait impls, new public methods, new dependencies), bump its version. For pre-1.0 library crates, cargo treats the **z** position of `0.y.z` as the compatible slot: bug fixes *and additive changes* bump patch (`0.6.0` → `0.6.1`, so `^0.6` consumers like pathbase-app pick them up for free); bump minor only for potentially-breaking changes. `path-cli` is the app, not a library — it bumps minor per feature.

**Every version bump must update all of the following:**

1. **`crates/<name>/Cargo.toml`** — the crate's own `version` field
2. **`Cargo.toml`** (workspace root) — the `[workspace.dependencies]` entry for that crate
3. **`site/_data/crates.json`** — the `version` field for the crate's entry
4. **`CHANGELOG.md`** — add a new section at the top with the version and changes

**When adding a new crate**, also update:

5. **`Cargo.toml`** (workspace root) — add to `members` and `[workspace.dependencies]`
6. **`CLAUDE.md`** — repository layout, dependency graph
7. **`README.md`** — workspace listing
8. **`site/_data/crates.json`** — add a full entry (name, version, description, docs, crate, role)
9. **`site/pages/crates.md`** — dependency diagram
10. **`scripts/release.sh`** — add to `ALL_CRATES` array and the correct tier in the publish section
11. **Crate README** — create `crates/<name>/README.md` and wire it into lib.rs via `#![doc = include_str!("../README.md")]`

**Release script** (`scripts/release.sh`) publishes in dependency order:
- Tier 1: `toolpath` (no workspace deps)
- Tier 2: `toolpath-convo`; then the provider/render crates
- Tier 3: `path-cli`
- Tier 4: `toolpath-cli` (deprecated shim; ships only the `path` binary)

The `toolpath-cli` shim lives **outside** the workspace (`exclude = ["crates/toolpath-cli"]` in the root `Cargo.toml`): both it and `path-cli` produce a binary literally named `path`, and cargo can't write two bin targets to the same workspace `target/debug/path`. Consequently `cargo build/test --workspace` and `cargo run -p toolpath-cli` **do not** include it — use `--manifest-path crates/toolpath-cli/Cargo.toml`. The release script special-cases it.

Build the site after changes: `cd site && pnpm run build` (should produce 12 pages).

## Things to know

Format references for the agent on-disk formats live at `docs/agents/formats/` — Claude Code gets twelve focused docs at `docs/agents/formats/claude-code/`; single-file references cover codex, gemini, opencode, cursor, and copilot-cli. **Keep them in sync with their derive crates.** Details below are limited to what you need before opening those docs or the code.

### Core types

- `PathOrRef::Path` is `Box<Path>` to avoid a large enum variant size difference.
- Shared derivation: `toolpath-convo` provides the provider-agnostic `ConversationView → Path` mapping (`toolpath_convo::derive_path`). New conversation providers build on it rather than re-implementing the mapping.
- Provider extras: `Turn.extra` and `WatcherEvent::Progress.data` use provider-namespaced keys (e.g. `extra["claude"]`, `extra["gemini"]`) so trait-only consumers can reach provider metadata without importing provider types.
- Path kinds: `PathMeta.kind` is an optional URI naming a hosted kind spec; URIs are immutable and semver-versioned. The only one defined is `https://toolpath.net/kinds/agent-coding-session/v1.1.0` (`toolpath::v1::PATH_KIND_AGENT_CODING_SESSION`); every conversation → `Path` derivation sets it. Spec sources: `site/kinds/<name>/<version>/{index.md,schema.json}` (schema.json symlinks into `crates/path-cli/kinds/`, which `p validate` bundles). RFC section: "Document Kind".

### Token accounting (kind v1.1.0)

- Two optional keys on `conversation.append`/`Turn`: `token_usage` is the total for a provider message, stamped on the message group's final step (`Σ` over a path = session total); `attributed_token_usage` is a step's own attributed spend, populated only where the source genuinely reports per-step spend. `Turn.group_id` groups the steps of one message (remainder = group total − Σ attributed, computed not stored).
- Hard rules: never stamp a cumulative counter, a repeated message total, or zero-filled placeholders onto a step. Claude's per-line `usage` is a cumulative streaming snapshot, not a per-block cost — take the field-wise-max group total and never derive attribution from it. Codex must difference the cumulative `total_token_usage`, never sum `last_token_usage` (re-emitted stale). pi/opencode decode all-zero wire counters as `None`.
- `breakdowns` is an optional decomposition of a top-level class into sub-classes (e.g. `breakdowns["output"]["reasoning"]`). Informational only — **never summed into any total**; invariant `Σ(inner) ≤ parent`; omitted when empty. Gemini and OpenCode fold reasoning tokens into `output_tokens` and record this breakdown (projectors un-fold on the reverse path); Codex differences `reasoning_output_tokens`; Claude reports no breakdown.

### Providers

- Data locations: claude `~/.claude/projects/` (JSONL); gemini `~/.gemini/tmp/<project>/chats/` (project slot is a friendly name or a path SHA-256; sub-agents in sibling UUID dirs); codex `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (date-bucketed, not project-keyed); copilot `~/.copilot/session-state/<id>/events.jsonl` (`COPILOT_HOME` overrides; global, not project-keyed); opencode `~/.local/share/opencode/opencode.db` (read-only SQLite); cursor `state.vscdb` global SQLite under the Cursor app-support dir; pi `~/.pi/agent/sessions/` (tree in a single file, preserved as a DAG).
- `toolpath-claude` follows session chains by default — Claude Code rotates JSONL files on continuation while the chain keeps its oldest segment's id. `read_conversation` merges segments, `list_conversations` returns chain heads, `session_chain` resolves a chain oldest-first; `read_segment`/`list_segments` for single-file access; `ChainIndex` makes it incremental.
- `toolpath-gemini` folds sibling sub-agent files into `DelegatedWork` with populated `turns`; `toolpath-claude` sub-agent turns live in separate session files and stay empty.
- `toolpath-copilot` is a **preview** provider over a reverse-engineered `events.jsonl` schema; the reader is deliberately tolerant of shape variations and preserves unknown events. It is wired both directions: forward (`p import/list/show copilot`, `share`) and reverse via `CopilotProjector` (`p export copilot`, `resume` — writes `session-state/<id>/` plus a `session-store.db` row, only ever INSERTing a fresh id). The real `copilot --resume` loader is strict; its writer contract is documented in `docs/agents/formats/copilot-cli/writing-compatible.md` — follow it exactly.
- File-change fidelity varies: codex and copilot embed real diffs/file content in the session log, so derived paths get a true `raw` perspective; opencode diffs a sibling snapshot git repo via `git2`, falling back to structural-only changes for gitignored paths; cursor round-trips numeric tool-dispatch ids via `TOOL_TABLE` so projector-written composers render correctly in Cursor.app (the cursor-agent CLI's separate protobuf store is not parsed).
- `ConversationMetadata`/`SessionMeta` for claude/gemini/pi expose `first_user_message: Option<String>` (cheap, populated during the metadata pass) — used by pickers and any "list sessions by topic" surface.

### CLI behaviors

- Interactive pickers: `p import <provider>` auto-launches a fuzzy picker when TTY and no `--session`. Backend: external `fzf` if present, else the embedded skim picker (`embedded-picker` default feature, `crates/path-cli/src/skim_picker.rs`). Multi-select produces a `Graph`; single-select a `Path`. No usable backend falls back to most-recent (with `--project`) or prints the manual recipe. `p list <provider> --format tsv` is the machine-readable surface; the trailing column carries `first_user_message`.
- `path share` is the one-shot `p import <harness> | p export pathbase`: probes installed harnesses, aggregates sessions into one picker ranking current-directory sessions first; `--harness`/`--session`/`--project` skip the picker; pathbase flags match `p export pathbase`. When the sync manifest shows the picked session unchanged (`sync::fresh_cache_id`), it uploads the cached doc instead of re-deriving. Uploads carry the same full derivation as local projection — no egress stripping.
- Share also resolves a **configured share remote** from the session's own directory (the project for path-keyed harnesses, the recorded cwd otherwise — via the doc's `path.base` when no `--project` is in play): `crates/path-cli/src/share_config.rs` checks `~/.toolpath/config.toml` `[[project]]` rules (`dir` subtree match, `~/`-expandable, most specific wins; `remote` = bare `owner/name` or a canonical Pathbase repo web URL `https://<host>/u/<owner>/<name>`, which also carries the server — the URL scheme is the extension point for future backends, unknown schemes rejected). Precedence: `--repo` flag > config > `<you>/pathstash`; `--url` beats a URL remote's embedded server. A resolved remote prints `Sharing to <remote> (<origin>)`; hitting one while logged out errors with a `path auth login` hint (no silent anon fall-through), and explicit `--anon` opts out of the mapping. Both sides of the subtree match are prefix-canonicalized (longest existing ancestor resolved, tail re-appended) so macOS `/var`→`/private/var` and deleted checkouts still match. A repo-tracked `.toolpath.toml` is deliberately **not** consulted — a committed file redirecting other users' uploads needs a first-use consent flow (issue #179).
- `path resume <input>` is the inverse: accepts a Pathbase URL, `owner/repo/slug` shorthand, local file, or cache id; validates a single agent-bearing `Path`; opens a harness picker (pre-selecting `path.meta.source` when installed; `--harness` skips); projects the session into the harness's on-disk layout under `-C/--cwd` (default: shell cwd) and `execvp`'s the harness's resume command (spawn-and-wait on Windows).
- `path query` plans before it scans: `crates/path-cli/src/query/plan.rs` classifies the jaq filter into `PerFileStream` (element-wise, print as you go), `Decompose` (algebraic aggregation with a derived combine), or `Slurp` (the always-correct whole-array fallback). Recognition is conservative so **the planner never changes an answer**; `query/filter.rs` tests assert streamed output equals slurp byte-for-byte. Execution is parallel (`mod.rs::execute_plan`/`for_each_file`): `PerFileStream`/`Decompose` run the whole per-file pipeline (parse → wrap → filter → render/pack) on rayon workers in chunks — partials cross threads as compact JSON bytes since jaq `Val`s are `Rc`-based — while `Slurp` parallelizes only parse/wrap; output stays byte-identical to a sequential scan (ordering, warnings, error precedence), and the emscripten build stays fully sequential. `TOOLPATH_QUERY_EXPLAIN=1` prints the chosen plan; there is no user-facing flag. Caveat: a streamed top-N matches slurp's ranking, but boundary ties may resolve to different rows.

### Cache sync

- `p cache sync [types…]` incrementally ingests artifacts into the cache. Engine in `crates/path-cli/src/sync/` (`engine.rs` loop + `SyncObserver`, `sources.rs` per-provider `ArtifactSource` impls); artifact model in `src/artifact.rs`; progress UI in `cmd_cache.rs`.
- Change detection is **stat-level** (source mtime+size, or DB row updated-at) — a no-op sync reads no session bodies. Claude stamps the *whole session chain* (max segment mtime + summed sizes) because appends land in the newest segment, not the chain head.
- Manifest at `~/.toolpath/manifest.json`: artifact type → id → `{path?, cache_id, modified?, size?, synced_at}`. Atomic temp+rename writes, advisory lock with read-merge-save, checkpointed every 10 writes — concurrent invocations union their records, and an interrupted run keeps what it derived (derives run newest-first).
- Sync writes with refresh semantics and never deletes: artifacts removed upstream keep their cache docs and manifest records (archive, not mirror). Derivation failures warn and tally, they don't abort. A record without `cache_id` is "known, not materialized" (scope-excluded, or downgraded by `p cache rm`); the next in-scope sync re-materializes it, and sync verifies doc files actually exist before skipping.
- `--project-under <dir>` (on both `p cache sync` and `path query`) restricts to sessions whose project directory is under that subtree. Claude compares in *slug space* (its dir slugs are lossy — `/`, `_`, `.` all became `-`); codex/copilot get a one-line cwd peek only when new/changed, memoized into the record. The stat gate always runs before any scope check. Claude derives leave `DeriveConfig.project_path` unset so `path.base` comes from the session's recorded cwd, not the lossy slug.
- `path query` runs sync implicitly, scoped to its flags (`--source X` → that type; `--id`s → their prefixes; bare query → all types; `--input`-only → none), degrading to the cache as-is if sync fails; `--no-sync` opts out.
- `p import` and `share` record provenance for what they write (an `ArtifactRef` stamped *before* the source is read, saved via `sync::record_artifact`), so the next sync skips those artifacts. `--no-cache` paths record nothing: the manifest describes the cache.
- `ArtifactType` (`src/artifact.rs`) names artifact sources — the seven agent harnesses plus `Git`. Git artifacts are recorded by `p import git` but never discovered by sync (no machine-wide registry of repos); github/pathbase are deliberately not artifact types (remote services, not local sources). The parallel `Harness` enum (`src/harness.rs`) names the seven agent *runtimes* — what `share`/`resume` `--harness` take. Keep new code on `ArtifactType` unless it's genuinely harness-only.

### Claude Code plugin

- `.claude-plugin/marketplace.json` (marketplace `toolpath`) + `plugins/claude-code/` (plugin `path`; commands `/path:share`, `/path:query`, `/path:resume`, `/path:link-pr`). No binaries committed — commands bootstrap the CLI via `plugins/claude-code/scripts/ensure-path.sh` (Toolpath `path` on PATH → `~/.local/bin/path` → `~/.toolpath/bin/path` → sha256-verified GitHub release download).
- `/path:resume` projects a shared session into the current project via `p import pathbase` + `p export claude` and hands the user `/resume <id>` — the running TUI cannot be switched programmatically, and the command guards against re-exporting a session that already exists locally (export overwrites the file). `/path:link-pr` runs the share flow and appends the link to a PR description via `gh pr view/edit`.
- Two hard constraints in the command docs: slash-command `!` context commands and model-issued Bash must not contain `$PWD`/variables (Claude Code's permission checker rejects commands it can't statically analyze — hence the `sessions`/`current-session` helper modes), and `--project` must always be absolute (relative values silently match nothing).
- Tests: `scripts/test-plugin.sh` (the `plugin` quality gate); plugin scripts are shellchecked. Dev loop: `claude --plugin-dir ./plugins/claude-code`. Keep `plugins/claude-code/.claude-plugin/plugin.json` and the marketplace entry version in lockstep; `MIN_VERSION` in ensure-path.sh names the oldest CLI the command docs support.
