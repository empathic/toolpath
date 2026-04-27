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
  path-cli/                     # unified CLI (binary: path)
  toolpath-cli/                 # deprecated shim that re-exports path-cli (excluded from the workspace; see below)
schema/toolpath.schema.json     # JSON Schema for the format
examples/*.json                 # 12 example documents (step, path, graph)
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
 ├── toolpath-opencode → toolpath, toolpath-convo
 ├── toolpath-pi      → toolpath, toolpath-convo
 ├── toolpath-dot     → toolpath
 └── toolpath-md      → toolpath

toolpath-cli (deprecated shim, binary: path)
 └── path-cli
```

Cross-dependencies between satellite crates: `toolpath-claude → toolpath-convo`, `toolpath-gemini → toolpath-convo`, `toolpath-codex → toolpath-convo`, `toolpath-opencode → toolpath-convo`, `toolpath-pi → toolpath-convo`.

The desktop GUI lives in the private [pathbase](https://github.com/empathic/pathbase) repo as `pathbase-app` — it consumes the toolpath crates via git or crates.io.

## Build and test

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Requires Rust 1.85+ (edition 2024). Pinned to 1.94.0 via `rust-toolchain.toml`.

## CLI usage

The binary is called `path` (package: `path-cli`; the older `toolpath-cli` package is a deprecated shim that still installs the same binary for users running `cargo install toolpath-cli`):

```bash
# Import from external formats into the local toolpath cache (~/.toolpath/documents/)
cargo run -p path-cli -- import git --repo . --branch main
cargo run -p path-cli -- import github https://github.com/owner/repo/pull/42
cargo run -p path-cli -- import claude --project /path/to/project
cargo run -p path-cli -- import gemini --project /path/to/project
cargo run -p path-cli -- import codex --session <uuid>
cargo run -p path-cli -- import opencode --session ses_<id>
cargo run -p path-cli -- import pi --project /path/to/project
cargo run -p path-cli -- import pathbase <trace-id-or-url>
cargo run -p path-cli -- import claude --project . --no-cache | path render md --input -

# Export toolpath documents into external formats. <ref> is a cache id or a file path.
cargo run -p path-cli -- export claude --input <ref> --project /tmp/sandbox
cargo run -p path-cli -- export claude --input <ref> --output conv.jsonl
cargo run -p path-cli -- export pathbase --input <ref>

# Manage the cache
cargo run -p path-cli -- cache ls
cargo run -p path-cli -- cache rm <cache-id>

# Inspect / analyze
cargo run -p path-cli -- render dot --input doc.json
cargo run -p path-cli -- render md --input doc.json --detail full
cargo run -p path-cli -- query dead-ends --input doc.json
cargo run -p path-cli -- query ancestors --input doc.json --step-id step-003
cargo run -p path-cli -- query filter --input doc.json --actor "agent:"
cargo run -p path-cli -- merge doc1.json doc2.json --title "Combined"
cargo run -p path-cli -- list git --repo .
cargo run -p path-cli -- list github --repo owner/repo
cargo run -p path-cli -- list opencode
cargo run -p path-cli -- list pi
cargo run -p path-cli -- list pi --project /path/to/project
cargo run -p path-cli -- list claude --format tsv  # one session per line, fzf-friendly
cargo run -p path-cli -- show claude --project /path/to/project --session <session-id>  # markdown summary; used by fzf preview
cargo run -p path-cli -- track init --file src/main.rs --actor "human:alex"
cargo run -p path-cli -- validate --input doc.json
cargo run -p path-cli -- auth login
cargo run -p path-cli -- auth status
cargo run -p path-cli -- auth whoami
cargo run -p path-cli -- auth logout
```

`path derive`, `path incept`, and `path project` are deprecated aliases for `path import` / `path export claude` and print a deprecation warning to stderr. They will be removed in the release after next.

The **cache** at `~/.toolpath/documents/<cache-id>.json` is the single landing zone for every `import` (and for `import pathbase` downloads). Cache id is `<source>-<inner-id>` — e.g. `claude-abc123`, `git-main`, `pathbase-trc_01H…`. Files are `0600`, parent directory `0700`. `$TOOLPATH_CONFIG_DIR` overrides the root. Default behavior: error on cache hit; pass `--force` to overwrite. `--no-cache` sends the JSON to stdout for shell composition.

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

Tests live alongside the code (`#[cfg(test)] mod tests`), plus `path-cli` has integration tests in `tests/`. Per-crate counts:

- `toolpath`: 32 unit + 9 doc tests (serde roundtrip, builders, query)
- `toolpath-convo`: 58 unit + 1 doc test (types, enrichment, display, ConversationView -> Path derivation)
- `toolpath-git`: 33 unit + 3 doc tests (derive, branch detection, diffstat)
- `toolpath-github`: 28 unit + 2 doc tests (mapping, DAG construction, fixtures)
- `toolpath-claude`: 278 unit + 6 doc tests (path resolution, conversation reading, query, chaining, watcher, derive, metadata first-user-message)
- `toolpath-gemini`: 163 unit + 12 integration + 4 doc tests (path resolution, chat-file parsing, query, watcher, derive, provider, round-trip fidelity)
- `toolpath-codex`: 69 unit + 33 integration + 1 doc test (rollout parsing, provider assembly, patch-fidelity derive, real-session fixture, source→path fidelity invariants, JSON wire-level round-trip)
- `toolpath-opencode`: 43 unit + 1 doc test (SQLite reader, JSON payload serde, provider assembly, snapshot-based derive, tool-input fallback for gitignored paths)
- `toolpath-pi`: 123 unit + 4 doc tests (types, paths, error, reader, io, provider)
- `toolpath-dot`: 30 unit + 2 doc tests (render, visual conventions, escaping)
- `path-cli`: 174 unit + 26 integration tests (import/export/cache, track sessions, merge, validate, roundtrip, render-md snapshots, deprecation aliases, pathbase HTTP mock-server tests, fzf-friendly TSV output)
- `toolpath-cli`: 0 tests (it's a one-line `path_cli::run()` shim crate that exists only so `cargo install toolpath-cli` keeps installing the `path` binary)

Validate example documents: `for f in examples/*.json; do cargo run -p path-cli -- validate --input "$f"; done`

## Feature flags

- `toolpath-claude` has a `watcher` feature (default: on) gating `notify`/`tokio` dependencies for filesystem watching
- `toolpath-gemini` has a `watcher` feature (default: on) gating the polling-based `ConversationWatcher` module

## Desktop app

The Tauri 2 desktop GUI lives in the private [pathbase](https://github.com/empathic/pathbase) repo as `pathbase-app`. It consumes `toolpath`, `toolpath-claude`, `toolpath-git`, `toolpath-github`, `toolpath-gemini`, `toolpath-codex`, `toolpath-opencode`, and `toolpath-pi` via git/crates.io deps. Don't look for it in this workspace — it was moved out when Pathbase went closed-source.

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
- Tier 3: `path-cli` (depends on everything above)
- Tier 4: `toolpath-cli` (deprecated shim that depends on `path-cli`; ships only the `path` binary)

The `toolpath-cli` shim lives **outside** the workspace (`exclude = ["crates/toolpath-cli"]` in the root `Cargo.toml`). Both `toolpath-cli` and `path-cli` produce a binary literally named `path`, and cargo can't write two bin targets to the same workspace `target/debug/path` — so the shim opts out and gets its own `crates/toolpath-cli/target/` (covered by the `crates/*/target` line in `.gitignore`). Practical consequences: `cargo build --workspace`, `cargo test --workspace`, and `cargo run -p toolpath-cli` from the repo root **do not** include the shim. To touch it, use `--manifest-path crates/toolpath-cli/Cargo.toml`. The release script special-cases the shim in `get_version` and `publish` so the workflow is otherwise unchanged.

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
- Interactive session selection: `path import <provider>` (claude / gemini / pi / codex / opencode) auto-launches `fzf` when stdin and stderr are TTYs, `fzf` is on `$PATH`, and no `--session` was given. Multi-select (TAB) produces a `Graph` document; single-select produces a `Path`. The picker uses `path show <provider> --…` as its `--preview` command. When fzf isn't available, it falls back to most-recent (with `--project`) or prints the manual recipe (without). `path list <provider> --format tsv` is the documented machine-readable surface — column 1 is the project (for claude/gemini/pi) or session id (for codex/opencode), and the trailing column carries `first_user_message` so consumers can fuzzy-match by topic.
- Conversation metadata title field: `toolpath-claude::ConversationMetadata`, `toolpath-gemini::ConversationMetadata`, and `toolpath-pi::SessionMeta` all expose `first_user_message: Option<String>` — the first non-empty user-prompt text. Populated cheaply during the metadata pass (single-pass for Claude/Gemini; one extra short read for Pi). Used by the picker UI but useful for any "list sessions by topic" surface.
