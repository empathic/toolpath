# Changelog

All notable changes to the Toolpath workspace are documented here.

## toolpath-convo 0.12.0: session-level `user_actor` override — 2026-06-30

Adds `DeriveConfig.user_actor: Option<String>` so a provider can set a
session-level actor string for user turns (e.g. a channel-aware
`human:whatsapp/<peerId>`) instead of the default `human:user`. Additive
and backward-compatible — existing callers using `..Default::default()`
are unaffected. Enables the multi-channel human identity in the new
`toolpath-openclaw` provider.

## Token usage: once per message, with per-step attribution + kind v1.1.0 — 2026-06-17

Fixes token over-counting in derived documents (~3× output-token
inflation on real Claude sessions, unbounded on Codex) and adds per-step
token attribution where the source genuinely reports it (Codex). Two
over-counting bugs, one spec gap, plus a capability the corrected reads
make possible. Verified against every Claude session and all Codex
sessions on disk, and cross-checked against the Anthropic streaming API
reference and OpenAI's codex issue tracker.

- **Claude**: Claude Code writes one JSONL line per content block of an
  assistant API message, repeating the message-level `usage` on every
  line. `toolpath-claude` emitted one step per line, each carrying the
  full usage — so summing `token_usage` per step over-counted by the
  block count, and the disambiguating `message.id` was dropped.
- **Codex**: `toolpath-codex` stamped the *cumulative* session counter
  (`total_token_usage`) onto each assistant turn instead of per-step
  spend, so per-step sums grew quadratically.

Core model (kind `agent-coding-session` **v1.1.0**, both fields optional
so any producer can populate per-step attribution later with no further
kind version):

- `token_usage` always means **the total for a message**, on the
  group's final step (`Σ token_usage` over a path = session total).
- `attributed_token_usage` (new) is **this step's own attributed
  spend**, on its own key so the sum above is unaffected. Whether a
  number is a total or a share is structural (the key), never
  positional. The unattributed remainder
  (`group token_usage − Σ attributed`) is computed by consumers, never
  recorded — stored values stay verbatim source observations.
- `breakdowns` (new, optional) is a **decomposition of a top-level
  class into named sub-classes** — keyed by the class being broken down (e.g.
  `"output"`), inner map sub-class → tokens (e.g. `{"output":
  {"reasoning": 243}}`). It is **informational and never summed into
  any total** — the parent class already counts those tokens — so the
  session-total guarantee is untouched. Invariant: `Σ(inner) ≤` the
  parent class's value; the field is omitted when empty. It rides both
  `token_usage` and `attributed_token_usage`.

Changes:

- `toolpath_convo::TokenUsage` gains `breakdowns`
  (`BTreeMap<class, BTreeMap<sub-class, tokens>>`); the kind
  `tokenUsage` `$def` gains a matching optional `breakdowns` property.
- **Gemini under-count FIX**: Gemini reports `thoughts` (reasoning) as
  an additive sibling of `output_tokens` that the derivation was
  **dropping** — so Gemini output totals were under-counted by the
  reasoning spend. `thoughts` is now **folded into `output_tokens`**
  (correcting the total) *and* recorded under
  `breakdowns["output"]["reasoning"]`; the projector **un-folds** it on
  the reverse path for a lossless round-trip (`Some(0)` is preserved as
  a real Gemini-3 zero-reasoning signal, not collapsed to absent).
- **OpenCode**: continues folding `reasoning` into `output_tokens`, and
  now also records it under `breakdowns["output"]["reasoning"]`.
- **Codex**: `reasoning_output_tokens` (a subset of `output_tokens`,
  cumulative → differenced like the other counters) is surfaced under
  `breakdowns["output"]["reasoning"]` on both the per-step
  `attributed_token_usage` and the per-round `token_usage`.
- **Claude**: records no breakdown — its JSONL `usage` does not itemize
  thinking tokens.
- `toolpath_convo::Turn` gains `group_id` (grouping key) and
  `attributed_token_usage`. `derive_path` writes `token_usage` once per
  `group_id` group and `attributed_token_usage` on each step that has
  it; `extract_conversation` reads both back.
- `toolpath-claude`: a split message's lines carry `message.usage` as a
  **cumulative streaming snapshot**, not a per-line bill — per the
  Anthropic streaming API, `message_start` seeds `output_tokens` near
  zero and each `message_delta` reports the running cumulative total
  (confirmed across every session sampled: input/cache constant, output
  climbing to the final-line total; ~27% of multi-line messages vary).
  Each `group_id` run is reduced to the **field-wise maximum** total
  (never under-counts whatever the line order) on its final turn. The
  intermediate snapshots are flush-time artifacts, *not* per-block costs
  (a real prose block routinely shows `output_tokens: 1`), so Claude
  emits **no** `attributed_token_usage`. `total_usage` is deduped by
  group; the projector re-expands the total onto every line of a split.
- `toolpath-codex`: per-step spend is the increase in the cumulative
  `total_token_usage` since the previous count — **differencing the
  cumulative is dedup-safe**, where summing `last_token_usage` would
  double-count because Codex re-emits a stale `last_token_usage` on
  repeated `token_count` events (a documented trap: openai/codex #14489,
  #17539). Each per-call delta is attributed to the step it follows as
  `attributed_token_usage`; a round's `token_usage` total is the sum of
  its steps' attributions (one source of truth — total and shares cannot
  drift). The projector emits a `turn_context` per group and a cumulative
  `token_count` after each step, so grouping and attribution survive the
  round-trip.
- `toolpath-pi` and `toolpath-opencode` decode absent/all-zero wire
  usage counters as `token_usage: None` ("spend unknown") instead of
  `Some(zeros)` — their wires require usage fields, which
  foreign-source projections zero-fill.
- `PATH_KIND_AGENT_CODING_SESSION` now points at v1.1.0;
  `PATH_KIND_AGENT_CODING_SESSION_V1_0_0` names the old URI. `path p
  validate` bundles both schemas. The v1.0.0 spec page gains an erratum
  documenting the historical duplication (consumers of v1.0.0 documents
  still need dedup heuristics; the byte-identical-tuple heuristic does
  not repair Codex documents).

Crates bumped (every crate that depends on `toolpath`, matching the
domain-rename precedent since the emitted kind URI changes): `toolpath`
0.7.0, `toolpath-convo` 0.11.0, `toolpath-git` 0.6.0, `toolpath-github`
0.6.0, `toolpath-claude` 0.12.0, `toolpath-gemini` 0.6.0,
`toolpath-codex` 0.6.0, `toolpath-opencode` 0.5.0, `toolpath-cursor`
0.2.0, `toolpath-pi` 0.6.0, `toolpath-dot` 0.5.0, `toolpath-md` 0.7.0,
`path-cli` 0.14.0, `toolpath-cli` 0.14.0. `pathbase-client` is
unaffected.

## toolpath-claude 0.11.1 + path-cli 0.13.1 + toolpath-cli 0.13.1: derive `project_path` from the file's parent directory — 2026-06-09

`ConversationReader::read_conversation_metadata` used to set
`ConversationMetadata.project_path` from the JSONL's first `cwd`
entry. That broke for sessions projected onto this machine from
elsewhere (e.g. `path resume` of a Pathbase upload): the recorded
`cwd` reflected the original author's machine, and downstream
`read_conversation(meta.project_path, meta.session_id)` calls routed
at a directory that didn't exist locally, crashing with
`Conversation not found`.

Fix: derive `project_path` from the file's parent directory instead
(unsanitized). The parent directory is the on-disk locator by
construction — it's where the file actually lives — so it always
round-trips correctly. JSONL `cwd` is no longer read for any metadata
purpose. The chained variant in
`ClaudeConvo::read_conversation_metadata` drops its now-dead
`project_path` accumulator.

Public API unchanged. `path-cli` and the `toolpath-cli` shim bump to
0.13.1 so a release ships the fix to users on `cargo install path-cli`
/ `cargo install toolpath-cli`.

## Domain rename: toolpath.dev → toolpath.net + hosted install.sh — 2026-06-04

The canonical domain for the site, kind URIs, and schema `$id`s moves from
`toolpath.dev` to `toolpath.net`. This is a breaking change: any saved
`Path` whose `meta.kind` points at the old URI is now treated as a generic
path by `path validate` (base-schema-only; no kind-level constraints
applied). Producers (the shared `toolpath_convo::derive_path` and every
provider crate built on it) now emit the new URI.

- New constant value: `toolpath::v1::PATH_KIND_AGENT_CODING_SESSION =
  "https://toolpath.net/kinds/agent-coding-session/v1.0.0"`.
- The base JSON Schema's `$id` is now
  `https://toolpath.net/schema/toolpath.schema.json`; the kind schema's
  `$id` and `const` constraint move to `toolpath.net` as well.
- `scripts/install.sh` is now served from the site at
  `https://toolpath.net/install.sh` (eleventy passthrough). The
  documented one-liner is
  `curl -fsSL https://toolpath.net/install.sh | bash`.

Crates bumped (every crate that depends on `toolpath`, directly or
transitively): `toolpath` 0.6.0, `toolpath-convo` 0.10.0, `toolpath-git`
0.5.0, `toolpath-github` 0.5.0, `toolpath-claude` 0.11.0,
`toolpath-gemini` 0.5.0, `toolpath-codex` 0.5.0, `toolpath-opencode`
0.4.0, `toolpath-dot` 0.4.0, `toolpath-md` 0.6.0, `toolpath-pi` 0.5.0,
`path-cli` 0.13.0, `toolpath-cli` 0.13.0. `pathbase-client` is
unaffected.

## Cursor (IDE) provider — 2026-06-04

`toolpath-cursor` 0.1.0 (new crate). Reads Cursor.app's bubble store
(`state.vscdb` SQLite at `~/Library/Application Support/Cursor/User/globalStorage/`)
— composer rows, bubble rows, content-addressed `composer.content.<hash>`
blobs, and the `composer.composerHeaders` index. Implements
`ConversationProvider` and derives via `toolpath-convo`'s shared
`derive_path`. Round-trips back to a Cursor-loadable composer via
`CursorProjector` with full `TOOL_TABLE` coverage (53 entries, ids 0–63)
extracted from the workbench bundle for round-trip-correct numeric tool
ids. The cursor-agent CLI uses a different protobuf store at
`~/.cursor/chats/<wsHash>/<chatId>/store.db` and is deferred to a future
`toolpath-cursor-cli` companion.

`path-cli` 0.12.0. Wires cursor into every plumbing surface: `p import
cursor`, `p export cursor`, `p list cursor`, `show cursor`, `share`,
`resume`, and the cross-harness matrix test. `p incept` is now a
provider subcommand surface (`p incept claude` / `p incept cursor`),
replacing the implicit-claude form. `p export cursor --project <ws>`
writes the composer into Cursor.app's SQLite so the chat sidebar can
load it on next workspace open.

For projected sessions to load + render correctly in Cursor.app's
chat, the projector emits the full native field set on each bubble
(48 empty arrays, 6 booleans, capabilities array, `context` skeleton,
`conversationMap`/`codeBlockData`/`originalFileStates`/`usageData: {}`,
and `selectedModels[0].parameters: []`). The full enumeration —
including the dev-console errors that surface when each is missing —
lives in `docs/agents/formats/cursor.md` under "Projecting bubbles
Cursor will render". Diffs render via reconstructed before/after
content blobs (hunks-only when only a `raw_diff` is available, full
content when provider snapshots it).

Cross-provider derive: a Cursor composer's `edit_file_v2` tool calls
land on `Turn.tool_uses` with `tu.input.{file_path, content,
old_string, new_string}` populated from the resolved content blobs,
so projecting to Claude, Codex, opencode, etc. produces an `Edit` /
`Write` tool_use whose UI renders the diff.

## Embedded fuzzy picker — fzf is now optional — 2026-05-27

`path-cli` 0.12.0. The CLI no longer requires the external `fzf` binary
for its interactive flows (`path share`, `path resume`, `path p import
<provider>`). When `fzf` is on `PATH` we still prefer it — so users'
fzf config and keybindings keep working — but absence is no longer a
reason to bail out of an interactive flow.

- New `skim`-backed embedded picker in `crates/path-cli/src/skim_picker.rs`,
  routed through the existing `fzf::pick` API so all call sites work
  unchanged. Same `{1}`/`{2}` preview placeholders and `--with-nth`
  column grammar; existing `path show ...` preview commands work.
- Gated by the `embedded-picker` Cargo feature, on by default. Build
  with `--no-default-features` to drop skim and shave ~2 MB off the
  release binary; the CLI falls back to the manual-recipe printout
  when neither backend is available.
- New global `--picker auto|fzf|skim` flag forces a backend (default
  `auto`). `--picker skim` overrides external fzf even when it's on
  PATH; `--picker fzf` errors out when fzf isn't installed.
- `fzf::available()` is now TTY-only — `fzf::external_fzf_available()`
  and `fzf::embedded_picker_available()` split out the two halves so
  callers (and `print_recipe`) can describe what's actually missing.

## Readable conversation previews — kind-aware rendering + ANSI — 2026-05-27

`toolpath-md` 0.5.0 and `path-cli` 0.12.0. Renders an `agent-coding-session` path as a flat conversation
transcript instead of the generic step/DAG timeline. The active (head-ancestry)
turns render in causal order, speaker-labeled (`**User:**` / `**Assistant:**`),
with the per-step UUID headers, timestamps, parent links, dead-end markers, and
attachment/event steps dropped; abandoned branches collapse to a count.

Full detail shows each turn's text, reasoning (`thinking`), tool calls with
inputs and results, delegations, `file.write` diffs, and a compact
stop/tokens/cwd line. Summary is prose-focused: it keeps user prompts verbatim,
truncates agent responses, drops text-less turns entirely (no bare
`**Assistant:**`), and collapses runs of tool calls into a per-name breakdown
(`*tools: Read (3), Write (1)*`). Paths without a recognized kind are unchanged.

`path show` gains `--ansi`, which renders that Markdown as ANSI-styled terminal
output (bold speakers, dim metadata, cyan inline code, red/green diff lines)
rather than raw markers. The fzf preview panes for `path import` / `path share`
use it, with fzf's `wrap-word` preview wrapping so lines break at word
boundaries and reflow on resize (requires fzf ≥ 0.59). `path show` without the
flag still emits plain Markdown.

Also fixes `truncate_str` slicing on a byte index that could fall inside a
multibyte character (it now truncates by character).

## `meta.kind` — new path-kind field; hosted kind spec registry — 2026-05-27

`toolpath` 0.5.0, `toolpath-convo` 0.9.0, `toolpath-claude` 0.10.0,
`toolpath-gemini` 0.4.0, `toolpath-codex` 0.4.0, `toolpath-opencode` 0.3.0,
`toolpath-pi` 0.4.0, and `path-cli` 0.12.0.

New optional `meta.kind` field on `Path` (`toolpath::v1::PathMeta::kind`,
plus the `toolpath::v1::PATH_KIND_AGENT_CODING_SESSION` constant). `kind` is a
URI naming a *kind specification* — a hosted, immutable, semver-versioned
contract describing the additional shape a path follows on top of the base
format. Absent or unrecognized `kind` ⇒ generic path; existing documents
parse and validate unchanged.

The first defined kind is `https://toolpath.net/kinds/agent-coding-session/v1.0.0`,
which marks a path as an AI coding conversation (each step is a
`conversation.append` change carrying that turn's `role`, `text`, and so
on; `meta.source` names the producing harness). Every conversation → `Path`
derivation now sets it — the shared `toolpath_convo::derive_path` and each
conversation provider crate's own. The JSONL form carries `kind` through
`PathOpen.meta` and `PathMeta` patch lines.

Kind specs are sourced under `site/kinds/<name>/<version>/` (Markdown spec
plus an additive JSON Schema fragment) and published under
`https://toolpath.net/kinds/`. A registry index lives at
`https://toolpath.net/kinds/`. The Toolpath RFC ("Document Kind") and the
JSON Schema (`$defs/pathMeta`) reference the registry rather than carrying
kind-specific contracts inline.

`path validate` is now kind-aware: the kind schemas are bundled into the CLI
and, for each path carrying a recognized `meta.kind`, the matching kind schema
is applied on top of the base schema. An unrecognized `kind` validates against
the base schema only.

Cascading minor bumps with no source changes of their own — only the
dependency on the new `toolpath 0.5` major: `toolpath-git` 0.4.0,
`toolpath-github` 0.4.0, `toolpath-dot` 0.3.0.

## Pathbase 1.1 wire-model refresh — graphs-by-UUID — 2026-05-21

`pathbase-client` 0.2.0 and `path-cli` 0.11.0. **Breaking** (pre-1.0).
The Pathbase HTTP API was restructured around graphs-by-UUID; the
client was regenerated from the live `pathbase-dev` OpenAPI spec and
the CLI was updated to match.

- **`pathbase-client` 0.2.0.** Regenerated from
  `https://pathbase-dev.fly.dev/api/v1/openapi.json`. All endpoints
  now live under `/api/v1/u/{owner}/repos/{repo}/...` (was
  `/api/v1/repos/...`). Graphs are the only addressable upload shape
  — the path-specific endpoints are gone. New tri-state `Visibility`
  enum (`public` / `unlisted` / `private`) replaces the old
  `is_public: bool`. New `ApiErrorResponse` is `{code, error}` with a
  typed `ApiErrorCode` enum. Anon uploads go to
  `/api/v1/u/anon/repos/pathstash/graphs`. The hand-rolled `/logout`
  endpoint is gone; revoke now goes through
  `GET /auth/sessions` + `DELETE /auth/sessions/{id}`. Newly exposed:
  `update_me`, `update_repo`, `update_graph_visibility`,
  `update_repo_visibility`, `list_graph_paths`, `get_graph_path`,
  `update_graph_path`, `delete_graph_path`, `get_graph_path_chat`.
- **`path-cli` 0.11.0.** `path share` / `path export pathbase`:
  `--slug` is renamed to `--name` (kept as a hidden alias). The flag
  is a display label only; graphs are addressed by UUID server-side,
  so the slug never appears in the share URL. `--public` still maps
  to public-vs-unlisted (the historical "secret" semantic). The
  printed share URL now comes from the server response rather than
  being reconstructed locally. `path import pathbase` / `path resume`
  accept `https://host/u/<owner>/repos/<repo>/graphs/<uuid>` URLs
  (plus the short `https://host/<owner>/<repo>/graphs/<uuid>` and
  legacy `paths` delimiter for back-compat); the trailing identifier
  must parse as a UUID — old slug-style refs no longer resolve.
  `path auth logout` revokes the current session via the sessions
  endpoint before clearing local credentials.

## Plumbing/porcelain split — `path p …` — 2026-05-20

`path-cli` 0.10.0. **Breaking** (pre-1.0). The lower-level operations
that compose into the day-to-day flows are now grouped under a single
`path p …` subcommand (`p` for "plumbing"), keeping the top-level
`--help` focused on the porcelain (`show`, `share`, `resume`, `query`,
`auth`, `haiku`).

- New canonical surface under `path p`: `list`, `import`, `export`,
  `cache`, `render`, `merge`, `validate`, `derive`, `project`,
  `incept`, `track`.
- **Hard removal at the top level.** `path import`, `path export`,
  `path cache`, `path list`, `path render`, `path merge`,
  `path validate`, `path derive`, `path project`, `path incept`, and
  `path track` no longer exist as top-level subcommands — they only
  resolve under `path p`. There is no deprecation shim. Scripts will
  fail with `error: unrecognized subcommand`; update call sites to the
  `path p X` form.
- Internal: `cmd_p.rs` owns the `PCommand` enum and dispatches to the
  existing per-command handlers. No behavioral changes to the handlers
  themselves.

## Actor validation fixes — derived paths conform to the base schema — unreleased

`derive_path` produced actor strings the base JSON Schema rejected: event steps
used `provider:<name>`, system turns used `system:<provider>`, and `Role::Other`
turns used `<role>:unknown` — none of those prefixes are in the schema's
`actorRef` vocabulary. Separately the `actorRef` pattern's name segment
disallowed `.`, so dotted model identifiers like `agent:gpt-5.5` failed
validation.

`toolpath`: `actorRef`'s name segment now allows `.`. `toolpath-convo`:
`derive_path` emits `tool:<provider>` for event steps, system turns, and
`Role::Other` turns (the role label stays in the change's `role` field), and a
new test derives a path covering every actor variant and validates it against
the embedded base schema so this can't regress. Touches `toolpath` and
`toolpath-convo`; versions to be bumped at release.

## `path resume` — one-shot resume into a coding agent — 2026-05-09

`path-cli` 0.9.0. New subcommand `path resume <input>` that fetches a
Toolpath document (Pathbase URL, `owner/repo/slug` shorthand, local
file, or cache id), validates it as a single agent-bearing `Path`,
launches an `fzf` picker over installed coding-agent harnesses
(`--harness X` skips the picker), projects the session into that
harness's on-disk layout under `-C, --cwd P` (default: shell cwd),
and `execvp`'s the harness's resume command (`claude -r <id>`,
`gemini --resume <id>`, `codex resume <id>`, `opencode --session <id>`,
`pi --session <id>`). On Windows the harness is spawned and waited on
with the exit code propagated.

Source-harness inference reads `path.meta.source` (`claude-code` /
`gemini-cli` / `codex` / `opencode` / `pi`) with actor-string
fallback; the picker pre-selects the source when it's installed.

Implementation introduces five `pub(crate)` `project_<harness>`
helpers in `cmd_export.rs` that compose the existing build + write
pairs and return the projected session id. `cmd_resume.rs` adds an
`ExecStrategy` trait (`RealExec` for production, `RecordingExec` for
tests) so the integration tests can exercise the full
resolve→pick→project pipeline without launching a real harness.

Also fixed an unrelated env-var race in
`cmd_export::tests::opencode_writes_into_db_with_project` that
cleared `$HOME` on cleanup without restoring; this had been quietly
flaking the parallel test suite.

## Conversation-stack realignment onto `toolpath` 0.4 + path-cli schema vendoring

Republish of every `toolpath-convo`-consuming crate so they pin the
current `toolpath` 0.4.x line. Source-only fix — no API changes — but
required because the previously-published satellites pinned `toolpath
^0.2`, which dragged a second `toolpath` major into any consumer's
graph that combined them with `toolpath 0.4` (and outright broke
publish-verify for `toolpath-pi` 0.3.0).

Also fixes a `path-cli` packaging bug: the schema-validation feature
added in 0.7.0 used `include_str!("../../../schema/toolpath.schema.json")`
to embed the schema, but cargo only packages files inside the crate
directory. The path resolved fine in workspace builds but fails at
`cargo publish` verify time, where the unpacked tarball has no path
back to the workspace root. Hidden by the same dry-run blind spot
(`path-cli` is always "deferred" in dry-runs because its deps are
also being published in the same wave). 0.7.0 never reached
crates.io; the latest live `path-cli` is 0.5.0.

The fix relocates the schema into the foundational `toolpath` crate
as a `pub const SCHEMA_JSON: &str`. `path-cli`'s schema validator
sources from there. `schema/toolpath.schema.json` at the workspace
root is now a symlink into `crates/toolpath/schema/` — preserves
URLs, RFC references, and `tests/schema_examples.rs` runtime reads
without duplicating the file.

### toolpath 0.4.0 → 0.4.1

- New `pub const SCHEMA_JSON: &str` exporting the canonical JSON
  Schema for Toolpath documents. Schema file moves to
  `crates/toolpath/schema/toolpath.schema.json` (inside the crate's
  package boundary so `include_str!` and `cargo publish` agree);
  workspace-root `schema/toolpath.schema.json` becomes a symlink to
  preserve external URL references and existing tests' runtime path.
- Additive (no API change otherwise).

### toolpath-convo 0.7.0 → 0.8.0

- No source change. Republished so the on-registry manifest pins
  `toolpath` 0.4 (the workspace dep was already 0.4 locally; the old
  0.7.0 manifest still recorded `toolpath ^0.2`). Every downstream
  satellite needs to follow.

### toolpath-claude 0.8.0 → 0.9.0

- Republished against `toolpath-convo` 0.8.0 / `toolpath` 0.4.

### toolpath-gemini 0.2.0 → 0.3.0

- Republished against `toolpath-convo` 0.8.0 / `toolpath` 0.4.

### toolpath-codex 0.2.0 → 0.3.0

- `toolpath-codex` 0.2.0 was published earlier in the same release
  run that bumped `toolpath` to 0.4, before this realignment was
  caught. Its on-registry manifest still references the old
  `toolpath-convo` 0.7.0 (which itself pins `toolpath ^0.2`), so any
  consumer pulling it in alongside `toolpath` 0.4 ends up with both
  `toolpath` 0.2 and 0.4 in the graph. 0.3.0 is the first release
  that resolves cleanly. Yank 0.2.0 if you want to force the upgrade.
- No source change relative to 0.2.0.

### toolpath-opencode 0.1.0 → 0.2.0

- Republished against `toolpath-convo` 0.8.0 / `toolpath` 0.4.

### toolpath-pi 0.3.0 (no version change)

- 0.3.0 failed publish-verify in the previous release run with
  `E0308: mismatched types` between `toolpath::v1::Path` 0.2.0 and
  0.4.0 — `toolpath-pi`'s `derive_path` is the only satellite that
  delegates straight through to `toolpath_convo::derive_path` at the
  return-type boundary, so it was the one that surfaced the dual-
  version graph at compile time. With `toolpath-convo` 0.8.0 pinning
  `toolpath` 0.4, the dual graph collapses and 0.3.0 publishes
  cleanly.

### path-cli 0.8.0, toolpath-cli 0.8.0 (no version change)

- Neither was published in the failed run. Their workspace deps now
  resolve to the realigned satellite versions automatically.
- `path-cli` no longer `include_str!`s the schema directly; it
  consumes `toolpath::SCHEMA_JSON` instead, which fixes the
  publish-verify failure that was masked by the dry-run blind spot.

## Pathbase rewire

**Breaking** (pre-1.0). `path import pathbase` / `path export pathbase`
rewire to the real Pathbase HTTP surface — the previous
`/api/v1/traces` endpoints were never implemented server-side. New
typed `pathbase-client` crate, generated at build time from the
committed OpenAPI spec.

### pathbase-client 0.1.0 (new crate)

- New workspace member: an auto-generated typed Rust client for the [Pathbase](https://pathbase.dev) HTTP API. Derived at build time from `schema/pathbase-openapi.json` (also new in this release) via [progenitor](https://github.com/oxidecomputer/progenitor). Spec drift surfaces as a `cargo build` failure rather than runtime HTML-instead-of-JSON.
- `build.rs` downgrades the spec from OAS 3.1 to 3.0 in-memory (nullable arrays → `nullable: true`, permissive schemas for empty media-type bodies) before handing it to the generator. The committed spec stays faithful to what the server publishes.
- `scripts/refresh-pathbase-openapi.sh` repulls the spec (default `https://pathbase.dev`; override via `$PATHBASE_URL`) and pretty-prints it for stable diffs. Writes to `crates/pathbase-client/openapi.json` so the spec ships inside the published crate.
- The CLI auth-redeem endpoint (`POST /api/v1/auth/cli/redeem`) is real in production but absent from the OpenAPI spec, so it is **not** available through this client; `path-cli`'s hand-rolled redeem call remains the source of truth.

### path-cli 0.7.0 → 0.8.0

- **Breaking:** the previous `POST/GET /api/v1/traces[/:id]` endpoints were never implemented server-side. Replaced with the real surface:
  - `POST /api/v1/anon/paths` for unauthenticated public uploads (5 MB cap, rate-limited, **not listable**).
  - `POST /api/v1/repos/{owner}/{repo}/paths` for authenticated repo-scoped uploads, with `is_public` controlling visibility.
  - `POST /api/v1/repos` to create repos (used to ensure `pathstash` exists on first authed upload).
  - `GET /api/v1/repos/{owner}/{repo}/paths/{slug}/download` for fetching path contents.
- **Breaking:** `path import pathbase` no longer accepts the legacy `trc_<id>` shape. The new positional ref is either a full URL like `https://pathbase.dev/<owner>/<repo>/<slug>` or a bare `<owner>/<repo>/<slug>` triple. URLs that wrap the slug as `…/<owner>/<repo>/paths/<slug>` (the shape the anon endpoint returns) are recognized too.
- New `path export pathbase` behavior:
  - Default (logged in): writes a **secret** path under your `pathstash` repo. The repo is auto-created on first upload. Listable from your account; not publicly visible.
  - Default (not logged in): falls through to the anonymous endpoint with a stderr advisory suggesting `path auth login` for a listable upload.
  - `--anon`: force the anonymous endpoint regardless of credentials.
  - `--repo owner/name`: target a specific repo instead of `<you>/pathstash`.
  - `--slug`: override the auto-derived slug (otherwise sanitized from the toolpath document id).
  - `--public`: flip `is_public` to `true` (default: secret/unlisted).
- The URL printed on stdout now reflects how the path is actually shareable:
  - Secret upload (default): `<base>/<owner>/<repo>/paths/<uuid>` — the UUID is the share token; the slug URL would be a dead stub for non-owners.
  - Public upload (`--public`): `<base>/<owner>/<repo>/<slug>` — the listable canonical address.
  - Anonymous: whatever URL the server returns from `AnonUploadResponse` (always UUID-shaped).
- The auth flow (`path auth login` / `whoami` / `logout`) is unchanged; the redeem endpoint stays hand-rolled because the OpenAPI spec doesn't list it.
- Internally, the four documented path operations now go through the typed `pathbase-client` crate. A `OnceLock`-cached current-thread tokio runtime in `cmd_pathbase.rs` bridges sync callers into the async generated client. The whole module — auth, paths, downloads, async upload — runs on a single reqwest version (the workspace dep was unified to 0.13 to match what `pathbase-client`/`progenitor-client 0.14` generate against).
- `scripts/test-pathbase-live.sh <url>`: live-server smoke test. Always runs the same two scenarios in the same order (anon roundtrip, then authed pathstash roundtrip). Preconditions (server reachable, logged into the URL) are checked up-front; failure modes are explicit; no environment-conditional branching.

### toolpath-cli 0.7.0 → 0.8.0 (deprecation shim)

- Lockstep bump with `path-cli` 0.8.0. No behavioral change.

## `path.base` reconciliation — `branch` field, schema-validating `path validate`

**Breaking** (pre-1.0). `path.base` gains a `branch` field and `ref`'s
semantics are tightened. `path validate` now actually validates against
`schema/toolpath.schema.json`, not just the Rust types.

The `commit` field on `path.base` that several example fixtures carried
since the simplification commit was being silently dropped on every
serde round-trip — `Base` had no slot for it. The fix splits those two
distinct pieces of state into proper fields:

- `ref` is the state identifier the origin uses to name a specific
  reproducible state (commit hash, revision number, tag, changeset ID,
  etc.). Branch names no longer overload this slot.
- `branch` is the branch the path was opened against, when one applies.

`toolpath-github`'s derive was the load-bearing bug: it was writing the
PR base's branch name into `ref` and discarding the SHA. After this
change `ref` carries the SHA and `branch` carries the branch name.

### toolpath 0.3.0 → 0.4.0

- `Base` gains `branch: Option<String>` plus a `with_branch` builder.
  Existing `Base::vcs` and `Base::toolpath` constructors still apply;
  they default `branch` to `None`.

### toolpath-github 0.2.1 → 0.3.0

- **Behavioral change.** `Base.ref` is now populated from
  `pr.base.sha` (the commit hash); `Base.branch` is now populated from
  `pr.base.ref` (the branch name). Documents derived by previous
  versions had the SHA missing and the branch name in `ref` —
  re-deriving any PR will produce the corrected shape.

### toolpath-git 0.2.0 → 0.3.0

- `Base.branch` is now populated from the branch spec name.

### toolpath-codex 0.1.0 → 0.2.0

- `Base.branch` is now populated from `session_meta.git.branch` when
  present.

### toolpath-md 0.3.0 → 0.4.0

- Markdown rendering surfaces `Base.branch` alongside `ref`:
  `**Base:** <uri> @ <ref> (<branch>)`. YAML rendering adds
  `base_branch:` line.

### path-cli 0.6.0 → 0.7.0

- New `jsonschema` runtime dep. `path validate` now schema-validates
  canonical `.path.json` documents against the embedded
  `schema/toolpath.schema.json` after the type round-trip; previously
  it only round-tripped through the Rust types (which silently drop
  unknown fields). JSONL still validates via strict streaming parse.
- New `tests/schema_examples.rs` integration test schema-validates
  every `examples/*.json` fixture so future drift is caught at
  `cargo test` time.

### toolpath-cli 0.6.0 → 0.7.0 (deprecation shim)

- Lockstep bump with `path-cli`. No behavioral change.

### Schema (`schema/toolpath.schema.json`)

- `base.branch` added (optional). `base.uri` is the only required
  field.
- `pathIdentity.base` is no longer required, matching the Rust
  `Option<Base>` and the RFC's minimal-step example.
- Descriptions softened: `base` is an "origin identifier" rather than
  strictly "VCS reference"; `ref`/`branch` document what they are
  without claiming the field can only describe a VCS state.

### Examples

- The four PR-shaped fixtures (`path-01-pr.path.json`,
  `path-03-signed-pr.path.json`, `graph-01-release.json` × 2 paths)
  carry their original commit SHAs in `ref` again, with `"main"` moved
  to `branch`. `.path.jsonl` siblings updated to match.
- `path-02-local-session.path.json` had its standalone `commit` field
  renamed to `ref`.

## Format simplification — single-root Graph

**Breaking.** `Graph` is now the only root document type. Every `.path.json`
file deserializes to a `Graph`; every `.path.jsonl` file is a single-path
`Graph` at the boundary. The previous three-variant `Document` enum
(`{"Step": …}` / `{"Path": …}` / `{"Graph": …}` envelopes) is removed.

What was a single Step or single Path at the root is now wrapped in a
single-path Graph: `Graph { graph: { id }, paths: [Path { …, steps: [Step] }] }`.
This unifies file shape — one schema, one parser path, no envelope to detect.

### toolpath 0.3.0

- **Remove `Document` enum.** The new root type is `Graph`. All `Graph::*`
  helpers (`from_json`, `to_json`, `to_json_pretty`) plus `Graph::from_path`,
  `Graph::single_path`, `Graph::into_single_path` cover the previous
  `Document` surface and add ergonomic single-path lifts.
- **JSONL.** `Graph::from_jsonl_*` / `Graph::to_jsonl_*` are the file-level
  API. They wrap a single inline `Path` as a single-path `Graph`. The
  underlying line-streaming machinery on `Path` is unchanged. New
  `JsonlError::NotSinglePathGraph` flags multi-path graphs at write time.

### toolpath-git 0.2.0

- `derive` returns `Graph` (was `Document`). A single branch yields a
  single-path graph; multiple branches yield a multi-path graph.

### toolpath-dot 0.2.0

- `render(&Graph, …)` replaces `render(&Document, …)`. Single-path graphs
  render through the existing path-level layout; multi-path graphs use the
  cluster layout. `render_step`, `render_path`, `render_graph` remain.

### toolpath-md 0.3.0

- `render(&Graph, …)` replaces `render(&Document, …)`. Same single-path /
  multi-path dispatch.

### toolpath-pi 0.3.0

- `derive_project` returns `Graph` (was `Document::Graph(...)`).

### path-cli 0.6.0

- All commands (`validate`, `render`, `query`, `merge`, `import`, `export`,
  `track`, `cache`) read and write `Graph` documents at file boundaries.
- `export claude` / `export gemini` reject multi-path graphs with a clear
  error — projection requires exactly one inline path.
- `examples/` rewritten as graph-rooted JSON. `step-NN.json` examples are
  now single-path single-step graphs; `path-NN.path.json` are single-path
  graphs; `graph-01-release.json` drops its envelope.
- Schema (`schema/toolpath.schema.json`) collapses the root `oneOf` of
  three envelopes into a direct `$ref` to the `graph` definition.

### toolpath-cli 0.6.0 (deprecation shim)

- Bumped in lockstep with `path-cli` 0.6.0 (its only dependency). No
  behavioral change.

## path-cli 0.5.0 + toolpath-cli 0.5.1 + workspace re-alignment

### path-cli 0.5.0 (new crate name)

- Renamed the unified CLI crate from `toolpath-cli` to `path-cli` so the package name matches the binary it installs (`path`). No code changes vs. `toolpath-cli` 0.5.0 — the source moved verbatim to `crates/path-cli/`.
- Extracted a `pub fn run() -> anyhow::Result<()>` library so the deprecated `toolpath-cli` shim can re-export it without duplicating source. The `path` and `gen_synthetic_path` binaries are now thin wrappers around the library.

### toolpath-cli 0.5.1 (deprecation shim)

- `toolpath-cli` is now a tiny shim crate whose only job is to make `cargo install toolpath-cli` keep working — it depends on `path-cli` and ships the same `path` binary. Existing users see no behavioral change on upgrade. The shim will be retired in a future release; pin to `path-cli` directly to avoid the eventual removal.
- The dev-only `gen_synthetic_path` helper is no longer shipped from this crate; it lives in `path-cli` only.

### toolpath-dot 0.1.3, toolpath-md 0.2.1, toolpath-git 0.1.4, toolpath-github 0.2.1 (publish re-alignment)

Patch bumps with no source changes. These four satellite crates were last released when `toolpath` was at 0.1.5, so their on-registry manifests still pin `toolpath = "0.1.5"`. Without these bumps, publishing any new crate (like `path-cli`) that depends on both `toolpath = "0.2.0"` and one of these four would drag two majors of `toolpath` into cargo's publish-time resolution and fail with E0308 type mismatches between `toolpath::types::Document` and `toolpath::v1::Document`. Each crate still uses `toolpath = { workspace = true }`, so the new published versions automatically pick up the workspace's current `toolpath = "0.2.0"` and the skew is closed.

## toolpath-claude 0.8.0 + toolpath-gemini 0.2.0 + toolpath-pi 0.2.0

### toolpath-claude 0.8.0

- Add `ConversationMetadata.first_user_message: Option<String>` — the first non-empty user-prompt text in the conversation. Populated cheaply during the metadata pass so picker UIs can surface what the conversation was *about* without a full read. Chain-aware: aggregated from the oldest segment.
- Fix `sanitize_project_path` to also map `.` → `-`, matching Claude Code's actual encoding. Without this, projects under dotted directories (`github.com/…`, `.claude/worktrees/…`) couldn't be looked up by their original path.

### toolpath-gemini 0.2.0

- Add `ConversationMetadata.first_user_message: Option<String>` — the first non-empty user-prompt text in the main chat. Populated for both main-session-file and orphan-UUID-directory cases.

### toolpath-pi 0.2.0

- Add `SessionMeta.first_user_message: Option<String>` — extracted during `list_sessions` by walking the JSONL until a user-role message with text content is found.

## 0.2.0 — toolpath + 0.4.0 — toolpath-cli

### toolpath 0.2.0

- Add JSONL streaming format for `Path` documents (new `v1::jsonl` module) per [docs/RFC-jsonl.md](docs/RFC-jsonl.md). Read with `Path::from_jsonl_reader` / `Path::from_jsonl_str`, write with `Path::to_jsonl_writer` / `Path::to_jsonl_string`. Line kinds: `PathOpen`, `Step`, `ActorDef`, `Signature`, `PathMeta`, `Head`, `PathClose`.
- Add `PathIdentity.graph_ref: Option<String>` — an optional `$ref`-style URL naming the graph a path belongs to. Additive and backwards-compatible; serialization omits the field when `None`, so existing documents and signatures remain byte-stable.

### toolpath-cli 0.4.0

- Accept `.path.jsonl` files wherever `--input` takes a canonical JSON path: `validate`, `render dot`, `render md`, `query *`, and `merge`. Extension-based routing via a new `io::read_document_auto` helper; stdin paths remain JSON-only in this release.
- Refactor `track` to persist sessions as `.path.jsonl` streams. The session file is still a single file per session, now in JSONL format, with tracking bookkeeping stored in `path.meta.extra["track"]` (stripped on export/close). Strict append-only writes are a future optimization.
- Rename example path documents to the two-part extension: `examples/path-*.json` → `examples/path-*.path.json`, with new `examples/path-*.path.jsonl` siblings.

## [Unreleased]

### Removed

- `toolpath-desktop` moved out of the workspace. The Tauri 2 desktop GUI now lives in the private [pathbase](https://github.com/empathic/pathbase) repo as `pathbase-app`. The toolpath derive/render crates remain open-source and are consumed by `pathbase-app` via git/crates.io deps.

### Changed

- `toolpath-cli` 0.5.0: CLI restructure — external-boundary verbs collapsed into two symmetric verbs with an on-disk document cache at `~/.toolpath/documents/<cache-id>.json`:
  - `path import <source>` replaces `path derive <source>`. Writes each derived document into the cache and prints the resulting path to stdout. `--no-cache` sends JSON to stdout instead (preserving old pipe ergonomics: `path import git --no-cache | path render md`). `--force` overwrites an existing cache entry; default is error-on-exists, uniform across every source.
  - `path export <target>` replaces `path incept` and `path project claude`. `export claude --input <ref> [--project <dir> | --output <file>]` covers both old commands; `<ref>` resolves as a bare cache id first (e.g. `claude-abc123`) or a filesystem path.
  - `path cache ls | rm` list / remove cached documents.
  - **New Pathbase round-trip:** `path import pathbase <id-or-url>` downloads a previously uploaded trace into the cache; `path export pathbase --input <ref>` uploads. Reuses the existing `path auth login` session at `~/.toolpath/credentials.json`. Targets `POST/GET /api/v1/traces[/:id]`. `--url` on `export pathbase` warns when its host differs from the session's.
  - **Cache id** is `<source>-<inner-id>`. Git folds a short hash of the canonical repo path so two repos on the same branch don't collide (`git-a1b2c3d4-path-main` vs `git-e5f6a7b8-path-main`). `make_id` strips any trailing `.json` to avoid round-tripping into a `.json.json` file.
  - **Atomic cache writes:** `write_cached` uses `O_CREAT | O_EXCL` when not forcing, so concurrent imports can't silently stomp each other.
  - **Deprecation aliases** (one-release overlap, hidden, stderr warning): `path derive` → `path import` (stdout preserved via implicit `--no-cache`), `path incept` → `path export claude --project <dir>`, `path project claude` → `path export claude`.
  - Shared HTTP/session plumbing extracted into `cmd_pathbase` (from `cmd_auth`); config-dir resolution lives in a new `config` module so `cmd_cache` builds on wasm/emscripten targets where `cmd_pathbase` is gated out.
- `toolpath-convo` 0.7.0: **breaking** — `file_write_diff` gains a `before_state: Option<&str>` parameter. For the `Write { content }` shape, callers can now supply the prior file contents (e.g. resolved from `git show HEAD:<path>`) so the resulting diff shows `-` lines for replaced content instead of an addition-only hunk. `None` preserves the old behaviour (diff against `""`). `Edit` / `MultiEdit` shapes are unaffected — they carry their own `old_string`. `toolpath-claude`'s Claude-JSONL deriver wires a best-effort git-HEAD lookup for `Write` tool invocations; falls back silently to additions-only when the project isn't a git repo, the file isn't tracked, or `git` isn't on `PATH`. (#35)
- `toolpath-convo` 0.6.0: adds `derive_path(view, config) -> Path` and `DeriveConfig` (moved in from the unreleased `toolpath-derive` crate). `toolpath-convo` now depends on `toolpath`.
- `toolpath-convo` 0.6.0: adds `ConversationProjector` trait, `AnyProjector` type-erasing wrapper, `extract_conversation()` for Path → ConversationView, and conversation sub-protocol (`conversation.init`, `conversation.append`, `tool.invoke`, `agent://` URN scheme).

### Added

- `toolpath-pi` 0.1.0: new crate — reads Pi (pi.dev) coding-agent session JSONL logs, implements `ConversationProvider`, and derives Toolpath `Path` documents via `toolpath-convo`'s shared derivation (`toolpath_convo::derive_path`). Reads from `~/.pi/agent/sessions/` by default; base directory is configurable. Preserves Pi's in-file conversation tree (id/parentId) as a DAG in the derived `Path`, and follows `parentSession` links across session files (bounded depth). CLI subcommands planned: `path derive pi` and `path list pi` (wiring may be merged separately).
- `toolpath-claude` 0.7.0: `ClaudeProjector` for projecting `ConversationView` back to Claude `Conversation`. Enriched derive: full text, tool invocation steps, `agent://` URNs, token usage, tool results via cross-entry assembly, `conversation.init` steps.
- `toolpath-gemini` 0.1.0: new crate — reads Gemini CLI conversation logs from `~/.gemini/tmp/<project>/chats/`, implements `ConversationProvider`, and derives Toolpath `Path` documents. `PathResolver` supports both friendly-name (`projects.json`) and SHA-256 hash-slot layouts. Sub-agent chat files (`kind: "subagent"`) are folded into `DelegatedWork` on the parent `task` tool invocation, with `turns` populated from the sub-agent's messages. Polling-based `ConversationWatcher` (feature `watcher`, default on) emits `Turn` / `TurnUpdated` / `Progress { kind: "subagent_started" | "subagent_complete" }` events. Guarantees round-trip fidelity at the `ChatFile` layer via `Option<Vec<T>>` for absent-vs-empty preservation, `GeminiRole::Other(String)` catch-all, `Option<Value>` on polymorphic `resultDisplay`, and `#[serde(flatten)] extra` at chat and message levels. 163 unit + 12 integration + 4 doc tests.
- `toolpath-codex` 0.1.0: new crate — reads Codex CLI rollout JSONL from `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`, implements `ConversationProvider`, and derives Toolpath `Path` documents. Maps the streaming `response_item` / `event_msg` model onto message-shaped `Turn`s: pairs `function_call` / `custom_tool_call` to outputs by `call_id`, buffers `reasoning` onto the next assistant turn, enriches tool invocations with `exec_command_end` stdout/exit code, and surfaces `patch_apply_end.changes` as sibling file artifacts carrying the real unified diff as the `raw` perspective. Non-turn rollout items (`session_meta`, `turn_context`, `task_started`, `task_complete`, `token_count`, etc.) preserved as `ConversationEvent`s for round-trip fidelity. Sessions are global (date-bucketed), not project-keyed; session id is either the UUIDv7 or the filename stem. 69 unit + 33 integration + 1 doc test.
- `toolpath-opencode` 0.1.0: new crate — reads opencode's `~/.local/share/opencode/opencode.db` SQLite database (opened read-only via `rusqlite` with `SQLITE_OPEN_READ_ONLY` so it never interferes with a live opencode process), implements `ConversationProvider`, and derives Toolpath `Path` documents. Strongly types all 12 `part.data` variants (text, reasoning, tool, step-start/-finish, snapshot, patch, file, agent, subtask, retry, compaction) with `#[serde(other)]` catch-alls so new upstream variants round-trip. Each message becomes a step with tool invocations attached; reasoning folds onto `Turn.thinking`. Real unified diffs come from opencode's sibling bare git snapshot repositories via `git2` tree↔tree comparisons, honoring both the current `snapshot/<project-id>/<sha1(worktree)>/` layout and the older `snapshot/<project-id>/` flat layout. Files under `.gitignore`d paths (which opencode never captures in its snapshot store) fall back to tool-input-derived structural changes with `source: "tool_input_gitignored"` labeling. Project id is the SHA of the repo's first root commit (stable across clones and renames). 43 unit + 1 doc test.
- `toolpath-cli` 0.4.0: adds `gen_synthetic_path` binary for generating synthetic `Path` fixtures at configurable step counts (bench support for toolpath-desktop Preview, see issue #41).
- `toolpath-cli` 0.3.1: `path project claude` and `path incept` commands for projecting toolpath documents into Claude sessions; `derive gemini`/`list gemini`, `derive codex`/`list codex`, and `derive opencode [--session ID] [--all] [--project ID] [--no-snapshot-diffs]` / `list opencode [--project ID] [--json]` subcommands.

## 0.3.0 — toolpath-cli

### toolpath-cli 0.3.0

- Add `render md` subcommand for Markdown output (`path render md --input doc.json [--detail full] [--front-matter]`)
- Add `derive github` and `list github` subcommands for GitHub PR provenance
- Update `toolpath-claude` dependency from 0.1.x to 0.6.x (session chaining, enriched conversation data)
- Add `toolpath-md` and `toolpath-github` as new dependencies
- Add insta snapshot integration tests for all 12 example documents

## 0.2.0 — toolpath-github + toolpath-md

### toolpath-github 0.2.0

- Capture `diff_hunk` on review comments as `raw` for code context display
- Capture PR summary stats in path meta: state, merged, additions, deletions, changed_files, draft, number, author
- Capture `author_association` (MEMBER, COLLABORATOR, etc.) stored in `extra["github"]["actor_associations"]`
- Capture `html_url` on CI check runs in structural extra
- Set review body as `meta.intent` on review decision steps for renderer visibility
- Thread review comment replies via `in_reply_to_id` — replies branch off the step they reply to instead of trunk-chaining

### toolpath-md 0.2.0

- Render review comment bodies inline in both summary and full modes (no more opaque `review://` URIs)
- Render CI conclusions with emoji indicators (pass/fail/skip) in summary mode
- Show diff_hunk code context alongside review comments in full mode
- Add PR-level diffstat to context block (`**Changes:** +N −M across K files`)
- Add Review summary section collecting all decisions and inline comments
- Friendly date range display in context block (e.g. `Feb 26–27, 2026`)
- PR identity line when GitHub metadata present (`**PR #6** by author · status · dates`)
- Hide opaque head ID when PR identity is shown
- Strip `review://` and `ci://checks/` prefixes for natural display names

## 0.1.0 — toolpath-md

### toolpath-md 0.1.0

- New crate: render Toolpath documents as Markdown for LLM consumption
- Handles all three document variants: Step, Path, and Graph
- Two detail levels: `Summary` (file-level diffstats) and `Full` (inline diffs as fenced code blocks)
- Optional YAML front matter with machine-readable metadata (step count, actors, artifacts, dead end count)
- Dead ends are marked inline and summarized in a dedicated section with intent and parent references
- Topological sort ensures steps appear in causal order regardless of input ordering
- Actor definitions rendered when present in path/graph metadata
- CLI: `path render md [--input FILE] [--detail summary|full] [--front-matter]`

## 0.1.0 — toolpath-github

### toolpath-github 0.1.0

- New crate: derive Toolpath Path documents from GitHub pull requests via the REST API
- Every PR event becomes a Step: commits (with per-file diffs), inline review comments, PR discussion comments, review decisions (approve/reject), and CI check runs
- Platform-agnostic artifact URIs: `review://` for code review artifacts, `ci://` for CI artifacts
- `derive_pull_request()` fetches all data and builds a complete provenance DAG
- `list_pull_requests()` lists PRs with summary metadata
- `resolve_token()` checks `GITHUB_TOKEN` env var, falls back to `gh auth token`
- Configurable: `--no-ci` and `--no-comments` flags to exclude non-code events
- CLI: `path derive github --repo owner/repo --pr 42` and `path list github --repo owner/repo`

## 0.5.0 — toolpath-convo / 0.6.2 — toolpath-claude

### toolpath-convo 0.5.0

- Added `WatcherEvent::as_turn()` — returns the `Turn` payload for both `Turn` and `TurnUpdated` variants
- Added `WatcherEvent::as_progress()` — returns `(kind, data)` for `Progress` events
- Added `WatcherEvent::is_update()` — returns `true` only for `TurnUpdated`
- Added `WatcherEvent::turn_id()` — returns the turn ID for turn-carrying variants
- Added dispatch loop example to `WatcherEvent` rustdoc

### toolpath-claude 0.6.2

- `to_turn()` now populates `Turn.extra["claude"]` with provider-specific metadata from `ConversationEntry.extra` (e.g. `subtype`, `data`), enabling trait-only consumers to access state-inference signals without importing provider types
- `WatcherEvent::Progress` events now include the full entry payload under `data["claude"]`, carrying fields like `data.type`, `data.hookName`, `data.agentId`, and `data.message` that were previously discarded
- Both changes are additive — previously-empty fields are now populated; no existing behavior changes
- Thanks to the crabcity maintainers for the detailed gap analysis

## 0.6.1 — toolpath-claude

### toolpath-claude 0.6.1

- Fix broken intra-doc links (`read_segment`, `list_segments`, `take_pending_rotations`, `poll`)
- Gate `successor_of` and `entry_to_watcher_event` behind `cfg(any(feature = "watcher", test))` to silence dead-code warnings when building without the `watcher` feature

## 0.6.0 — toolpath-claude

### toolpath-claude 0.6.0

- **Breaking:** `read_conversation` now follows session chains by default — any session ID returns the full merged conversation with bridge entries filtered out and `Conversation.session_ids` populated
- **Breaking:** `list_conversations` returns logical conversations (chain heads only) instead of all file stems
- **Breaking:** `list_conversation_metadata` / `read_conversation_metadata` aggregate across chain segments
- **Breaking:** `session_chain()` / `chain_head()` are now `pub(crate)` — no longer needed externally since `read_conversation` handles chain resolution
- **Breaking:** `ConversationMeta.predecessor` / `successor` are always `None` — chains are transparent, not navigable
- Added `read_segment()` and `list_segments()` for opt-in single-file access
- Added `Conversation.session_ids` field listing merged segment IDs
- Added `ChainIndex` — cached, incrementally-refreshed chain index replacing per-call `build_succession_map` scans
- `ConversationProvider` trait methods simplified to delegate to chain-aware `ClaudeConvo`
- `ConversationWatcher` now uses `read_segment` internally and `ChainIndex` for successor lookup
- Removed standalone `build_succession_map`, `resolve_chain`, `find_successor`, `successor_of`, `build_reverse_map` functions (superseded by `ChainIndex`)

## 0.4.0 — toolpath-convo / 0.5.0 — toolpath-claude

### toolpath-convo 0.4.0

- Added `SessionLinkKind` enum and `SessionLink` type for expressing predecessor/successor relationships between session segments
- Added `ConversationMeta.predecessor` and `ConversationMeta.successor` fields (`Option<SessionLink>`) for session chain navigation
- Added `ConversationView.session_ids` field (`Vec<String>`) listing all merged segment IDs in chronological order
- All new fields use `#[serde(default)]` — existing JSON without them deserializes cleanly

### toolpath-claude 0.5.0

- **Breaking (behavioral):** `load_conversation()` now transparently merges session chains — when Claude Code rotates to a new JSONL file, the segments are combined into a single `ConversationView` with bridge entries filtered out
- `load_metadata()` and `list_metadata()` now populate `predecessor`/`successor` links via the session chain
- `ConversationView.session_ids` populated with the full chain when multiple segments are merged
- `ConversationWatcher` automatically follows session rotations — polls seamlessly continue into successor files
- `ConversationWatcher` trait impl emits `WatcherEvent::Progress { kind: "session_rotated" }` when a rotation is detected
- Added `ClaudeConvo::session_chain()` and `ClaudeConvo::chain_head()` convenience methods
- New internal `chain` module with `build_succession_map`, `resolve_chain`, `find_successor`, `is_bridge_entry`
- Added `ConversationReader::read_first_session_id()` for O(1) bridge detection
- Thanks to the crab city team for reporting the 52 rotation events that motivated this work

## 0.3.0 — toolpath-convo / 0.4.0 — toolpath-claude

### toolpath-convo 0.3.0

- Added `EnvironmentSnapshot` type and `Turn.environment` field for per-turn working directory and VCS branch/revision
- Added `DelegatedWork` type and `Turn.delegations` field for sub-agent delegation tracking
- Added `ToolCategory` enum (`FileRead`, `FileWrite`, `FileSearch`, `Shell`, `Network`, `Delegation`) — toolpath's own classification ontology for tool invocations
- Added `ToolInvocation.category` field (`Option<ToolCategory>`) for semantic classification of tool calls
- Added `TokenUsage.cache_read_tokens` and `TokenUsage.cache_write_tokens` for prompt/context caching visibility
- Added `ConversationView.total_usage` for session-level aggregate token usage
- Added `ConversationView.provider_id` for identifying the conversation source (e.g. `"claude-code"`)
- Added `ConversationView.files_changed` for deduplicated, first-touch-ordered file mutation summary
- All new fields use `#[serde(default)]` — existing JSON without them deserializes cleanly

### toolpath-claude 0.4.0

- Populates `Turn.environment` from entry `cwd` and `gitBranch` fields
- Populates `ToolInvocation.category` by mapping known Claude Code tool names to `ToolCategory` variants
- Populates `Turn.delegations` from `Task` tool invocations, with cross-entry result assembly
- Populates `TokenUsage.cache_read_tokens` and `cache_write_tokens` from Claude usage data
- Computes `ConversationView.total_usage` by summing per-turn token usage
- Sets `ConversationView.provider_id` to `"claude-code"`
- Computes `ConversationView.files_changed` from `FileWrite`-categorized tool invocation inputs
- Thanks to the crabcity maintainers for the detailed enrichment proposal

## 0.2.0 — toolpath-convo / 0.3.0 — toolpath-claude

### toolpath-convo 0.2.0

- Added `WatcherEvent::TurnUpdated` variant for signaling when a previously-emitted turn has been updated with additional data (e.g. tool results that arrived in a later log entry)

### toolpath-claude 0.3.0

- **Breaking (behavioral):** `conversation_to_view()` and `ConversationProvider::load_conversation()` now perform cross-entry tool result assembly — tool-result-only user entries are absorbed into the preceding assistant turn's `ToolInvocation.result` fields instead of being emitted as separate phantom empty turns
- **Breaking (behavioral):** `ConversationWatcher` trait impl now emits `WatcherEvent::TurnUpdated` when tool results arrive, instead of emitting phantom empty user turns
- Added `Message::tool_results()` convenience method and `ToolResultRef` type, symmetric with `tool_uses()`/`ToolUseRef`
- Added shared `merge_tool_results()` that pairs results to invocations by `tool_use_id`
- Thanks to the crabcity maintainers for the detailed design request

## 0.2.1 — toolpath-claude

### toolpath-claude 0.2.1

- Fixed silent data loss when deserializing Claude Code JSONL conversation logs: `stop_reason`, `stop_sequence`, and all `Usage`/`CacheCreation` fields were always `None` because the structs expected camelCase keys but Claude Code writes the inner `message` object in the Anthropic API's native snake_case
- Added `#[serde(alias = "...")]` for snake_case variants on `Message`, `Usage`, and `CacheCreation` fields — both camelCase and snake_case are now accepted during deserialization
- Thanks to the crabcity maintainers for the detailed bug report

## 0.1.0 — toolpath-convo / 0.2.0 — toolpath-claude

### toolpath-convo 0.1.0

- New crate: provider-agnostic conversation types and traits for AI coding tools
- Types: `Turn`, `Role`, `ConversationView`, `ConversationMeta`, `TokenUsage`, `ToolInvocation`, `ToolResult`, `WatcherEvent`
- Traits: `ConversationProvider` (list/load conversations), `ConversationWatcher` (poll for updates)
- Enables consumer apps to code against a common conversation model instead of provider-specific types

### toolpath-claude 0.2.0

- Added convenience methods on `Message`: `text()`, `thinking()`, `tool_uses()`, `is_user()`, `is_assistant()`, `is_role()`
- Added convenience methods on `ConversationEntry`: `text()`, `role()`, `thinking()`, `tool_uses()`, `stop_reason()`, `model()`
- Added convenience methods on `Conversation`: `title(max_len)`, `first_user_text()`
- Implemented `toolpath_convo::ConversationProvider` for `ClaudeConvo`
- Implemented `toolpath_convo::ConversationWatcher` for sync `ConversationWatcher`
- Added `provider::to_view()` and `provider::to_turn()` for direct conversion
- New dependency: `toolpath-convo`

## 0.1.4 — toolpath / 0.2.0 — toolpath-cli

### toolpath 0.1.4

- Added `extra: HashMap<String, serde_json::Value>` with `#[serde(flatten)]` to `PathMeta`, `StepMeta`, and `GraphMeta`, matching the schema's `additionalProperties: true` and enabling round-trip fidelity for extension fields

### toolpath-cli 0.2.0

- **Breaking:** `path track` session files are now valid `{"Path": {...}}` Toolpath documents at all times. Tracking bookkeeping (buffer cache, sequence mappings) lives in `meta.track` and is stripped on export/close. Any Toolpath tool can read a live session file — `path validate`, `path query dead-ends`, `path render dot` all work mid-session without export.

## 0.1.3 — toolpath / 0.1.2 — all other crates

### All crates

- Improved README documentation: added motivating "why" context, cross-links between crates
- READMEs render as docs.rs landing pages with compilable examples

## 0.1.2 — toolpath / 0.1.1 — all other crates

### toolpath 0.1.2

- Added `repository`, `keywords`, and `categories` to crate metadata
- README now renders as the docs.rs landing page via `include_str!`
- All code examples in the README are compiled as doc tests

### toolpath-git 0.1.1

- Added `repository`, `keywords`, and `categories` to crate metadata
- Added `list_branches` to the API table in the README
- Added doc examples to `normalize_git_url` and `slugify_author`
- Module-level doc example for `derive`
- README now renders as the docs.rs landing page

### toolpath-claude 0.1.1

- Added `repository`, `keywords`, and `categories` to crate metadata
- README now renders as the docs.rs landing page

### toolpath-dot 0.1.1

- Added `repository`, `keywords`, and `categories` to crate metadata
- Added module-level documentation with usage example
- Added field docs to `RenderOptions`
- README now renders as the docs.rs landing page

### toolpath-cli 0.1.1

- Renamed crate from `path` to `toolpath-cli` (binary still called `path`)
- Moved crate directory from `crates/path/` to `crates/toolpath-cli/`
- Added `repository`, `keywords`, and `categories` to crate metadata
- Added `haiku` subcommand
- Installable via `cargo install toolpath-cli`

## 0.1.1 — toolpath

- Initial metadata-only release (added `repository`, `keywords`, `categories`)

## 0.1.0 — all crates

- Initial public release
- Core types: `Document`, `Graph`, `Path`, `Step` with builder API
- Query operations: `ancestors`, `dead_ends`, `filter_by_actor`, `filter_by_artifact`, `filter_by_time_range`
- Git derivation via `git2` (single branch -> Path, multiple branches -> Graph)
- Claude conversation derivation with filesystem watching
- Graphviz DOT rendering with actor color-coding and dead-end highlighting
- CLI with `derive`, `query`, `render`, `validate`, `list`, `merge`, `track` commands
- JSON Schema (`schema/toolpath.schema.json`)
- 11 example documents
- Full format specification (RFC.md)
