# path

Unified CLI for deriving, querying, and visualizing Toolpath provenance documents.

## Installation

```bash
cargo install --path crates/path
```

Or run directly:

```bash
cargo run -p path -- <command>
```

## Commands

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

### validate

Check that a JSON file is a valid Toolpath document.

```bash
path validate --input examples/step-01-minimal.json
# Valid: Step (id: step-001)
```

## Global flags

| Flag | Description |
|---|---|
| `--pretty` | Pretty-print JSON output |
