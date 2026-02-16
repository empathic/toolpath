# toolpath-cli

Unified CLI for deriving, querying, and visualizing Toolpath provenance documents.

## Installation

```bash
cargo install toolpath-cli
```

This installs a binary called `path`.

Or run from source:

```bash
cargo run -p toolpath-cli -- <command>
```

## Typical workflows

**Capture the provenance of a PR:**

```bash
path derive git --repo . --branch feature --pretty > pr-provenance.json
```

**Visualize how a branch evolved, including dead ends:**

```bash
path derive git --repo . --branch main:HEAD~20 | path render dot | dot -Tpng -o history.png
```

**Review what an AI agent changed:**

```bash
path derive claude --project . --pretty | path query filter --actor "agent:" --pretty
```

**Record provenance for a live editing session:**

```bash
cat src/main.rs | path track init --file src/main.rs --actor "human:alex"
# ... edit the file ...
cat src/main.rs | path track step --session /tmp/session.json --seq 1 --intent "Refactored auth"
path track close --session /tmp/session.json --pretty > session-provenance.json
```

**Merge multiple sources into a release graph:**

```bash
path merge git-provenance.json claude-provenance.json --title "v2.0 Release" --pretty
```

## Commands

### list

Discover available sources before deriving.

```bash
# List git branches with metadata
path list git --repo .

# List Claude projects
path list claude

# List sessions within a project
path list claude --project /path/to/project

# Machine-readable output
path list git --repo . --json
```

### derive

Generate Toolpath documents from source systems.

```bash
# From git history (single branch -> Path, multiple -> Graph)
path derive git --repo . --branch main --pretty
path derive git --repo . --branch main --branch feature --title "Release v2"
path derive git --repo . --branch main:HEAD~20 --pretty

# From Claude conversation logs
path derive claude --project /path/to/project --pretty
path derive claude --project /path/to/project --session abc123
path derive claude --project /path/to/project --all
```

### query

Query Toolpath documents.

```bash
# Walk ancestry from a step
path query ancestors --input doc.json --step-id step-003

# Find abandoned branches
path query dead-ends --input doc.json

# Filter by criteria (combinable)
path query filter --input doc.json --actor "agent:"
path query filter --input doc.json --artifact "src/main.rs"
path query filter --input doc.json --after "2026-01-29T00:00:00Z" --before "2026-01-30T00:00:00Z"
```

### render

Render documents to other formats.

```bash
# Graphviz DOT output
path render dot --input doc.json --output graph.dot
path render dot --input doc.json --show-files --show-timestamps

# Pipe through Graphviz
path derive git --repo . --branch main | path render dot | dot -Tpng -o graph.png
```

### merge

Combine multiple documents into a single Graph.

```bash
path merge doc1.json doc2.json --title "Release v2" --pretty
path merge *.json --pretty
```

### track

Incrementally build a Path document step by step, useful for editor integrations and live sessions.

```bash
# Start a session (pipe initial content via stdin)
echo "hello" | path track init --file src/main.rs --actor "human:alex" --title "Editing session"

# Record a step (pipe current content via stdin)
echo "world" | path track step --session /tmp/session.json --seq 1 --intent "Changed greeting"

# Add a note to the current step
path track note --session /tmp/session.json --intent "Refactored for clarity"

# Export the session as a Toolpath Path document
path track export --session /tmp/session.json --pretty

# Export and clean up
path track close --session /tmp/session.json --pretty

# List active sessions
path track list
```

### validate

Check that a JSON file is a valid Toolpath document.

```bash
path validate --input examples/step-01-minimal.json
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
