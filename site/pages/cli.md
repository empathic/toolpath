---
layout: base.njk
title: CLI
nav: cli
permalink: /cli/
---

# CLI

The `toolpath-cli` crate installs a binary called `path`.

```bash
cargo install toolpath-cli
```

## Commands

```
path
  list
    git       [--repo PATH] [--remote NAME] [--json]
    claude    [--project PATH] [--json]
  derive
    git       --repo PATH --branch NAME[:START] [--base COMMIT] [--remote NAME] [--title TEXT]
    claude    --project PATH [--session ID] [--all]
  query
    ancestors --input FILE --step-id ID
    dead-ends --input FILE
    filter    --input FILE [--actor PREFIX] [--artifact PATH] [--after TIME] [--before TIME]
  render
    dot       [--input FILE] [--output FILE] [--show-files] [--show-timestamps]
  merge       FILE... [--title TEXT]
  track
    init      --file PATH --actor ACTOR [--title TEXT] [--base-uri URI] [--base-ref REF]
    step      --session FILE --seq N --parent-seq N [--actor ACTOR] [--source JSON]
    visit     --session FILE --seq N [--inherit-from N]
    note      --session FILE --intent TEXT
    annotate  --session FILE [--step ID] [--intent TEXT] [--source JSON] [--ref JSON]...
    export    --session FILE
    close     --session FILE [--output FILE]
    list      [--session-dir PATH] [--json]
  validate    --input FILE
  haiku
```

All commands that produce JSON output accept `--pretty` for formatted output.

<svg class="topo topo-wide" viewBox="0 0 900 70" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
  <path d="M0,50 Q150,15 350,45 Q550,70 700,30 Q800,10 900,40" stroke="#b5652b" stroke-width="1" opacity="0.10" fill="none"/>
  <path d="M0,55 Q160,22 360,50 Q560,72 710,36 Q810,16 900,46" stroke="#8a8078" stroke-width="1" opacity="0.07" fill="none"/>
  <path d="M0,60 Q170,30 370,55 Q565,74 715,42 Q815,22 900,52" stroke="#b5652b" stroke-width="1" opacity="0.05" fill="none"/>
</svg>

## Typical workflows

### See what happened in this repo

```bash
path derive git --repo . --branch main --pretty
```

Walks the git history on `main`, converts each commit to a Step, and outputs a Path document.

### Visualize the provenance

```bash
path derive git --repo . --branch main | path render dot | dot -Tpng -o graph.png
```

Pipe the derived document through the DOT renderer, then through Graphviz. Steps are color-coded by actor type, dead ends get red dashed borders.

### Derive from a Claude session

```bash
path derive claude --project /path/to/project --pretty
```

Reads the most recent Claude conversation for that project and maps it to a Toolpath Path.

### Find abandoned approaches

```bash
path query dead-ends --input doc.json --pretty
```

Returns steps that have no descendants leading to the path head. These are the things that were tried and discarded.

### Track changes in real time

```bash
# Start a session (pipe initial content via stdin)
cat src/main.rs | path track init --file src/main.rs --actor human:alex --title "Refactoring auth"

# After each save, record a step (pipe current content via stdin)
cat src/main.rs | path track step --session /tmp/session.json --seq 1 --parent-seq 0
cat src/main.rs | path track step --session /tmp/session.json --seq 2 --parent-seq 1

# Annotate a step with intent or VCS source
path track annotate --session /tmp/session.json --intent "Extract helper"

# Export the finished document
path track export --session /tmp/session.json --pretty
```

The `track` command group records changes to a file over time, building a Path document incrementally. Each `step` captures a diff from the previous state.

### Combine multiple documents

```bash
path merge pr-42.json pr-43.json pr-44.json --title "Release v2" --pretty
```

Merges Path documents into a Graph. Useful for collecting related PRs into a release provenance bundle.

### Validate a document

```bash
path validate --input doc.json
```

Checks that a Toolpath document is structurally valid against the format specification.

### Multi-branch derivation

```bash
path derive git --repo . --branch main --branch feature/auth --pretty
```

When given multiple branches, produces a Graph document with one Path per branch.
