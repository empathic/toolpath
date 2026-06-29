# path-cli

One binary that ties together everything Toolpath can do: derive provenance from git and Claude, query and filter documents, render DAG visualizations, track live editing sessions, and merge results into release graphs. If you want to use Toolpath without writing Rust, start here.

Unified CLI for deriving, querying, and visualizing Toolpath provenance documents.

## Installation

```bash
cargo install path-cli
```

This installs a binary called `path`.

> Previously published as `toolpath-cli`. The old crate name still works — `cargo install toolpath-cli` is now a thin shim that depends on this crate and installs the same `path` binary — but new installs should pin to `path-cli` directly.

Or run from source:

```bash
cargo run -p path-cli -- <command>
```

The CLI splits into porcelain (top-level: `show`, `share`, `resume`,
`query`, `auth`, `haiku`) and plumbing (`path p …`: `p list`,
`p import`, `p export`, `p cache`, `p render`, `p merge`, `p validate`,
`p derive`, `p project`, `p incept`, `p track`).

## Typical workflows

**Capture the provenance of a PR:**

```bash
path p import git --repo . --branch feature --no-cache --pretty > pr-provenance.json
```

**Visualize how a branch evolved, including dead ends:**

```bash
path p import git --repo . --branch main:HEAD~20 --no-cache | path p render dot | dot -Tpng -o history.png
```

**Review what an AI agent changed:**

```bash
path p import claude --project . --no-cache | path query --input - 'map(select(.step.actor | startswith("agent:")))'
```

**Record provenance for a live editing session:**

```bash
cat src/main.rs | path p track init --file src/main.rs --actor "human:alex"
# ... edit the file ...
cat src/main.rs | path p track step --session /tmp/session.json --seq 1 --parent-seq 0
path p track annotate --session /tmp/session.json --intent "Refactored auth"
path p track close --session /tmp/session.json --pretty > session-provenance.json
```

**Merge multiple sources into a release graph:**

```bash
path p merge git-provenance.json claude-provenance.json --title "v2.0 Release" --pretty
```

## Commands

### p list

Discover available sources before importing.

```bash
# List git branches with metadata
path p list git --repo .

# List Claude projects
path p list claude

# List sessions within a project
path p list claude --project /path/to/project

# Machine-readable output
path p list git --repo . --json
```

### p import

Generate Toolpath documents from source systems and write them to the
local cache (`~/.toolpath/documents/`). Use `--no-cache` to stream the
JSON to stdout for shell composition instead.

```bash
# From git history (single branch -> Path, multiple -> Graph)
path p import git --repo . --branch main --pretty
path p import git --repo . --branch main --branch feature --title "Release v2"
path p import git --repo . --branch main:HEAD~20 --pretty

# From Claude conversation logs
path p import claude --project /path/to/project --pretty
path p import claude --project /path/to/project --session abc123
path p import claude --project /path/to/project --all
```

### query

Load every step in the local cache into one JSON array and transform it with
an in-process jaq (jq) filter. Each element wraps a Toolpath step with
`cache_id`, `path` (the parent path's `id`/`base`/`meta`), and `dead_end`.
Scope flags choose which documents load; the filter does the rest.

```bash
# Find abandoned branches (the former `dead-ends` subcommand)
path query 'map(select(.dead_end))'

# Steps by an agent actor (the former `filter --actor`)
path query --input doc.json 'map(select(.step.actor | startswith("agent:")))'

# Turns over 50k input tokens, in Claude sessions only
path query --source claude 'map(select(any(.change[].structural.token_usage; .input_tokens > 50000)))'

# Top 10 steps by total tokens
path query --kind agent-coding-session \
  'map({step: .step.id, t: ([.change[].structural.token_usage//empty | (.input_tokens//0)+(.output_tokens//0)] | add//0)}) | sort_by(-.t) | .[:10]'

# Raw output (-r): a column of ids straight into another command
path query -r '.[].cache_id' | sort -u
```

Scope flags: `--source <name>` / `--id <cache-id>` / `--input <file>` (file
selection), `--project <path>` / `--kind <selector>` (content scoping). Output
mirrors jq: pretty on a TTY, compact when piped (`-c` forces compact); `-r`
prints string results unquoted (for piping ids/paths onward, or reading
text/diff content unescaped).

### kind

List the document kinds the binary bundles a spec for, or print a kind's
bundled `schema.json` — the per-field type and semantics reference for writing
`path query` filters.

```bash
path kind                                # list bundled kinds
path kind agent-coding-session           # newest version's schema
path kind agent-coding-session/v1.0.0    # pin a version
```

### p query

Low-level graph traversal on a single document.

```bash
# Walk ancestry from a step
path p query ancestors --input doc.json --step-id step-003
```

### p render

Render documents to other formats.

```bash
# Graphviz DOT output
path p render dot --input doc.json --output graph.dot
path p render dot --input doc.json --show-files --show-timestamps

# Pipe through Graphviz
path p import git --repo . --branch main --no-cache | path p render dot | dot -Tpng -o graph.png
```

### p merge

Combine multiple documents into a single Graph.

```bash
path p merge doc1.json doc2.json --title "Release v2" --pretty
path p merge *.json --pretty
```

### p track

Incrementally build a Path document step by step, useful for editor integrations and live sessions.

```bash
# Start a session (pipe initial content via stdin)
echo "hello" | path p track init --file src/main.rs --actor "human:alex" --title "Editing session"

# Record a step (pipe current content via stdin)
echo "world" | path p track step --session /tmp/session.json --seq 1 --parent-seq 0

# Record a step with VCS source metadata
echo "world" | path p track step --session /tmp/session.json --seq 2 --parent-seq 1 \
  --source '{"type":"git","revision":"abc123"}'

# Add a note to the current step
path p track note --session /tmp/session.json --intent "Refactored for clarity"

# Annotate any step with metadata (intent, source, refs)
path p track annotate --session /tmp/session.json --step step-001 \
  --intent "Extract helper" \
  --source '{"type":"git","revision":"abc123"}' \
  --ref '{"rel":"issue","href":"https://github.com/org/repo/issues/42"}'

# Export the session as a Toolpath Path document
path p track export --session /tmp/session.json --pretty

# Export and clean up
path p track close --session /tmp/session.json --pretty

# List active sessions
path p track list
```

### p validate

Check that a JSON file is a valid Toolpath document.

```bash
path p validate --input examples/step-01-minimal.json
# Valid: Step (id: step-001)
```

### haiku

```bash
path haiku
```

## Global flags

| Flag | Description |
|---|---|
| `--pretty` | Pretty-print JSON output |

## Part of Toolpath

This is the CLI for the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) -- core types and query API
- [`toolpath-git`](https://crates.io/crates/toolpath-git) -- derive from git history
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) -- derive from Claude conversations
- [`toolpath-dot`](https://crates.io/crates/toolpath-dot) -- Graphviz DOT rendering
- [RFC](https://github.com/empathic/toolpath/blob/main/RFC.md) -- full format specification
