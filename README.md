# Toolpath

A tool-agnostic format for tracking artifact transformation provenance.

## What is this?

Toolpath records the complete history of how code (and other artifacts) evolved:

- **Who** made changes (humans, AI agents, formatters, linters, CI)
- **What** they changed (unified diffs + structural AST operations)
- **Why** they changed it (intent, linked issues, reasoning)
- **What else they tried** (dead ends preserved for reflection)
- **Verification** (cryptographic signatures, identity resolution)

## Three core objects

| Object    | What it represents                        | Example                |
|-----------|-------------------------------------------|------------------------|
| **Step**  | A single change to artifact(s)            | One commit, one edit   |
| **Path**  | A sequence of steps with a base context   | A PR, a coding session |
| **Graph** | A collection of related paths             | A release              |

Steps form a DAG via parent references. Dead ends are implicit: steps not in the ancestry of `path.head`.

```
              +-- step-3a -- step-4a  (dead end)
step-1 -- step-2 --+
              +-- step-3b -- step-4b -- step-5b  (head)
```

## Install

```bash
# Prebuilt binary (macOS arm64, Linux x86_64/arm64)
curl --proto '=https' --tlsv1.2 -fsS https://toolpath.net/install.sh | bash

# from source
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

## Quick start

```bash
# Build everything
cargo build --workspace

# Import a Toolpath document from this repo's git history (cached under ~/.toolpath/documents/)
path p import git --repo . --branch main

# Visualize it
path p import git --repo . --branch main --no-cache | path p render dot | dot -Tpng -o graph.png

# Render as Markdown for an LLM
path p import git --repo . --branch main --no-cache | path p render md

# Import from a GitHub pull request
path p import github https://github.com/owner/repo/pull/42

# Import from Claude conversation logs
path p import claude --project /path/to/project

# Import from Gemini CLI conversation logs
path p import gemini --project /path/to/project

# Import from Codex CLI rollout files (most recent session by default)
path p import codex

# Import from opencode session database (most recent session by default)
path p import opencode

# List what's in the cache
path p cache ls

# Ingest new/changed agent sessions into the cache (all harnesses, or named ones)
path p cache sync
path p cache sync claude codex

# Export a cached document back into a Claude Code session
path p export claude --input claude-<session-id> --project /path/to/resume

# Push a cached document to Pathbase
path auth login
path p export pathbase --input claude-<session-id>

# Pull a path from Pathbase back into the local cache
# (full URL or bare `<owner>/<repo>/<slug>` triple)
path p import pathbase https://pathbase.dev/alex/pathstash/path-pr-42

# Send shares somewhere else instead. Designate it once, and bare
# `path share` goes there from then on.
path auth s3 login                      # credentials, if you're using a bucket
path target ~/Dropbox/toolpath-traces   # a folder — no credentials needed
path target s3://my-bucket/traces       # or a bucket; checked before it's stored
path share                              # → wherever you pointed it
path target                             # what's in effect, and why

# Override for one call, without changing the default
path share --to /tmp/scratch
path share --to pathbase

# Resume a Toolpath document into your coding agent of choice (interactive
# harness picker; project the session and exec the harness's resume command)
path resume https://pathbase.dev/alex/pathstash/path-pr-42
path resume ~/Dropbox/toolpath-traces   # lists what you've shared, pick one
path resume s3://my-bucket/traces/2026-08-07-fix-the-parser-claude-abc.json
path resume claude-<session-id> --harness claude -C /path/to/project

# Query the whole local cache with a jaq (jq) filter over wrapped steps
# (e.g. dead ends, or turns by an agent actor)
path query 'map(select(.dead_end))'
path query --input doc.json 'map(select(.step.actor | startswith("agent:")))'

# List bundled document kinds, or print a kind's schema (the field reference)
path kind
path kind agent-coding-session

# Walk the ancestry of a step (plumbing)
path p query ancestors --input doc.json --step-id step-003

# Merge multiple documents into a graph
path p merge doc1.json doc2.json --title "Release v2" --pretty

# Validate a document
path p validate --input examples/step-01-minimal.json
```

## CLI reference

```
path
  haiku
  show          # markdown summary for a single session (used as fzf preview)
    claude    --project PATH --session ID
    gemini    --project PATH --session UUID
    codex     --session ID
    opencode  --session ID
    pi        --project PATH --session ID [--base DIR]
  share       # one-shot interactive picker + upload to the share target
              [--to pathbase|s3://BUCKET/PREFIX|FOLDER]
              [--harness NAME] [--session ID] [--project PATH] [--no-cache]
              [--url URL] [--anon] [--repo OWNER/NAME] [--name TEXT] [--public]
  resume      # project a doc into a coding agent and exec --resume
              # INPUT: pathbase URL | s3://…/doc.json | a destination to
              # browse | owner/repo/slug | file | cache id
  query       # jaq (jq) filter over cached steps
              FILTER [--source NAME] [--id CACHE-ID] [--input FILE]
              [--project PATH] [--kind SELECTOR] [-c] [-r]
  kind        # list bundled kinds, or print a kind's schema
              [KIND[/VERSION]]
  auth        login | status | whoami | logout [--url URL]
              s3 login [--region R] [--endpoint URL] [--access-key-id ID]
                       [--secret-access-key KEY] [--session-token TOK]
                       [--virtual-hosted-style]
              s3 status | s3 logout
  target      # where `path share` uploads; no argument prints it.
              # Setting one writes a probe object to prove it works.
              [pathbase | s3://BUCKET/PREFIX | FOLDER] [--clear] [--no-verify]
  p           # plumbing: lower-level building blocks
    query
      ancestors --input FILE --step-id ID
    list
      git       [--repo PATH] [--remote NAME] [--format pretty|json|tsv]
      github    --repo OWNER/REPO [--format ...]
      claude    [--project PATH] [--format ...]
      gemini    [--project PATH] [--format ...]
      codex     [--format ...]
      opencode  [--project ID] [--format ...]
      pi        [--project PATH] [--base DIR] [--format ...]
    import                                            # writes to ~/.toolpath/documents/ by default
      git       --repo PATH --branch NAME[:START] [--base COMMIT] [--remote NAME] [--title TEXT]
      github    --repo OWNER/REPO --pr NUMBER [--no-ci] [--no-comments]
      claude    [--project PATH] [--session ID] [--all]
      gemini    [--project PATH] [--session UUID] [--all]
      codex     [--session UUID|STEM] [--all]
      opencode  [--session ID] [--all] [--project ID] [--no-snapshot-diffs]
      pi        [--project PATH] [--session ID] [--all] [--base DIR]
      pathbase  TRACE-ID-OR-URL [--url URL]
      object    URL                                   # s3:// or file://; alias: s3
                                                      # global: [--force] [--no-cache]
    export
      claude    --input REF [--project DIR | --output FILE]
      pathbase  --input REF [--url URL]
      object    --input REF [--to DEST]               # alias: s3
    cache
      ls | rm CACHE-ID
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

## Using the libraries

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

### Git derivation

```rust
use toolpath_git::{derive, DeriveConfig};

let repo = git2::Repository::open(".")?;
let config = DeriveConfig { remote: "origin".into(), title: None, base: None };
let doc = derive(&repo, &["main".into()], &config)?;
```

### DOT rendering

```rust
use toolpath_dot::{render, RenderOptions};

let dot_string = render(&doc, &RenderOptions::default());
```

### Markdown rendering

```rust
use toolpath_md::{render, RenderOptions};

let md_string = render(&doc, &RenderOptions::default());
```

## Documentation

- [RFC.md](RFC.md) -- Full format specification
- [FAQ.md](FAQ.md) -- Design rationale and FAQ
- [CHANGELOG.md](CHANGELOG.md) -- Release history
- [schema/toolpath.schema.json](schema/toolpath.schema.json) -- JSON Schema
- [examples/](examples/) -- 11 example documents covering steps, paths, and graphs
- [docs/agents/formats/](docs/agents/formats/README.md) -- Reference for the on-disk
  formats emitted by agents we derive from (Claude Code today; more as they land)

## Requirements

Rust 1.85+ (edition 2024).

## License

Apache-2.0
