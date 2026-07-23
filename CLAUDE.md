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
  toolpath-copilot/             # derive from + project to GitHub Copilot CLI session logs (preview; import/list/show/share/export/resume wired; resume verified in copilot 1.0.67)
  toolpath-opencode/            # derive from opencode SQLite databases
  toolpath-cursor/              # derive from Cursor (IDE) state.vscdb bubble store
  toolpath-pi/                  # derive from Pi (pi.dev) agent session logs
  toolpath-dot/                 # Graphviz DOT rendering
  toolpath-md/                  # Markdown rendering for LLM consumption
  path-cli/                     # unified CLI (binary: path)
  toolpath-cli/                 # deprecated shim that re-exports path-cli (excluded from the workspace; see below)
  pathbase-client/              # progenitor-derived client for the Pathbase HTTP API
                                # (spec at crates/pathbase-client/openapi.json; refresh via scripts/refresh-pathbase-openapi.sh)
schema/toolpath.schema.json     # JSON Schema for the toolpath format
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

`toolpath-copilot` is a **preview** provider (workspace member + `path-cli`
dependency) wired both directions: forward (`path p import / list / show
copilot`, `path share`) and reverse via a `CopilotProjector` (`path p export
copilot`, `path resume`). Resume/export write `~/.copilot/session-state/<id>/`
plus a `session-store.db` `sessions` row (only ever INSERTing a fresh id).
**✅ Verified in copilot 1.0.67:** a projected session loads and resumes in the
real `copilot --resume`. Getting there mapped the loader's writer contract
(UUID `id`/`parentId`, offset-ISO timestamps, `turnId`, `messageId`, non-empty
`toolCallId`, full `session.start` shape, and `subagent.*` fields) — see
`docs/agents/formats/copilot-cli/writing-compatible.md`. Verified on both a
27-event session and a 5817-event session with sub-agents (a Pathbase graph
resumed by URL); also validated by the cross-harness matrix + a round-trip test.

Cross-dependencies between satellite crates: `toolpath-claude → toolpath-convo`, `toolpath-gemini → toolpath-convo`, `toolpath-codex → toolpath-convo`, `toolpath-copilot → toolpath-convo`, `toolpath-opencode → toolpath-convo`, `toolpath-cursor → toolpath-convo`, `toolpath-pi → toolpath-convo`.

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

The top-level surface is the porcelain (`show`, `share`, `resume`,
`query`, `kind`, `auth`, `haiku`). Lower-level building blocks live under
`path p …` (plumbing): `p list`, `p import`, `p export`, `p cache`,
`p render`, `p merge`, `p validate`, `p derive`, `p project`,
`p incept`, `p track`, `p query` (graph traversal: `ancestors`).

```bash
# Plumbing: import from external formats into the local toolpath cache
# (~/.toolpath/documents/)
cargo run -p path-cli -- p import git --repo . --branch main
cargo run -p path-cli -- p import github https://github.com/owner/repo/pull/42
cargo run -p path-cli -- p import claude --project /path/to/project
cargo run -p path-cli -- p import gemini --project /path/to/project
cargo run -p path-cli -- p import codex --session <uuid>
cargo run -p path-cli -- p import opencode --session ses_<id>
cargo run -p path-cli -- p import cursor --session <composer-uuid>   # IDE composer from state.vscdb
cargo run -p path-cli -- p import pi --project /path/to/project
cargo run -p path-cli -- p import pathbase <pathbase-url-or-owner/repo/slug>
cargo run -p path-cli -- p import claude --project . --no-cache | path p render md --input -

# Share an agent session to Pathbase (interactive picker, single-shot)
cargo run -p path-cli -- share
cargo run -p path-cli -- share --harness claude --session <session-id> --project /path/to/project
cargo run -p path-cli -- share --url https://my-pathbase.example

# Resume a Toolpath document into your coding agent of choice (interactive harness picker)
cargo run -p path-cli -- resume <pathbase-url-or-shorthand-or-file-or-cache-id>
cargo run -p path-cli -- resume <input> --harness claude -C /path/to/project

# Plumbing: export toolpath documents into external formats. <ref> is a
# cache id or a file path.
cargo run -p path-cli -- p export claude --input <ref> --project /tmp/sandbox
cargo run -p path-cli -- p export claude --input <ref> --output conv.jsonl
cargo run -p path-cli -- p export cursor --input <ref> --project /tmp/workspace   # writes composer rows into state.vscdb
cargo run -p path-cli -- p export cursor --input <ref> --output composer.json
cargo run -p path-cli -- p export pathbase --input <ref>

# Plumbing: manage the cache
cargo run -p path-cli -- p cache ls
cargo run -p path-cli -- p cache rm <cache-id>

# Inspect / analyze
cargo run -p path-cli -- p render dot --input doc.json
cargo run -p path-cli -- p render md --input doc.json --detail full
# Query the whole local cache with a jaq (jq) filter over wrapped steps
cargo run -p path-cli -- query 'map(select(.dead_end)) | map(.step.id)'
cargo run -p path-cli -- query --source claude 'map(select(any(.change[].structural.token_usage; .input_tokens > 50000)))'
cargo run -p path-cli -- query --input doc.json 'map(select(.step.actor | startswith("agent:")))'
cargo run -p path-cli -- kind                          # list bundled kinds
cargo run -p path-cli -- kind agent-coding-session     # print a kind's schema (field reference)
cargo run -p path-cli -- p query ancestors --input doc.json --step-id step-003
cargo run -p path-cli -- p merge doc1.json doc2.json --title "Combined"
cargo run -p path-cli -- p list git --repo .
cargo run -p path-cli -- p list github --repo owner/repo
cargo run -p path-cli -- p list opencode
cargo run -p path-cli -- p list cursor                       # Cursor (IDE) composers
cargo run -p path-cli -- p list cursor --project /path/to/workspace   # filter by workspace
cargo run -p path-cli -- p list pi
cargo run -p path-cli -- p list pi --project /path/to/project
cargo run -p path-cli -- p list claude --format tsv  # one session per line, fzf-friendly
cargo run -p path-cli -- show claude --project /path/to/project --session <session-id>  # markdown summary; used by fzf preview
cargo run -p path-cli -- p track init --file src/main.rs --actor "human:alex"
cargo run -p path-cli -- p validate --input doc.json
cargo run -p path-cli -- auth login
cargo run -p path-cli -- auth status
cargo run -p path-cli -- auth whoami
cargo run -p path-cli -- auth logout
```

**Breaking** (pre-1.0). The previous top-level commands `path import`,
`path export`, `path cache`, `path list`, `path render`, `path merge`,
`path validate`, `path derive`, `path incept`, `path project`, and
`path track` were **removed** in 0.10.0 — there is no top-level alias
and no deprecation shim. They all now live exclusively under
`path p …`.

The **cache** at `~/.toolpath/documents/<cache-id>.json` is the single landing zone for every `import` (and for `import pathbase` downloads). Cache id is `<source>-<inner-id>` — e.g. `claude-abc123`, `git-main`, `pathbase-alex-pathstash-path-pr-42` (Pathbase paths key on `<owner>-<repo>-<slug>`, anon paths on `anon-pathstash-<uuid>`). Files are `0600`, parent directory `0700`. `$TOOLPATH_CONFIG_DIR` overrides the root. Default behavior: error on cache hit; pass `--force` to overwrite. `--no-cache` sends the JSON to stdout for shell composition.

`path auth login` prints `<base>/auth/cli`; the user opens it, logs in, and
pastes the 8-character code back into the CLI. The CLI calls
`POST /api/v1/auth/cli/redeem` to trade the code for a bearer token, which it
writes to `~/.toolpath/credentials.json` (0600, parent dir 0700) and sends as
`Authorization: Bearer <token>` on future requests. `$TOOLPATH_CONFIG_DIR`
overrides the credentials directory. Server URL comes from `--url`, then
`$PATHBASE_URL`, then `https://pathbase.dev`.

The CLI redeem endpoint (`POST /api/v1/auth/cli/redeem`) is real and works
in production but is **not listed in `schema/pathbase-openapi.json`** — the
OpenAPI spec only covers the documented surface. Don't be surprised that
the progenitor-derived `pathbase-client` lacks a `redeem` method; the
hand-rolled redeem call in `cmd_pathbase.rs` is the source of truth until
the server publishes that operation.

## Key conventions

- Actor strings follow the pattern `type:name` (e.g. `human:alex`, `agent:claude-code`, `tool:rustfmt`)
- Artifact keys in `change` are URLs; bare paths are relative to `path.base`
- Change perspectives: `raw` (unified diff) and `structural` (AST-level operations)
- The `meta` object is always optional; minimal documents need only `step` + `change`
- IDs must be unique within their containing scope (steps within a path, paths within a graph)

## Testing

Tests live alongside the code (`#[cfg(test)] mod tests`), plus `path-cli` has integration tests in `tests/`. Per-crate counts:

- `toolpath`: 69 unit + 11 doc tests (serde roundtrip, builders, query)
- `toolpath-convo`: 135 unit + 3 property + 4 doc tests (types, enrichment, display, ConversationView -> Path derivation, message-group usage accounting, breakdowns, compaction/`kept_from` contract; `tests/proptests.rs` holds the proptest suite — unique derived ids, derive→extract→derive stability, replay-drop no-op; `src/testing.rs` ships the `check_view_invariants`/`assert_fixpoint` oracle every provider round-trip suite uses)
- `toolpath-git`: 33 unit + 3 doc tests (derive, branch detection, diffstat)
- `toolpath-github`: 32 unit + 3 doc tests (mapping, DAG construction, fixtures)
- `toolpath-claude`: 232 unit + 30 integration + 6 doc tests (path resolution, conversation reading incl. mid-write concatenated-line recovery, query, chaining, watcher, derive, metadata first-user-message, group_id grouping + once-per-message usage totals, wire entry-order + isMeta preservation on the captured compacted fixture, no-empty-text-block projection)
- `toolpath-gemini`: 164 unit + 29 integration + 5 doc tests (path resolution, chat-file parsing, query, watcher, derive, provider, round-trip fidelity, thoughts-folded-into-output + reasoning breakdown round-trip)
- `toolpath-codex`: 84 unit + 58 integration + 2 doc tests (rollout parsing, provider assembly, patch-fidelity derive, real-session fixture, source→path fidelity invariants, JSON wire-level round-trip, per-turn token deltas from cumulative counters, reasoning breakdown)
- `toolpath-copilot`: 66 unit + 11 integration + 1 doc test (`events.jsonl` envelope/event-type classification incl. `session.start` nested `context` + `tool.execution` `result.content` + `assistant.message` `reasoningText`/`outputTokens`, `session.shutdown` `tokenDetails`, path resolution incl. legacy `history-session-state/`, reader malformed-line tolerance without env races, tolerant `workspace.yaml` parse, `to_view` turn/tool/delegation assembly + per-turn token usage + shutdown-total merge, id-based **and** id-less positional tool pairing, position-stable turn ids, native file-state diff from `result.detailedContent`, `CopilotProjector` round-trip + foreign-tool-name remap, compaction mapping: successful `session.compaction_complete` → typed boundary, failed → generic event, parent stitching through the boundary). Ships a **real captured feature-elicit session** at `tests/fixtures/real-session.jsonl` (also `test-fixtures/copilot/convo.jsonl`) driving `real_fixture_roundtrip.rs` (forward invariants, projection round-trip fidelity, wire-level serde value-identity), plus a **real captured compacted session** at `test-fixtures/copilot/compacted-real.jsonl` driving `compacted_real_roundtrip.rs` (typed-boundary read, looped derive → extract → derive stability, observed compaction-pair emission). The projector is exercised by the cross-harness matrix in `path-cli`.
- `toolpath-opencode`: 54 unit + 25 integration + 1 doc test (SQLite reader, JSON payload serde, provider assembly, snapshot-based derive, tool-input fallback for gitignored paths, reasoning breakdown)
- `toolpath-cursor`: 78 unit + 8 integration round-trip + 1 real-DB sanity + 1 doc test (state.vscdb SQLite reader, bubble store + composer header parsing, content-addressed blob lookup, projector with full TOOL_TABLE coverage, JSONL transcript ingest in `examples/dump_fixture.rs`)
- `toolpath-pi`: 139 unit + 33 integration + 5 doc tests (types, paths, error, reader, io, provider, non-turn compaction-anchor resolution, parent-chain kept expansion, meta-entry event round-trip + provider pairing on the captured compacted fixture)
- `toolpath-dot`: 30 unit + 2 doc tests (render, visual conventions, escaping)
- `toolpath-md`: 65 unit + 3 doc tests (transcript vs DAG rendering, kind-URI matching across all published versions, detail levels)
- `path-cli`: 326 unit + 107 integration tests (import/export/cache, track sessions, merge, validate, roundtrip, render-md snapshots, deprecation aliases, pathbase HTTP mock-server tests, fzf-friendly TSV output, `path resume` orchestration with injectable `ExecStrategy` incl. Copilot, Copilot import/list/show/**export** via `COPILOT_HOME` + Copilot in the `path share` aggregator + Copilot in the cross-harness conformance matrix, `path query`/`path kind` jaq filters + kind-selector matching + step wrapping over a `$TOOLPATH_CONFIG_DIR` cache sandbox, streaming-planner recognition + streamed-output-equals-slurp equality checks). For an end-to-end check against a real Pathbase deployment, run `scripts/test-pathbase-live.sh <url>` — it does an anon round-trip in a sandboxed config dir and, if you're logged into that URL, an authed pathstash round-trip too.
- `toolpath-cli`: 0 tests (it's a one-line `path_cli::run()` shim crate that exists only so `cargo install toolpath-cli` keeps installing the `path` binary)

Validate example documents: `for f in examples/*.json; do cargo run -p path-cli -- p validate --input "$f"; done`

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

Build the site after changes: `cd site && pnpm run build` (should produce 12 pages).

## Things to know

- The `Document` enum is externally tagged -- JSON documents are wrapped in `{"Step": ...}`, `{"Path": ...}`, or `{"Graph": ...}`
- `PathOrRef::Path` is `Box<Path>` to avoid a large enum variant size difference
- The git derivation (`toolpath-git`) uses `git2` (libgit2 bindings), not shelling out to git
- Claude conversation data lives in `~/.claude/projects/` as JSONL files; `toolpath-claude` reads these directly
- `toolpath-claude`'s reader stream-parses each JSONL line (a mid-write flush can concatenate two objects on one physical line; every complete value survives). Claude 2.1.216's transcript renderer aborts on an assistant entry whose content is `[{"type":"text","text":""}]` — the projector re-emits fully empty assistant turns as Claude's own empty thinking block, and `path p import claude` derives with thinking included.
- `toolpath-claude` follows session chains by default — Claude Code rotates JSONL files on context overflow; `read_conversation` merges segments, `list_conversations` returns chain heads. `read_segment`/`list_segments` for single-file access. `ChainIndex` makes this incremental.
- Gemini CLI conversation data lives in `~/.gemini/tmp/<project>/chats/`. Main sessions sit at the top (`session-<timestamp>-<short>.json`, `kind: "main"`); sub-agents live in sibling `<full-uuid>/` directories (`kind: "subagent"`). The `<project>` slot is either a friendly name from `~/.gemini/projects.json` or the SHA-256 hex of the absolute project path; `toolpath-gemini` resolves both.
- `toolpath-gemini` treats main file + sibling sub-agent UUID dir as one conversation. Sub-agent files are folded into `DelegatedWork` with populated `turns` (unlike `toolpath-claude`, whose sub-agent turns live in separate session files and stay empty). See `docs/agents/formats/gemini.md` for the full format reference.
- The conversation IR (`toolpath-convo`) is a **closed typed set** — no catch-all `extra`/passthrough fields on `Turn`, `Compaction`, or any other IR type. `Turn.extra` was removed on main (`0452f61`, "stop smuggling source-format details through the IR") and `Compaction` never gets one; provider detail either earns a typed optional field or is accepted as lossy. Native-layer catch-alls (e.g. `toolpath-claude`'s `ConversationEntry.extra` serde-flatten) are fine — they represent the *source format*, not the IR. `WatcherEvent::Progress.data` (a live event stream, not the IR) still uses provider-namespaced keys.
- Shared derivation: `toolpath-convo` provides a provider-agnostic `ConversationView → Path` mapping via `toolpath_convo::derive_path`. New conversation providers should build on it rather than re-implementing the mapping.
- Path kinds: `toolpath::v1::PathMeta.kind` is an optional URI naming a hosted kind spec; URIs are immutable and semver-versioned. The only kind defined so far is `agent-coding-session`, currently at `https://toolpath.net/kinds/agent-coding-session/v1.2.0` (constant `toolpath::v1::PATH_KIND_AGENT_CODING_SESSION`; `…_V1_1_0`/`…_V1_0_0` name the superseded URIs); v1.2.0 adds the `conversation.compact` step type for context-compaction boundaries on top of v1.1.0's message-level token accounting, and the earlier v1.1.0 and v1.0.0 URIs stay registered (their schemas kept in `path-cli`'s `BUNDLED_KINDS`) and documented for backward compatibility. Every conversation → `Path` derivation sets it via the shared `toolpath_convo::derive_path` or each provider crate's own. Carried through the JSONL form via `PathOpen.meta` and `PathMeta` patch lines. Spec sources live in `site/kinds/<name>/<version>/{index.md,schema.json}` (schema.json is a symlink into `crates/path-cli/kinds/`, which `path p validate` bundles — all versions) and publish under `https://toolpath.net/kinds/`; the registry index is `site/kinds/index.md`. RFC: "Document Kind". JSON Schema: `$defs/pathMeta`.
- Token accounting (kind v1.1.0): two keys on `conversation.append`/`Turn`, both optional. `token_usage` = "the total for a message" (on the group's final step; `Σ` over a path = session total). `attributed_token_usage` = "this step's own attributed spend", populated only where the source genuinely reports per-step spend (its own key, so the sum is unaffected; remainder = group total − Σ attributed, computed not stored). One provider message can span several steps (Claude writes one JSONL line per content block); `Turn.group_id` groups them. `toolpath-claude` fills `group_id` from `message.id` and takes the **field-wise-max** group total (line order not trusted). Claude's per-line `usage` is a cumulative *streaming snapshot* (Anthropic streaming API: `message_start` seeds output near 0, `message_delta` is cumulative), NOT a per-block cost — so Claude emits no `attributed_token_usage`; the projector re-expands the total onto every line. `toolpath-codex` differences the cumulative `total_token_usage` (dedup-safe: never sum `last_token_usage` — Codex re-emits it stale; openai/codex #14489), attributes each per-call delta to the step it follows, and derives the round total from those attributions. pi/opencode decode all-zero wire counters as `None`. Never stamp a cumulative counter, a repeated message total, or zero-filled placeholders onto a step; never derive attribution from Claude's streaming snapshots.
- Token usage `breakdowns` (kind v1.1.0, additive): an optional third key on `TokenUsage` — a decomposition of a top-level class into named sub-classes, keyed by class (e.g. `"output"`), inner map sub-class → tokens (e.g. `breakdowns["output"]["reasoning"] = 243`). INFORMATIONAL ONLY: **never summed into any total** (the parent class already counts those tokens, so the session-total guarantee is untouched); invariant `Σ(inner) ≤ parent`; omitted when empty; rides both `token_usage` and `attributed_token_usage`. Per-provider reality: **Gemini** reports `thoughts` (reasoning) as an additive sibling that the derivation used to **drop** (under-counting output) — it's now folded into `output_tokens` *and* recorded as `breakdowns["output"]["reasoning"]`, with the projector un-folding it on the reverse path for a lossless round-trip (`Some(0)` preserved as a real Gemini-3 zero-reasoning signal). **OpenCode** folds `reasoning` into output and records the same breakdown. **Codex** differences `reasoning_output_tokens` (⊆ output, cumulative) into `breakdowns["output"]["reasoning"]` on both per-step `attributed_token_usage` and per-round `token_usage`. **Claude** records no breakdown (its JSONL `usage` doesn't itemize thinking tokens).
- Compaction boundary (kind v1.2.0): the `conversation.compact` step type — a context-compaction boundary recorded as its own step between the turns it separates (turns after the boundary parent on it, so the `head`-ancestry walk crosses it in order). `structural` fields are all optional but `type`: `trigger` (`auto`|`manual`), `summary`, `pre_tokens`, and `kept` (ids of prior turns surviving verbatim into the post-compaction window — always a contiguous parent-chain run, oldest first, whose first element is the anchor; empty = wholesale). Compaction provenance is this **closed typed set** — no catch-all `extra`; native detail richer than the contiguous run (e.g. Claude's replay-pinned messages) is deliberately not carried and round-trips are lossy beyond these fields. The IR carries the anchor as `Compaction.kept_from`; `toolpath_convo::expand_kept` expands it to the run. Steps whose parents were rewired by derive-splicing also carry `source_parent` (string|null) on their `conversation.append`/`conversation.compact` structural change. It is not a turn (no `text`/`role`/`tool_uses`); `step.actor` is `tool:<provider>`. Populated by each provider's `Item::Compaction` derivation and projected back to harness markers.
- Pi provider: `toolpath-pi` reads Pi session JSONL from `~/.pi/agent/sessions/`. Sessions use a tree (id/parentId) in a single file, and may link to a parent file via `parentSession` in the header. The tree is preserved as a DAG in the derived `Path`. `model_change`/`thinking_level_change`/`label` entries map to typed `Item::Event`s (chain links — `expand_kept` passes through them) and project back; the projector threads the chain's model context into assistant `provider`/`api` so pi restores the resumed session's model (verified in pi 0.72). Compaction `details`/`fromHook` are deliberate loss.
- Codex provider: `toolpath-codex` reads Codex CLI rollout files from `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Sessions are date-bucketed (not project-keyed). File-change fidelity is excellent — Codex's `patch_apply_end` events carry either the unified diff (for updates) or the full file content (for adds), so the derived `Path` gets a real `raw` perspective on every file artifact. Compaction (2026-07 payload): `compacted.message` is empty — the summary lives encrypted in `replacement_history` (unrecoverable) and the surviving turns are a prefix-keep the suffix-anchored `kept_from` contract cannot represent, so the boundary derives wholesale with no summary; the projector follows every `compacted` line with the `event_msg`/`context_compacted` marker the TUI renders its "Context compacted" row from, and places the opening `turn_context` right before the first real prompt (codex titles a backfilled session from the first user message; the env-context XML must not win). `path resume` mints a deterministic fresh session id — reusing the source id made imports ambiguous and clobbered the native session's `threads` row (all verified in codex 0.144.4). See `docs/agents/formats/codex.md` for the full format reference.
- Copilot provider (**preview, schema reverse-engineered**): `toolpath-copilot` reads the GitHub Copilot CLI (`@github/copilot`) `events.jsonl` stream from `~/.copilot/session-state/<id>/` (root overridable via `COPILOT_HOME`; also globs the legacy `history-session-state/`). Sessions are global (id-keyed dirs), not project-keyed. The `events.jsonl` schema is **undocumented** but was **verified against a first-hand capture at `copilotVersion` 1.0.67**: envelope is `{type, data, id, timestamp, parentId}`; cwd + git (branch/remote/`headCommit`) live under `session.start`'s `data.context` (primary; `workspace.yaml` is the fallback, parsed by a tolerant key-scan with no YAML dep); tool calls carry `toolCallId`/`toolName`/`arguments` with results under `result.content`; `assistant.message` carries `reasoningText` (→ thinking) and per-message `outputTokens` (summed for the session total). The reader stays deliberately tolerant (payload inline / under `data` / under `payload`; multiple key spellings; unknown events preserved as `ConversationEvent`s). `subagent.*`, `session.shutdown` (`tokenDetails`), and **context compaction** are now observed via real captures at 1.0.68: a successful `session.compaction_complete` becomes the typed `Item::Compaction` (`summary` ← `summaryContent`, `pre_tokens` ← `preCompactionTokens`; wholesale — Copilot reports removed-message counts, not surviving ids), `success: false` and the `session.compaction_start` bookkeeping marker stay generic events, the projector emits the observed pair back, and the summary mirrors to `checkpoints/NNN-*.md`. Event types not yet observed (`skill.invoked`, `hook.*`, `abort`, mode/plan changes) remain unverified. Wired into the CLI both directions — forward (`path p import/list/show copilot`, `path share`) and reverse via `CopilotProjector` (`path p export copilot`, `path resume`; the projector emits `events.jsonl` in the observed shape, remaps foreign tool names via `native_name`, and — for resume — writes `session-state/<id>/` + a `session-store.db` `sessions` row, INSERTing only a fresh id). **Verified in copilot 1.0.67**: a projected session loads and resumes in the real `copilot --resume`. Reaching that mapped the loader's writer contract (UUID `id`/`parentId`, offset-ISO timestamps on every event, `turnId`/`messageId` on turn-scoped events, non-empty `toolCallId`, full `session.start` shape, and `subagent.*` fields `agentName`/`agentDisplayName`/`agentDescription`/`toolCallId`) — documented with verbatim rejection messages in `docs/agents/formats/copilot-cli/writing-compatible.md`; verified on a small session and a 5817-event sub-agent session. Also validated in isolation by the cross-harness conformance matrix (`crates/path-cli/tests/cross_harness_matrix.rs`, fixture `test-fixtures/copilot/`) + a round-trip test. File fidelity is **Codex-grade**: `edit`/`create` completes embed the real file-state diff inline (`result.detailedContent`), which upgrades the arg-derived `FileMutation.raw_diff`; sub-agents are `task` tool calls with thin `subagent.*` markers sharing the `toolCallId` (delegations pair by it). Session token totals take `output` from the summed per-message `outputTokens` and merge input/cache totals from `session.shutdown`'s `tokenDetails` (Copilot reports only `output` per-message, so input+cache would otherwise be dropped); no per-turn attribution. Full format reference + verification checklist: `docs/agents/formats/copilot-cli/`.
- opencode provider: `toolpath-opencode` reads a SQLite database at `~/.local/share/opencode/opencode.db` (opened read-only). Each session's messages and 12 typed part variants (text, reasoning, tool, step-start/-finish, snapshot, patch, file, agent, subtask, retry, compaction) land as one step per message with tool invocations attached. File diffs come from a sibling bare git repo at `snapshot/<project-id>/[<sha1(worktree)>]/` via `git2` tree↔tree diffs — opencode respects the user's `.gitignore`, so changes under gitignored paths fall back to tool-input-derived structural changes with no `raw` perspective. Project id is the SHA of the repo's first root commit. See `docs/agents/formats/opencode.md` for the full format reference.
- Cursor (IDE) provider: `toolpath-cursor` reads Cursor.app's global `state.vscdb` SQLite (opened read-only) at `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb` (macOS; `~/.config/Cursor/...` on Linux). Composers, bubbles, and content-addressed file blobs are stored as key-prefixed rows in the `cursorDiskKV` table (`composerData:<uuid>`, `bubbleId:<comp>:<bubble>`, `composer.content.<hash>`) plus a `composer.composerHeaders` index blob in `ItemTable`. The full tool-dispatch enum (53 entries, ids 0–63) is extracted from the workbench bundle into `TOOL_TABLE` for round-trip-correct numeric ids — projector-written composers load back into Cursor.app's UI with the right tool rendering. The cursor-agent CLI uses a different per-chat protobuf store at `~/.cursor/chats/<wsHash>/<chatId>/store.db` that this crate does not yet parse — that's deferred to a future `toolpath-cursor-cli` companion. See `docs/agents/formats/cursor.md` for the full format reference.
- Format references for the agent on-disk formats we derive from live at `docs/agents/formats/`. The Claude Code format (`~/.claude/projects/…` JSONL) gets the deepest treatment — twelve focused docs at `docs/agents/formats/claude-code/` covering envelope, entry types, tools, session chains, compaction, writing-compatible JSONL, a linear walkthrough, and a version-keyed changelog. Sibling single-file references: `codex.md`, `gemini.md`, `opencode.md`. Keep them in sync with their derive crates when fields or behaviors change.
- Interactive session selection: `path p import <provider>` (claude / gemini / pi / codex / opencode) auto-launches a fuzzy picker when stdin and stderr are TTYs and no `--session` was given. Backend: external `fzf` if on `$PATH`, otherwise the embedded skim picker (default-feature `embedded-picker`, defined in `crates/path-cli/src/skim_picker.rs`). Multi-select (TAB) produces a `Graph` document; single-select produces a `Path`. The picker uses `path show <provider> --…` as its `--preview` command. When neither backend can run (no TTY, or `--no-default-features` AND no `fzf`), it falls back to most-recent (with `--project`) or prints the manual recipe (without). `path p list <provider> --format tsv` is the documented machine-readable surface — column 1 is the project (for claude/gemini/pi) or session id (for codex/opencode), and the trailing column carries `first_user_message` so consumers can fuzzy-match by topic.
- Conversation metadata title field: `toolpath-claude::ConversationMetadata`, `toolpath-gemini::ConversationMetadata`, and `toolpath-pi::SessionMeta` all expose `first_user_message: Option<String>` — the first non-empty user-prompt text. Populated cheaply during the metadata pass (single-pass for Claude/Gemini; one extra short read for Pi). Used by the picker UI but useful for any "list sessions by topic" surface.
- `path share` is the one-shot equivalent of `path p import <harness> | path p export pathbase`. It probes installed agent harnesses (claude/gemini/codex/opencode/pi), aggregates their sessions into a single fzf picker, and ranks rows whose project (claude/gemini/pi) or recorded cwd (codex/opencode) canonicalizes to the current directory at the top. `--harness` narrows the picker to one provider; `--harness X --session Y` (and `--project P` for keyed providers) skips the picker entirely. Pathbase flags (`--url`, `--anon`, `--repo`, `--slug`, `--public`) match `path export pathbase`. By default the derived doc is written to the cache like `import` does; pass `--no-cache` to skip.
- `path resume <input>` is the inverse of `path share`. It accepts a Pathbase URL, an `owner/repo/slug` shorthand, a local toolpath JSON file, or a cache id; resolves it (caching URL fetches under `~/.toolpath/documents/` unless `--no-cache`); validates that the document is a single agent-bearing `Path`; then opens an `fzf` harness picker (skipped with `--harness X`). The picker pre-selects the source harness inferred from `path.meta.source` (`claude-code`/`gemini-cli`/`codex`/`opencode`/`pi`) when it's installed. After picking, `path resume` projects the session into the harness's on-disk layout under the chosen working directory (default: shell cwd; override with `-C, --cwd P`) and `execvp`'s the harness's resume command (`claude -r <id>` / `gemini --resume <id>` / `codex resume <id>` / `opencode --session <id>` / `pi --session <id>`). On Windows it spawns and waits, propagating the exit code. The exec is mockable via `cmd_resume::ExecStrategy` — production uses `RealExec`; integration tests use `RecordingExec` to capture the recipe without launching a real harness.
- `path query` does not load the whole cache into memory when it can avoid it. `crates/path-cli/src/query/plan.rs` parses the jaq filter into jaq's own AST (`jaq_core::load::parse::Term`) and classifies it into a `Plan`: `PerFileStream` (`.[] | g` element-wise work — run per document, print as you go), `Decompose { reduce }` (algebraic aggregations — run the whole filter per file, concatenate the per-file outputs, then run a derived combine: `map`→`add` (array concat), top-N `sort_by(k)|.[:N]`→`add | sort_by(k)|.[:N]`, `length`→`add` over exact integer counts), or `Slurp` (the always-correct whole-array fallback). Recognition is conservative — a non-distributive prefix like `unique`/`group_by` slurps, and so do scalar `add` (float sums re-associate across per-file partials), `min`/`max` (`[] | min == null` poisons the merge), and any unrecognized tail — so **the planner never changes an answer** — `crates/path-cli/src/query/filter.rs` tests assert streamed output equals slurp byte-for-byte. `filter::execute` compiles the filter once (jaq's compiled `Filter` is fully owned, so it's reused across files) and drives the plan; `mod.rs::stream_files` yields one document's wrapped steps at a time. `TOOLPATH_QUERY_EXPLAIN=1` prints the chosen plan to stderr. No user-facing flag — it's automatic. Tie-break caveat: a streamed top-N matches slurp's *ranking*, but boundary ties may resolve to different specific rows.
