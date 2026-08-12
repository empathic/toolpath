# Toolpath

> *The story of how we got here.*

Toolpath is an **open session format**: one schema for what a coding agent
did, why, what it tried that didn't work, and what it cost — no matter
which agent did it.

Every agent writes its own undocumented session log — Claude Code's
rotating JSONL chains, Codex rollout files, Gemini chat directories,
Copilot event streams, opencode's SQLite, Cursor's composer store, Pi
session trees. Each one is a private artifact of the harness that
produced it. Toolpath reads them all into one format and writes them
back, and one format buys you:

- **Portability** — share a session and resume it on another machine, in
  another harness. Start in Claude Code, pick up in Codex.
- **Unified tooling** — one `jq` filter over every session on the
  machine, whichever agent produced it (`path query`).
- **Token accounting** — one vocabulary for usage across providers that
  all report it differently: true session totals, per-step attribution
  where the source supports it, reasoning breakdowns.
- **Provenance** — who changed what and why, with the dead ends
  preserved. The decisions that git collapses at merge time.
- **An archive you own** — `path p cache sync` incrementally ingests
  every session on the machine into plain JSON under `~/.toolpath/`.

## Supported sources

| Source | Read | Write | Resume | Notes |
|---|:-:|:-:|:-:|---|
| Claude Code | ✓ | ✓ | ✓ `claude -r` | Rotated session files merged into one chain; projected sessions load in Claude Code |
| Gemini CLI | ✓ | ✓ | ✓ `gemini --resume` | Sub-agent sessions folded in; reasoning tokens round-trip losslessly |
| Codex CLI | ✓ | ✓ | ✓ `codex resume` | Real unified diffs on every file change; per-step token attribution |
| Copilot CLI | ✓ | ✓ | ✓ `copilot --resume` | Preview; projected sessions verified to resume in copilot 1.0.67 |
| opencode | ✓ | ✓ | ✓ `opencode --session` | File diffs recovered from opencode's git snapshots |
| Cursor (IDE) | ✓ | ✓ | ✓ opens workspace | Projected composers render in Cursor.app's chat sidebar |
| Pi | ✓ | ✓ | ✓ `pi --session` | Branching session trees preserved as DAGs |
| git | ✓ | — | — | Commit history → steps |
| GitHub PRs | ✓ | — | — | Review threads, CI runs, and comments attached |

*Read* is `path p import` / `path p list` / `path show`; *write* is
`path p export`; *resume* is `path resume` — project the session into
the harness's on-disk layout and exec its resume command. Any readable
session can be projected into any writable harness; the pairs are
exercised by a cross-harness conformance matrix in CI.

## Install

```bash
# Prebuilt binary (macOS arm64, Linux x86_64/arm64)
curl --proto '=https' --tlsv1.2 -fsS https://toolpath.net/install.sh | bash

# From source
cargo install path-cli
```

Both install a binary called `path`.

Claude Code users can skip the manual install entirely — the Toolpath plugin
bundles the CLI (downloaded and installed globally on first use) and adds
`/path:share` and `/path:query` slash commands:

```
/plugin marketplace add empathic/toolpath
/plugin install path@toolpath
```

See [plugins/claude-code](plugins/claude-code/) for details.

> The older `toolpath-cli` crate name still works — `cargo install toolpath-cli` is now a thin shim that pulls in `path-cli` and installs the same binary. New users should reach for `path-cli` directly; the shim will eventually be retired.

## Sixty-second tour

```bash
# Archive every agent session on this machine (all harnesses, incremental)
path p cache sync

# Query across all of them with a jq filter — dead ends, expensive turns,
# whatever — regardless of which agent produced them
path query 'map(select(.dead_end)) | length'
path query --source claude 'map(select(any(.change[].structural.token_usage; .input_tokens > 50000)))'

# Share a session: interactive picker across every installed harness,
# sessions from the current project ranked first
path share

# Resume a shared session — in the harness that produced it, or a different one
path resume https://pathbase.dev/alex/pathstash/path-pr-42
path resume claude-<session-id> --harness codex -C /path/to/project
```

Below the porcelain, every step is composable plumbing:

```bash
# Import a specific session into the local cache (~/.toolpath/documents/),
# or straight to stdout for shell composition
path p import claude --project /path/to/project --session <id>
path p import codex --no-cache | path p render md --detail full

# Derive from git history or a GitHub PR
path p import git --repo . --branch main
path p import github https://github.com/owner/repo/pull/42

# Visualize the step DAG
path p import git --repo . --branch main --no-cache | path p render dot | dot -Tpng -o graph.png

# Project a cached document into a harness's on-disk format
path p export codex --input claude-<session-id> --project /path/to/sandbox

# Merge, validate, walk ancestry
path p merge doc1.json doc2.json --title "Release v2"
path p validate --input examples/step-01-minimal.json
path p query ancestors --input doc.json --step-id step-003
```

## The format

Three objects, from a single change up to a release:

| Object    | What it represents                        | Example                |
|-----------|-------------------------------------------|------------------------|
| **Step**  | A single change to artifact(s)            | One commit, one edit   |
| **Path**  | A sequence of steps with a base context   | A PR, a coding session |
| **Graph** | A collection of related paths             | A release              |

Steps record an actor (`human:alex`, `agent:claude-code`, `tool:rustfmt`),
changes (unified diffs and/or structural operations), intent, and
verification. They form a DAG via parent references, so abandoned
approaches stay in the record — dead ends are simply steps not in the
ancestry of `path.head`:

```
              +-- step-3a -- step-4a  (dead end)
step-1 -- step-2 --+
              +-- step-3b -- step-4b -- step-5b  (head)
```

A path can declare a **kind** — a versioned, immutable URI naming what
its steps mean. Agent sessions use
`https://toolpath.net/kinds/agent-coding-session/v1.1.0`; `path kind
agent-coding-session` prints the field reference. The full spec is
[RFC.md](RFC.md), the machine-checkable contract is
[schema/toolpath.schema.json](schema/toolpath.schema.json).

## Token accounting

Providers report usage in incompatible ways: Claude streams cumulative
per-block snapshots, Codex re-emits stale cumulative counters, Gemini
reports reasoning as an additive sibling that's easy to drop, Copilot
reports only output per message. The `agent-coding-session` kind
normalizes all of it into three fields with strict semantics:

- **`token_usage`** — the total for a message, placed once per message
  group, so summing it over a path always yields the true session total.
- **`attributed_token_usage`** — a step's own spend, populated only
  where the source genuinely reports per-step numbers (Codex), never
  guessed from streaming snapshots.
- **`breakdowns`** — informational decompositions like
  `output → reasoning` (Gemini, Codex, opencode), never double-counted
  into any total.

Cumulative counters, repeated message totals, and zero-filled
placeholders are decoded away at derivation time, per provider — so
`path query` can aggregate spend across harnesses without knowing any
provider's quirks.

## Using the libraries

Everything the CLI does is a library call. Each source has its own crate
(see [Workspace](#workspace)); `toolpath` holds the core types and
`toolpath-convo` the provider-agnostic conversation model.

### Read a session, get a document

```rust
use toolpath_claude::{ClaudeConvo, derive::{derive_path, DeriveConfig}};

let convo = ClaudeConvo::new();
let conversation = convo.read_conversation("/path/to/project", "session-id")?;
let path = derive_path(&conversation, &DeriveConfig {
    include_thinking: true,
    ..Default::default()
});
let json = toolpath::Graph::from_path(path).to_json_pretty()?;
```

Every provider crate follows the same shape: a reader for the harness's
on-disk format, a `ConversationProvider` impl producing a
provider-agnostic `ConversationView`, and the shared
`toolpath_convo::derive_path` mapping views to `Path` documents.
Supporting a new harness means implementing the view — not the mapping.
`toolpath-claude` and `toolpath-gemini` also ship filesystem watchers
(default-on `watcher` feature) for tailing live sessions.

### Core types

```rust
use toolpath::{Step, Path, Base, Graph};

let step = Step::new("step-001", "human:alex", "2026-01-29T10:00:00Z")
    .with_parent("step-000")
    .with_raw_change("src/main.rs", "@@ -1,1 +1,1 @@\n-hello\n+world")
    .with_intent("Fix greeting");

let path = Path::new(
    "path-pr-42",
    Some(Base::vcs("github:org/repo", "abc123")),
    "step-001",
);

// Graph is the single root type of every Toolpath document. Wrap a single
// Path as a one-path Graph for serialization:
let graph = Graph::from_path(path);
let json = graph.to_json_pretty()?;
```

### Query operations

```rust
use toolpath::query;

let ancestors = query::ancestors(&path.steps, &path.path.head);
let dead_ends = query::dead_ends(&path.steps, &path.path.head);
let by_actor = query::filter_by_actor(&path.steps, "agent:");
let artifacts = query::all_artifacts(&path.steps);
```

### Rendering

```rust
let dot = toolpath_dot::render(&doc, &toolpath_dot::RenderOptions::default());
let md = toolpath_md::render(&doc, &toolpath_md::RenderOptions::default());
```

## Not a Rust shop?

The format stands on its own: [RFC.md](RFC.md) is the spec,
[schema/toolpath.schema.json](schema/toolpath.schema.json) validates
documents in any language, and [examples/](examples/) holds 12 documents
covering steps, paths, and graphs.

And the source formats we reverse-engineered to build the derive crates
are documented at [docs/agents/formats/](docs/agents/formats/README.md)
— including a twelve-part reference for Claude Code's JSONL (envelope,
entry types, session chains, compaction, and a writing-compatible guide)
and the Copilot CLI loader's writer contract, mapped rejection message
by rejection message. If you're building your own session tooling,
they're useful even if you never run our code.

## CLI reference

```
path
  haiku
  show          # markdown summary for a single session (used as fzf preview)
    claude    --project PATH --session ID
    gemini    --project PATH --session UUID
    codex     --session ID
    copilot   --session ID
    opencode  --session ID
    cursor    --session UUID
    pi        --project PATH --session ID [--base DIR]
  share       # one-shot interactive picker + Pathbase upload
  resume      # project a doc into a coding agent and exec --resume
  query       # jaq (jq) filter over cached steps
              FILTER [--source NAME] [--id CACHE-ID] [--input FILE]
              [--project PATH] [--kind SELECTOR] [-c] [-r]
  kind        # list bundled kinds, or print a kind's schema
              [KIND[/VERSION]]
  auth        login | status | whoami | logout [--url URL]
  p           # plumbing: lower-level building blocks
    query
      ancestors --input FILE --step-id ID
    list
      git       [--repo PATH] [--remote NAME] [--format pretty|json|tsv]
      github    --repo OWNER/REPO [--format ...]
      claude    [--project PATH] [--format ...]
      gemini    [--project PATH] [--format ...]
      codex     [--format ...]
      copilot   [--format ...]
      opencode  [--project ID] [--format ...]
      cursor    [--project PATH] [--format ...]
      pi        [--project PATH] [--base DIR] [--format ...]
    import                                            # writes to ~/.toolpath/documents/ by default
      git       --repo PATH --branch NAME[:START] [--base COMMIT] [--remote NAME] [--title TEXT]
      github    --repo OWNER/REPO --pr NUMBER [--no-ci] [--no-comments]
      claude    [--project PATH] [--session ID] [--all]
      gemini    [--project PATH] [--session UUID] [--all]
      codex     [--session UUID|STEM] [--all]
      copilot   [--session ID] [--all]
      opencode  [--session ID] [--all] [--project ID] [--no-snapshot-diffs]
      cursor    [--session UUID] [--all] [--project PATH]
      pi        [--project PATH] [--session ID] [--all] [--base DIR]
      pathbase  TRACE-ID-OR-URL [--url URL]
                                                      # global: [--force] [--no-cache]
    export
      claude    --input REF [--project DIR | --output FILE]
      gemini    --input REF [--project DIR | --output FILE]
      codex     --input REF [--project DIR | --output FILE]
      opencode  --input REF [--project DIR | --output FILE]
      copilot   --input REF [--project DIR | --output FILE]
      cursor    --input REF [--project DIR | --output FILE]
      pi        --input REF [--project DIR | --output FILE]
      pathbase  --input REF [--url URL]
    cache
      ls | rm CACHE-ID | sync [TYPE...]
    render
      dot       [--input FILE] [--output FILE] [--show-files] [--show-timestamps]
      md        [--input FILE] [--output FILE] [--detail summary|full] [--front-matter]
    merge       FILE... [--title TEXT]
    validate    --input FILE
    derive      # stdout-JSON sibling of import (same sources, --no-cache implied)
    project     # narrower file-shaped sibling of export
    incept      # file/stdin-shaped sibling of `export <provider> --project` (claude, cursor)
    track
      init      --file PATH --actor ACTOR [--title TEXT] [--base-uri URI] [--base-ref REF]
      step      --session FILE --seq N [--actor ACTOR] [--intent TEXT]
      visit     --session FILE --seq N
      note      --session FILE --intent TEXT
      export    --session FILE
      close     --session FILE
      list
```

Global: `--pretty` for formatted JSON output.

**Breaking** (pre-1.0). The previous top-level commands `path import`,
`path export`, `path cache`, `path list`, `path render`, `path merge`,
`path validate`, `path derive`, `path project`, `path incept`, and
`path track` were **removed** in `path-cli` 0.10.0 — they all now live
exclusively under `path p`.

## Interactive selection

When `path p import <provider>` is run with no `--session` and stdin/stderr
are TTYs, the CLI launches a fuzzy picker so you can pick a session by
topic. TAB selects multiple — the result is a `Graph`. `path share` and
`path resume` use the same picker.

Two backends, selected at runtime:

- **External `fzf`** is preferred when it's on `$PATH` (so your fzf
  config and keybindings keep working).
- **Embedded `skim`** (Rust fzf-clone) is shipped in the default build
  and used when `fzf` isn't installed. Same `{1}`/`{2}` preview
  placeholders, same column-selection grammar — visually similar UX.
  Build with `--no-default-features` to drop it for a ~2 MB smaller
  binary; without either backend the CLI prints a manual recipe.

Use the global `--picker auto|fzf|skim` flag to force a backend
(default `auto`):

```bash
path --picker skim share              # use embedded skim even with fzf on PATH
path --picker fzf p import claude     # error out if fzf isn't installed
```

The picker leans on two machine-readable surfaces you can also use yourself:

- `path p list <provider> --format tsv` — one session per line, tab-delimited.
  For project-keyed providers (claude, gemini, pi) the columns are
  `<project>\t<session>\t<iso8601 last_activity>\t<count>\t<first_user_message>`.
  For single-keyed providers (codex, opencode):
  `<session>\t<iso8601 last_activity>\t<count>\t<cwd>\t<first_user_message>`.
  `--format` defaults to `pretty` on a TTY and `tsv` when piped.
- `path show <provider> --…` — markdown summary for one session (the
  picker's `--preview` command).

Manual recipe (project-keyed; substitute `claude` for `gemini` or `pi`):

```bash
path p list claude --format tsv \
  | fzf --delimiter=$'\t' --with-nth=5,3 \
        --preview 'path show claude --project {1} --session {2}' \
  | awk -F'\t' '{print $1; print $2}' \
  | xargs -L2 sh -c 'path p import claude --project "$1" --session "$2"' --
```

Single-keyed (codex/opencode):

```bash
path p list codex --format tsv \
  | fzf --delimiter=$'\t' --with-nth=5,2 \
        --preview 'path show codex --session {1}' \
  | cut -f1 \
  | xargs -I{} path p import codex --session {}
```

## Workspace

```
crates/
  toolpath/           Core types, builders, query API
  toolpath-convo/     Provider-agnostic conversation types, traits, and Toolpath-Path derivation
  toolpath-git/       Derive from git repository history
  toolpath-github/    Derive from GitHub pull requests
  toolpath-claude/    Derive from Claude conversation logs
  toolpath-gemini/    Derive from Gemini CLI conversation logs
  toolpath-codex/     Derive from Codex CLI rollout files
  toolpath-copilot/   Derive from GitHub Copilot CLI session logs (preview)
  toolpath-opencode/  Derive from opencode SQLite databases
  toolpath-cursor/    Derive from Cursor (IDE) state.vscdb bubble store
  toolpath-pi/        Derive from Pi (pi.dev) agent sessions
  toolpath-dot/       Graphviz DOT visualization
  toolpath-md/        Markdown rendering for LLM consumption
  pathbase-client/    Progenitor-derived typed client for the Pathbase HTTP API
  path-cli/           Unified CLI (binary: path)
  toolpath-cli/       Deprecated shim that re-exports path-cli
```

See each crate's README for library-level documentation.

## Documentation

- [RFC.md](RFC.md) -- Full format specification
- [FAQ.md](FAQ.md) -- Design rationale and FAQ
- [CHANGELOG.md](CHANGELOG.md) -- Release history
- [schema/toolpath.schema.json](schema/toolpath.schema.json) -- JSON Schema
- [examples/](examples/) -- 12 example documents covering steps, paths, and graphs
- [docs/agents/formats/](docs/agents/formats/README.md) -- Reference for the on-disk
  session formats of every agent we derive from

## Requirements

Rust 1.85+ (edition 2024).

## License

Apache-2.0
