# Toolpath

> *The story of how we got here.*

Toolpath is an open format for coding-agent sessions: one schema that
records what an agent did, why, what it tried that didn't work, and
what it cost, independent of which agent did it.

Every agent writes its own undocumented session log. Claude Code keeps
rotating JSONL chains, Codex writes rollout files, Gemini has chat
directories, Copilot an event stream, opencode a SQLite database,
Cursor a composer store, Pi session trees. Toolpath reads them all into
one format and writes them back. With everything in one format you can:

- **Portability.** Share a session and resume it on another machine or
  in another harness. A session started in Claude Code can be picked up
  in Codex.
- **An archive you control.** `path p cache sync` incrementally ingests
  every session on the machine into plain JSON under `~/.toolpath/`.
- **Unified tooling.** A tool written against one schema works for
  every harness at once, instead of needing a parser per agent.
  `path query` is the built-in example: it searches and aggregates
  across every session on the machine, whoever wrote them, using jq
  filters.
- **Token accounting.** Real session totals, per-step attribution where
  the source reports it, and reasoning breakdowns, even though every
  provider reports usage differently.
- **Provenance.** Who changed what, why, and what was tried and
  abandoned. The record that git collapses at merge time.

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
| git | ✓ | | | Commits become steps |
| GitHub PRs | ✓ | | | Review threads, CI runs, and comments attached |

Read means `path p import`, `path p list`, and `path show`. Write means
`path p export`. Resume means `path resume`, which projects the session
into the harness's on-disk layout and execs its resume command. Any
readable session can be projected into any writable harness; a
cross-harness conformance matrix in CI exercises the pairs.

## Install

```bash
# Prebuilt binary (macOS arm64, Linux x86_64/arm64)
curl --proto '=https' --tlsv1.2 -fsS https://toolpath.net/install.sh | bash

# From source
cargo install path-cli
```

Both install a binary called `path`.

Claude Code users can skip the manual install entirely: the Toolpath plugin
bundles the CLI (downloaded and installed globally on first use) and adds
`/path:share` and `/path:query` slash commands:

```
/plugin marketplace add empathic/toolpath
/plugin install path@toolpath
```

See [plugins/claude-code](plugins/claude-code/) for details.

## Quick start

```bash
# Archive every agent session on this machine (all harnesses, incremental)
path p cache sync

# Query across all of them with a jq filter, regardless of which agent
# produced them: dead ends, expensive turns, and so on
path query 'map(select(.dead_end)) | length'
path query --source claude 'map(select(any(.change[].structural.token_usage; .input_tokens > 50000)))'

# Share a session: interactive picker across every installed harness,
# sessions from the current project ranked first
path share

# Resume a shared session, in the original harness or a different one
path resume https://pathbase.dev/alex/pathstash/path-pr-42
path resume claude-<session-id> --harness codex -C /path/to/project
```

Each of those is built from lower-level plumbing commands you can
compose yourself:

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
approaches stay in the record: a dead end is any step outside the
ancestry of `path.head`.

```
              +-- step-3a -- step-4a  (dead end)
step-1 -- step-2 --+
              +-- step-3b -- step-4b -- step-5b  (head)
```

A path can declare a **kind**, a versioned, immutable URI naming what
its steps mean. Agent sessions use
`https://toolpath.net/kinds/agent-coding-session/v1.1.0`; `path kind
agent-coding-session` prints the field reference. [RFC.md](RFC.md) is
the full specification and
[schema/toolpath.schema.json](schema/toolpath.schema.json) is the JSON
Schema.

## Token accounting

Providers report usage in incompatible ways: Claude streams cumulative
per-block snapshots, Codex re-emits stale cumulative counters, Gemini
reports reasoning as an additive sibling that's easy to drop, Copilot
reports only output per message. The `agent-coding-session` kind
normalizes all of it into three fields with strict semantics:

- `token_usage` is the total for a message. It appears once per message
  group, so summing it over a path gives the true session total.
- `attributed_token_usage` is a step's own spend. It is populated only
  when the source genuinely reports per-step numbers (Codex does), and
  never guessed from streaming snapshots.
- `breakdowns` holds informational decompositions, such as reasoning
  tokens within output (Gemini, Codex, opencode). These are never added
  into any total.

Each provider's quirks (cumulative counters, repeated message totals,
zero-filled placeholders) are handled at derivation time, so
`path query` can aggregate spend across harnesses without knowing about
any of them.

## Using the libraries

Everything the CLI does is a library call. Each source has its own crate
(see [Workspace](#workspace)); `toolpath` holds the core types and
`toolpath-convo` the provider-agnostic conversation model.

### Deriving a document from a session

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
Supporting a new harness means implementing the view, not the mapping.
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

## Using the format without the crates

The format does not require Rust. [RFC.md](RFC.md) is the
specification,
[schema/toolpath.schema.json](schema/toolpath.schema.json) validates
documents in any language, and [examples/](examples/) holds 12 documents
covering steps, paths, and graphs.

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

## Interactive selection

When `path p import <provider>` is run with no `--session` and stdin/stderr
are TTYs, the CLI launches a fuzzy picker so you can pick a session by
topic. TAB selects multiple sessions, which produces a `Graph`.
`path share` and `path resume` use the same picker.

Two backends, selected at runtime:

- **External `fzf`** is preferred when it's on `$PATH` (so your fzf
  config and keybindings keep working).
- **Embedded `skim`** (Rust fzf-clone) is shipped in the default build
  and used when `fzf` isn't installed. It honors the same `{1}`/`{2}`
  preview placeholders and column-selection grammar, so the UX is
  close. Build with `--no-default-features` to drop it for a ~2 MB
  smaller binary; without either backend the CLI prints a manual
  recipe.

Use the global `--picker auto|fzf|skim` flag to force a backend
(default `auto`):

```bash
path --picker skim share              # use embedded skim even with fzf on PATH
path --picker fzf p import claude     # error out if fzf isn't installed
```

The picker leans on two machine-readable surfaces you can also use yourself:

- `path p list <provider> --format tsv`: one session per line, tab-delimited.
  For project-keyed providers (claude, gemini, pi) the columns are
  `<project>\t<session>\t<iso8601 last_activity>\t<count>\t<first_user_message>`.
  For single-keyed providers (codex, opencode):
  `<session>\t<iso8601 last_activity>\t<count>\t<cwd>\t<first_user_message>`.
  `--format` defaults to `pretty` on a TTY and `tsv` when piped.
- `path show <provider> --…`: markdown summary for one session (the
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
```

See each crate's README for library-level documentation.

## Documentation

- [RFC.md](RFC.md) -- Full format specification
- [FAQ.md](FAQ.md) -- Design rationale and FAQ
- [CHANGELOG.md](CHANGELOG.md) -- Release history
- [schema/toolpath.schema.json](schema/toolpath.schema.json) -- JSON Schema
- [examples/](examples/) -- 12 example documents covering steps, paths, and graphs
- [DEVELOPMENT.md](DEVELOPMENT.md) -- Building and testing, plus our working notes
  on the on-disk session formats of the agents we derive from

## Requirements

Rust 1.85+ (edition 2024).

## License

Apache-2.0
