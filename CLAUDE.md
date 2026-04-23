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
  toolpath-opencode/            # derive from opencode SQLite databases
  toolpath-pi/                  # derive from Pi (pi.dev) agent session logs
  toolpath-dot/                 # Graphviz DOT rendering
  toolpath-md/                  # Markdown rendering for LLM consumption
  toolpath-cli/                 # unified CLI (binary: path)
  toolpath-desktop/             # Tauri 2 GUI (binary: toolpath-desktop)
schema/toolpath.schema.json     # JSON Schema for the format
examples/*.json                 # 12 example documents (step, path, graph)
RFC.md                          # full format specification
FAQ.md                          # design rationale, FAQ, and open questions
```

## Dependency graph

```
toolpath-cli (binary: path)
 ├── toolpath           (core types)
 ├── toolpath-convo   → toolpath (conversation abstraction + shared derivation)
 ├── toolpath-git     → toolpath
 ├── toolpath-github  → toolpath
 ├── toolpath-claude  → toolpath, toolpath-convo
 ├── toolpath-gemini  → toolpath, toolpath-convo
 ├── toolpath-codex   → toolpath, toolpath-convo
 ├── toolpath-opencode → toolpath, toolpath-convo
 ├── toolpath-pi      → toolpath, toolpath-convo
 ├── toolpath-dot     → toolpath
 └── toolpath-md      → toolpath

toolpath-desktop (binary: toolpath-desktop, Tauri 2 app)
 ├── toolpath, toolpath-claude, toolpath-git, toolpath-github
```

Cross-dependencies between satellite crates: `toolpath-claude → toolpath-convo`, `toolpath-gemini → toolpath-convo`, `toolpath-codex → toolpath-convo`, `toolpath-opencode → toolpath-convo`, `toolpath-pi → toolpath-convo`. `toolpath-desktop` is a leaf — nothing depends on it.

## Build and test

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Requires Rust 1.85+ (edition 2024). Pinned to 1.94.0 via `rust-toolchain.toml`.

## CLI usage

The binary is called `path` (package: `toolpath-cli`):

```bash
cargo run -p toolpath-cli -- derive git --repo . --branch main --pretty
cargo run -p toolpath-cli -- derive github --repo owner/repo --pr 42 --pretty
cargo run -p toolpath-cli -- derive claude --project /path/to/project
cargo run -p toolpath-cli -- derive gemini --project /path/to/project
cargo run -p toolpath-cli -- derive codex --session <uuid>
cargo run -p toolpath-cli -- derive opencode --session ses_<id>
cargo run -p toolpath-cli -- derive pi --project /path/to/project
cargo run -p toolpath-cli -- render dot --input doc.json
cargo run -p toolpath-cli -- render md --input doc.json --detail full
cargo run -p toolpath-cli -- query dead-ends --input doc.json
cargo run -p toolpath-cli -- query ancestors --input doc.json --step-id step-003
cargo run -p toolpath-cli -- query filter --input doc.json --actor "agent:"
cargo run -p toolpath-cli -- merge doc1.json doc2.json --title "Combined"
cargo run -p toolpath-cli -- list git --repo .
cargo run -p toolpath-cli -- list github --repo owner/repo
cargo run -p toolpath-cli -- list opencode
cargo run -p toolpath-cli -- list pi
cargo run -p toolpath-cli -- list pi --project /path/to/project
cargo run -p toolpath-cli -- track init --file src/main.rs --actor "human:alex"
cargo run -p toolpath-cli -- validate --input doc.json
cargo run -p toolpath-cli -- auth login
cargo run -p toolpath-cli -- auth status
cargo run -p toolpath-cli -- auth whoami
cargo run -p toolpath-cli -- auth logout
```

`path auth login` prints `<base>/auth/cli`; the user opens it, logs in, and
pastes the 8-character code back into the CLI. The CLI calls
`POST /api/v1/auth/cli/redeem` to trade the code for a bearer token, which it
writes to `~/.toolpath/credentials.json` (0600, parent dir 0700) and sends as
`Authorization: Bearer <token>` on future requests. `$TOOLPATH_CONFIG_DIR`
overrides the credentials directory. Server URL comes from `--url`, then
`$PATHBASE_URL`, then `https://pathbase.dev`.

## Key conventions

- Actor strings follow the pattern `type:name` (e.g. `human:alex`, `agent:claude-code`, `tool:rustfmt`)
- Artifact keys in `change` are URLs; bare paths are relative to `path.base`
- Change perspectives: `raw` (unified diff) and `structural` (AST-level operations)
- The `meta` object is always optional; minimal documents need only `step` + `change`
- IDs must be unique within their containing scope (steps within a path, paths within a graph)

## Testing

Tests live alongside the code (`#[cfg(test)] mod tests`), plus `toolpath-cli` has integration tests in `tests/`. Per-crate counts:

- `toolpath`: 32 unit + 9 doc tests (serde roundtrip, builders, query)
- `toolpath-convo`: 58 unit + 1 doc test (types, enrichment, display, ConversationView -> Path derivation)
- `toolpath-git`: 33 unit + 3 doc tests (derive, branch detection, diffstat)
- `toolpath-github`: 28 unit + 2 doc tests (mapping, DAG construction, fixtures)
- `toolpath-claude`: 216 unit + 5 doc tests (path resolution, conversation reading, query, chaining, watcher, derive)
- `toolpath-gemini`: 163 unit + 12 integration + 4 doc tests (path resolution, chat-file parsing, query, watcher, derive, provider, round-trip fidelity)
- `toolpath-codex`: 69 unit + 33 integration + 1 doc test (rollout parsing, provider assembly, patch-fidelity derive, real-session fixture, source→path fidelity invariants, JSON wire-level round-trip)
- `toolpath-opencode`: 43 unit + 1 doc test (SQLite reader, JSON payload serde, provider assembly, snapshot-based derive, tool-input fallback for gitignored paths)
- `toolpath-pi`: ~88 unit tests (types, paths, error, reader, io, provider)
- `toolpath-dot`: 30 unit + 2 doc tests (render, visual conventions, escaping)
- `toolpath-cli`: 126 unit + 24 integration tests (all commands, track sessions, merge, validate, roundtrip, render-md snapshots)
- `toolpath-desktop`: 17 unit tests (IPC command modules — source listing, derive validation, export round-trip, upload stub, keychain input checks; tray activity-window bucketing, stats-snapshot smoke, session-id/basename helpers)

Validate example documents: `for f in examples/*.json; do cargo run -p toolpath-cli -- validate --input "$f"; done`

## Feature flags

- `toolpath-claude` has a `watcher` feature (default: on) gating `notify`/`tokio` dependencies for filesystem watching
- `toolpath-gemini` has a `watcher` feature (default: on) gating the polling-based `ConversationWatcher` module

## toolpath-desktop

Tauri 2 app. Rust backend links `toolpath`, `toolpath-claude`, `toolpath-git`, `toolpath-github` directly (no CLI subprocess). Frontend is Svelte 5 + TypeScript + Vite, in the Elm-architecture (TEA) shape.

Layout:
- `crates/toolpath-desktop/src/` — Rust backend (Tauri IPC commands).
- `crates/toolpath-desktop/frontend/` — Svelte app (bundled by Vite; managed with `bun`).
  - `src/lib/types.ts` — `Model`, `Msg`, `Cmd`, IPC payload types (mirror Rust serde).
  - `src/lib/update.ts` — pure `update(msg, model) -> [model, cmd]` reducer + `initialModel()`.
  - `src/lib/store.svelte.ts` — reactive Svelte 5 store wrapping `update`, runs `Cmd`s (invoke / batch / emitMsg / fn).
  - `src/lib/ipc.ts` — typed `invoke()` + `listen()` wrappers around `@tauri-apps/api`.
  - `src/lib/viz.ts` — dagre-d3 DAG renderer.
  - `src/routes/*.svelte` — one component per route, pure views over `store.m`.
  - `src/app.svelte` — top-level switch on `store.m.route`.

Tauri dev loop: `cargo tauri dev` spawns `bun --cwd frontend run dev` (Vite on `http://localhost:1420`), then runs the Rust binary against that URL. Frontend edits hot-reload via Vite HMR without restarting Rust; Rust edits trigger `cargo run` to restart. Production: `cargo tauri build` runs `bun --cwd frontend run build` first, bundling to `frontend/dist/`.

Menu-bar mode: the app runs as a normal GUI app (Dock icon + app-switcher entry) *and* installs a tray icon — the tray is an accessory, not a replacement for the main window. Accessory activation policy was tried and reverted because macOS tiling window managers (yabai, Amethyst) stop managing accessory windows. A tray icon is installed in `src/tray.rs`; a background thread polls every 30s across `toolpath-claude`, `-gemini`, `-codex`, `-opencode`, and `-pi`, classifies sessions as *active* (last activity in the last 2 min) or *recent* (last 24h), updates the tray title (`● N`), and emits a `tray:stats` event. The popover is a second Tauri window (`label = "popover"`, undecorated, hidden by default) with its own Vite entry (`frontend/popover.html` → `src/popover.ts` → `routes/Popover.svelte`); left-clicking the tray toggles it via `tauri-plugin-positioner`. For an on-demand snapshot (no waiting for the next poll) the popover invokes the `tray_stats_now` IPC command.

Opening a trace from the popover: clicking a recent-session row invokes `tray_open_trace { provider, project, session_id }`. The Rust side calls back into the existing `derive_claude` / `derive_pi` commands, shows the main window, and emits a `trace:opened` event to the main window with the derived `{ doc, source, filename }`. `app.svelte` listens for it and dispatches `DeriveSucceeded`, which routes to the preview. Only `claude` and `pi` have derive commands today — rows for `gemini`, `codex`, `opencode` still appear in the list (so users can see activity) but are rendered disabled.

Pre-derive cache: after each 30s poll the tray kicks off background derives for every recent claude/pi session and stashes the result in `src/cache.rs`'s `TraceCache` (shared via `app.manage(Arc<TraceCache>)`). Both the popover's `tray_open_trace` and the main-window's `derive_claude` / `derive_pi` commands route through the same cache (via `derive_claude_impl` / `derive_pi_impl` in `commands/derive.rs`), so clicking a session — whether from Quick View or the Browse view's "Select →" button — usually resolves instantly. Cache freshness keys on the source's `last_activity`; when a session gets new turns its cached entry is replaced on the next poll. Cacheable calls are limited to single-session, `include_thinking=false` derives (the shape the poller prewarms). Warm-up runs with at most 2 concurrent threads.

Two tiers: (1) memory, a `HashMap` capped at 32 entries; (2) disk, under `<temp_dir>/toolpath-desktop/trace-cache/<fnv1a64-of-key>.json`, so caches survive app restarts (macOS/Linux eventually clean `/tmp` themselves). Disk is capped at 200 entries, pruned oldest-first by mtime at startup. Memory misses fall through to disk and promote the hit back into memory. Corrupt files are silently deleted on read so a bad write doesn't poison the cache forever.

Perf tracer (`frontend/src/lib/perf.svelte.ts`, `PerfOverlay.svelte`): the store, Preview, and the `trace:opened` listener call `perfStart` / `perfMark` / `perfEnd` at each checkpoint of a click → derive → render flow (dispatch, invoke-start, invoke-end, model-updated, preview-mounted, viz-rendered). Every completed trace logs a summary to the devtools console; set `localStorage.perf = "1"` and reload to also show a phase-bar overlay in the bottom-right. Use this to tell whether perceived click latency is the Rust derive vs. the Svelte/dagre render.

Streaming pattern (Claude project/session lists): Rust command spawns a thread that emits `claude:project`, `claude:session`, `claude:projects-done`, `claude:sessions-done` events. The Svelte component subscribes with `$effect(() => { listen(...) ... return unlisten; })` — Svelte tears down listeners automatically when the effect's deps change or the component unmounts.

Package manager for the frontend is `bun` (installed at `~/.bun/bin/bun`). `bun install` to set up, `bun run check` for `svelte-check`, `bun run build` for a production Vite build. Never commit `node_modules/` or `dist/` — both are ignored.

Dev: `cargo tauri dev` from the crate directory. Build: `cargo tauri build`. GitHub PAT is stored in the OS keychain under service `dev.pathbase.toolpath-desktop`. Pathbase upload is a stub as of 0.1.0 — it validates the payload and returns a mock URL.

## Versioning and release checklist

When changing a crate's public API (new types, new trait impls, new public methods, new dependencies), bump its version. Use semver: patch for bug fixes, minor for additive changes, major for breaking changes. Pre-1.0 crates treat minor as "potentially breaking."

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
- Tier 2: `toolpath-convo` (depends on `toolpath`); then `toolpath-git`, `toolpath-github`, `toolpath-dot`, `toolpath-md`, `toolpath-claude`, `toolpath-gemini`, `toolpath-codex`, `toolpath-opencode`, `toolpath-pi`
- Tier 3: `toolpath-cli` (depends on everything)

Build the site after changes: `cd site && pnpm run build` (should produce 7 pages).

## Things to know

- The `Document` enum is externally tagged -- JSON documents are wrapped in `{"Step": ...}`, `{"Path": ...}`, or `{"Graph": ...}`
- `PathOrRef::Path` is `Box<Path>` to avoid a large enum variant size difference
- The git derivation (`toolpath-git`) uses `git2` (libgit2 bindings), not shelling out to git
- Claude conversation data lives in `~/.claude/projects/` as JSONL files; `toolpath-claude` reads these directly
- `toolpath-claude` follows session chains by default — Claude Code rotates JSONL files on context overflow; `read_conversation` merges segments, `list_conversations` returns chain heads. `read_segment`/`list_segments` for single-file access. `ChainIndex` makes this incremental.
- Gemini CLI conversation data lives in `~/.gemini/tmp/<project>/chats/`. Main sessions sit at the top (`session-<timestamp>-<short>.json`, `kind: "main"`); sub-agents live in sibling `<full-uuid>/` directories (`kind: "subagent"`). The `<project>` slot is either a friendly name from `~/.gemini/projects.json` or the SHA-256 hex of the absolute project path; `toolpath-gemini` resolves both.
- `toolpath-gemini` treats main file + sibling sub-agent UUID dir as one conversation. Sub-agent files are folded into `DelegatedWork` with populated `turns` (unlike `toolpath-claude`, whose sub-agent turns live in separate session files and stay empty). See `docs/agents/formats/gemini.md` for the full format reference.
- Provider-specific extras convention: `Turn.extra` and `WatcherEvent::Progress.data` use provider-namespaced keys (e.g. `extra["claude"]`, `extra["gemini"]`). `toolpath-claude` populates `Turn.extra["claude"]` from `ConversationEntry.extra`; `toolpath-gemini` populates `Turn.extra["gemini"]` with the full `tokens` struct, per-thought metadata, and tool-call status. This lets trait-only consumers access provider metadata without importing provider types.
- Shared derivation: `toolpath-convo` provides a provider-agnostic `ConversationView → Path` mapping via `toolpath_convo::derive_path`. New conversation providers should build on it rather than re-implementing the mapping.
- Pi provider: `toolpath-pi` reads Pi session JSONL from `~/.pi/agent/sessions/`. Sessions use a tree (id/parentId) in a single file, and may link to a parent file via `parentSession` in the header. The tree is preserved as a DAG in the derived `Path`.
- Codex provider: `toolpath-codex` reads Codex CLI rollout files from `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Sessions are date-bucketed (not project-keyed). File-change fidelity is excellent — Codex's `patch_apply_end` events carry either the unified diff (for updates) or the full file content (for adds), so the derived `Path` gets a real `raw` perspective on every file artifact. See `docs/agents/formats/codex.md` for the full format reference.
- opencode provider: `toolpath-opencode` reads a SQLite database at `~/.local/share/opencode/opencode.db` (opened read-only). Each session's messages and 12 typed part variants (text, reasoning, tool, step-start/-finish, snapshot, patch, file, agent, subtask, retry, compaction) land as one step per message with tool invocations attached. File diffs come from a sibling bare git repo at `snapshot/<project-id>/[<sha1(worktree)>]/` via `git2` tree↔tree diffs — opencode respects the user's `.gitignore`, so changes under gitignored paths fall back to tool-input-derived structural changes with no `raw` perspective. Project id is the SHA of the repo's first root commit. See `docs/agents/formats/opencode.md` for the full format reference.
- Format references for the agent on-disk formats we derive from live at `docs/agents/formats/`. The Claude Code format (`~/.claude/projects/…` JSONL) gets the deepest treatment — twelve focused docs at `docs/agents/formats/claude-code/` covering envelope, entry types, tools, session chains, compaction, writing-compatible JSONL, a linear walkthrough, and a version-keyed changelog. Sibling single-file references: `codex.md`, `gemini.md`, `opencode.md`. Keep them in sync with their derive crates when fields or behaviors change.
